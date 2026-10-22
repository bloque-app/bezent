// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The daily statement (`T-004-05`; `REQ-PR-1`, `REQ-PR-2`, `REQ-BE-3`, `REQ-BK-9`, `REQ-CV-4`,
//! `AC-E1.1`, `AC-E1.2`; PLAN `DEC-16`).
//!
//! At least once a day per asset, [`Statements::publish_due`] turns the asset's latest complete
//! evaluation into a JSON document that names:
//!
//! - the asset's backend and assurance class (`REQ-BE-3`), and every published reserve address
//!   (`REQ-BK-5`);
//! - every term of the reserve equation, itemised, each with the final block on its network it
//!   rests on, or the attesting party's report (`REQ-PR-1`, `REQ-BK-2`);
//! - unconverted holdings, itemised per token and network, outside the equation (`REQ-CV-4`);
//! - which terms anyone can recompute from public data and which rest on reports (`REQ-BK-9`);
//! - that it is a signed claim, not a proof of solvency (`REQ-PR-1`), and that the book behind it
//!   is not provable to third parties (`REQ-BK-9`);
//! - the SHA-256 of the asset's previous statement, so a revision of any earlier one is detectable
//!   from a later one without bezent's cooperation (`REQ-PR-2`, `AC-E1.2`).
//!
//! The exact bytes are signed with the statement key, ed25519, used for nothing else
//! ([`StatementSigner`]; the signer's policy refuses to bind an ed25519 key to any network), and
//! both the document and its signature go to an append-only [`StatementStore`] that refuses to
//! overwrite a name: the object-locked bucket of `DEC-16` in deployment (`F-014`), a directory in
//! tests.
//!
//! The core holds no key (`REQ-AR-3`, `REQ-CU-1`): in deployment the [`StatementSigner`] is
//! [`SignerProcess`], the signer process's statement key, reached through a recorded intent of
//! purpose `statement` (PLAN `DEC-16`, 2026-10-14; issue #42): the core records the intent with the
//! statement's exact bytes, asks the signer over its link, verifies the returned signature against
//! the recorded bytes and the statement key the core is configured with, and only then publishes.
//! Every signature is verified again before anything reaches the store. [`KeyBackendSigner`] signs
//! with a key custody directly, in the calling process: it is for tests, with a test file key, and
//! is never wired into a deployment.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;

use bezent_signer::keys::{KeyBackend, KeyError, Scheme};
use bezent_signer::link::{Client, Response};
use bezent_signer::verify::verifies_exactly;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::check::{LogPager, Page, Pager};
use super::{Asset, book};

/// How often each asset's statement is published: daily (`REQ-PR-1`).
pub fn period() -> Duration {
    Duration::days(1)
}

/// How long past due a statement may be before a human is paged (PR #50 review M2): a statement
/// is attempted every cycle, so an hour is many failed attempts.
pub fn overdue_grace() -> Duration {
    Duration::hours(1)
}

/// How often a stuck statement chain is paged again (PR #50 re-check S-3): once per statement
/// cycle, the [`period`], for the same asset and reason; not on every core cycle.
pub fn stuck_page_window() -> Duration {
    period()
}

/// The oldest evaluation a statement states: the check runs every minute (`NFR-5`), so an older
/// one means it is not running, and the statement waits rather than state stale terms (review S7).
pub fn max_evaluation_age() -> Duration {
    Duration::minutes(15)
}

/// What every statement says of itself (`REQ-PR-1`).
pub const CLAIM: &str = "This statement is a signed claim by the operator of bezent about the \
    reserve and supply of this bridged asset. It is not a proof of solvency.";

/// What every statement says of the book behind it (`REQ-BK-9`).
pub const BOOK: &str = "The reserve book these figures are compared with is bezent's working \
    state and is not provable to third parties. Each term below says whether anyone can \
    recompute it from public data or whether it rests on an attesting party's reports.";

/// What every statement says of unconverted holdings (`REQ-CV-4`).
pub const HOLDINGS: &str = "Unconverted holdings are listed outside the reserve equation. They \
    back nothing until converted into the origin token.";

/// The key that signs statements and nothing else (`REQ-PR-1`).
pub trait StatementSigner: Send + Sync {
    /// Its ed25519 public key, 32 bytes.
    fn public_key(&self) -> Result<Vec<u8>, KeyError>;

    /// Signs `message`: 64 bytes.
    fn sign(&self, message: &[u8]) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send;
}

/// The statement key in a key custody (`DEC-11`): an ed25519 key, refused otherwise.
pub struct KeyBackendSigner<B> {
    backend: B,
    key: String,
}

impl<B: KeyBackend> KeyBackendSigner<B> {
    /// The statement key `key` of `backend`. Only an ed25519 key is accepted.
    pub fn new(backend: B, key: &str) -> Result<Self, KeyError> {
        if backend.scheme(key)? != Scheme::Ed25519 {
            return Err(KeyError::Refused(format!(
                "key {key} is not an ed25519 key; the statement key is ed25519 and signs nothing else (REQ-PR-1)"
            )));
        }
        Ok(Self {
            backend,
            key: key.to_owned(),
        })
    }
}

impl<B: KeyBackend> StatementSigner for KeyBackendSigner<B> {
    fn public_key(&self) -> Result<Vec<u8>, KeyError> {
        self.backend.public_key(&self.key)
    }

    fn sign(&self, message: &[u8]) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        self.backend.sign(&self.key, message)
    }
}

/// The statement key behind the signer process (PLAN `DEC-16`, 2026-10-14; issue #42): each
/// statement is signed through a recorded intent of purpose `statement`, and the core holds only the
/// key's public half, from its configuration.
pub struct SignerProcess {
    pool: PgPool,
    socket: PathBuf,
    uid: u32,
    token: String,
    public_key: Vec<u8>,
}

impl fmt::Debug for SignerProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignerProcess")
            .field("socket", &self.socket)
            .field("uid", &self.uid)
            .field("public_key", &hex0x(&self.public_key))
            .finish_non_exhaustive()
    }
}

impl SignerProcess {
    /// The signer at `socket`, served by user `uid`, reached with `token`, holding the ed25519
    /// statement key whose public key is `public_key`; intents are recorded through `pool` (the
    /// core's login).
    pub fn new(
        pool: PgPool,
        socket: PathBuf,
        uid: u32,
        token: String,
        public_key: Vec<u8>,
    ) -> Result<Self, KeyError> {
        let valid = <[u8; 32]>::try_from(public_key.as_slice())
            .ok()
            .is_some_and(|k| ed25519_dalek::VerifyingKey::from_bytes(&k).is_ok());
        if !valid {
            return Err(KeyError::Refused(
                "the statement key's public key is not an ed25519 public key (REQ-PR-1)".into(),
            ));
        }
        Ok(Self {
            pool,
            socket,
            uid,
            token,
            public_key,
        })
    }

    /// The statement intent for `message`: the one already recorded for these exact bytes, or a
    /// new one on the issuing network (migration 0039 holds its shape).
    async fn intent(&self, message: &[u8]) -> Result<i64, KeyError> {
        let doc: Value = serde_json::from_slice(message).map_err(|_| {
            KeyError::Refused("the statement key signs statements only (REQ-PR-1)".into())
        })?;
        let (Some(asset), Some(sequence)) = (doc["asset"].as_str(), doc["sequence"].as_i64())
        else {
            return Err(KeyError::Refused(
                "the statement key signs statements only (REQ-PR-1)".into(),
            ));
        };
        let sha256 = hex0x(&Sha256::digest(message));
        let store = |e: sqlx::Error| KeyError::Backend(format!("store: {e}"));
        let recorded: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM intents WHERE purpose = 'statement' \
               AND payload -> 'statement' ->> 'sha256' = $1 ORDER BY id LIMIT 1",
        )
        .bind(&sha256)
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?;
        if let Some(id) = recorded {
            return Ok(id);
        }
        let payload = json!({
            "message": hex0x(message),
            "amount": "0",
            "statement": {"asset": asset, "sequence": sequence, "sha256": sha256},
        });
        sqlx::query_scalar(
            "INSERT INTO intents (network, purpose, payload) \
             SELECT n.network, 'statement', $1::text::jsonb FROM networks n \
             WHERE n.role = 'issuing' RETURNING id",
        )
        .bind(payload.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(store)?
        .ok_or_else(|| KeyError::Backend("no issuing network is declared".into()))
    }
}

impl StatementSigner for SignerProcess {
    fn public_key(&self) -> Result<Vec<u8>, KeyError> {
        Ok(self.public_key.clone())
    }

    async fn sign(&self, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        let intent = self.intent(message).await?;
        let mut client = Client::connect(&self.socket, &self.token, self.uid)
            .await
            .map_err(|e| KeyError::Backend(e.to_string()))?;
        // `Client::sign` checks the signature against the intent's recorded bytes, and starts the
        // compromise path on one over anything else (INV-9).
        let answer = client
            .sign(&self.pool, intent)
            .await
            .map_err(|e| KeyError::Backend(e.to_string()))?;
        match answer {
            Response::Signed {
                scheme,
                public_key,
                signature,
                ..
            } => {
                let signature = hex::decode(signature.strip_prefix("0x").unwrap_or(&signature))
                    .map_err(|_| KeyError::Backend("the signature is not hex".into()))?;
                if scheme != "ed25519" || public_key != hex0x(&self.public_key) {
                    return Err(KeyError::Refused(format!(
                        "intent {intent} was signed by {public_key} ({scheme}), not the statement \
                         key {} (REQ-PR-1)",
                        hex0x(&self.public_key)
                    )));
                }
                if !verifies_exactly("ed25519", &self.public_key, message, &signature) {
                    return Err(KeyError::Refused(format!(
                        "the signature of intent {intent} does not verify (INV-9)"
                    )));
                }
                Ok(signature)
            }
            Response::Refused { spec, reason, .. } => Err(KeyError::Refused(format!(
                "intent {intent}: {spec}: {reason}"
            ))),
            Response::Unavailable { reason, .. } => {
                Err(KeyError::Backend(format!("intent {intent}: {reason}")))
            }
            other => Err(KeyError::Backend(format!(
                "intent {intent}: unexpected answer {other:?}"
            ))),
        }
    }
}

/// Signs `bytes` with `signer` and verifies the signature with `public_key` before anything is
/// published (issue #42, `INV-9`): a signature that does not verify publishes nothing.
async fn signed<S: StatementSigner>(
    signer: &S,
    public_key: &[u8],
    bytes: &[u8],
) -> Result<Vec<u8>, StatementError> {
    let signature = signer.sign(bytes).await.map_err(StatementError::Key)?;
    if !verifies_exactly("ed25519", public_key, bytes, &signature) {
        return Err(StatementError::Key(KeyError::Refused(
            "the statement's signature does not verify with the statement key; nothing is \
             published (REQ-PR-1, INV-9)"
                .into(),
        )));
    }
    Ok(signature)
}

/// Why the store refused or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// The name is taken: a statement is never overwritten (`REQ-PR-2`).
    Exists(String),
    /// The store could not be reached or written.
    Io(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exists(name) => {
                write!(f, "{name} exists; a statement is never revised (REQ-PR-2)")
            }
            Self::Io(e) => write!(f, "statement store: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// What a store answered a write with: where the object is and the bucket's receipt for it. The
/// core records it after the write succeeded (migration 0053; issue #58).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Receipt {
    /// Where the object was written: the bucket key, or the file name for a directory.
    pub location: String,
    /// The ETag, if the store has one.
    pub etag: Option<String>,
    /// The version id, if the store is versioned.
    pub version_id: Option<String>,
}

impl Receipt {
    /// The receipt of a store that answers with a location only.
    pub fn local(name: &str) -> Self {
        Self {
            location: name.to_owned(),
            ..Self::default()
        }
    }
}

/// An object read from a store, with its receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// Where it is, and its ETag and version.
    pub receipt: Receipt,
}

/// An append-only store of statements (`REQ-PR-2`; `DEC-16`'s bucket with object lock).
///
/// The core calls [`put_at`](Self::put_at) and [`get_at`](Self::get_at), naming the statement's
/// issue time, because the bucket keys a statement by its date. A store that does not need it
/// implements [`put`](Self::put) and [`get`](Self::get) and takes the defaults.
pub trait StatementStore: Send + Sync {
    /// Writes `bytes` under `name`; refuses a name that exists.
    fn put(&self, name: &str, bytes: &[u8]) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// Reads `name`.
    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send;

    /// Writes `bytes` under `name`, for a statement issued at `issued_at`; refuses a name that
    /// exists, unless it holds exactly these bytes already. Returns the store's receipt.
    fn put_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
        bytes: &[u8],
    ) -> impl Future<Output = Result<Receipt, StoreError>> + Send {
        let _ = issued_at;
        async move {
            self.put(name, bytes).await?;
            Ok(Receipt::local(name))
        }
    }

    /// Reads `name`, for a statement issued at `issued_at`.
    fn get_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<Option<Stored>, StoreError>> + Send {
        let _ = issued_at;
        async move {
            Ok(self.get(name).await?.map(|bytes| Stored {
                bytes,
                receipt: Receipt::local(name),
            }))
        }
    }
}

/// A local directory as the store: files are created once and never opened for writing again.
#[derive(Clone, Debug)]
pub struct DirectoryStore {
    root: PathBuf,
}

impl DirectoryStore {
    /// The store rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path(&self, name: &str) -> Result<PathBuf, StoreError> {
        if name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(StoreError::Io(format!(
                "{name} is not a plain relative name"
            )));
        }
        Ok(self.root.join(name))
    }
}

impl StatementStore for DirectoryStore {
    fn put(&self, name: &str, bytes: &[u8]) -> impl Future<Output = Result<(), StoreError>> + Send {
        let result = self.path(name).and_then(|path| {
            use std::io::Write;
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| StoreError::Io(e.to_string()))?;
            }
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => StoreError::Exists(name.to_owned()),
                    _ => StoreError::Io(e.to_string()),
                })?;
            file.write_all(bytes)
                .and_then(|()| file.sync_all())
                .map_err(|e| StoreError::Io(e.to_string()))
        });
        std::future::ready(result)
    }

    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        let result = self.path(name).and_then(|path| match std::fs::read(path) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Io(e.to_string())),
        });
        std::future::ready(result)
    }
}

/// Why a statement was not published.
#[derive(Debug)]
pub enum StatementError {
    /// No complete evaluation of the asset exists to state.
    NoEvaluation(String),
    /// The key refused or failed.
    Key(KeyError),
    /// The store refused or failed.
    Store(StoreError),
    /// The database failed.
    Db(sqlx::Error),
    /// The store holds the statement's name from an earlier attempt that did not finish, and it is
    /// not one bezent can adopt (review S7).
    Stuck(String),
}

impl fmt::Display for StatementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoEvaluation(a) => write!(f, "no complete evaluation of {a} to state"),
            Self::Key(e) => write!(f, "statement key: {e}"),
            Self::Store(e) => write!(f, "{e}"),
            Self::Db(e) => write!(f, "store: {e}"),
            Self::Stuck(why) => write!(f, "statement chain stuck: {why}"),
        }
    }
}

impl std::error::Error for StatementError {}

impl From<sqlx::Error> for StatementError {
    fn from(e: sqlx::Error) -> Self {
        Self::Db(e)
    }
}

/// A statement as published.
#[derive(Clone, Debug)]
pub struct Published {
    /// The asset.
    pub asset: String,
    /// Its place in the asset's chain, from 1.
    pub sequence: i64,
    /// The document's name in the store.
    pub name: String,
    /// The signature's name in the store.
    pub signature_name: String,
    /// The document's exact bytes.
    pub bytes: Vec<u8>,
    /// Their SHA-256.
    pub sha256: [u8; 32],
    /// The store's receipt for the document (issue #58).
    pub receipt: Receipt,
    /// The store's receipt for the signature file.
    pub signature_receipt: Receipt,
}

/// The statement publisher over the core's pool, the statement key and the store.
pub struct Statements<S, T> {
    pool: PgPool,
    signer: S,
    store: T,
    pager: Box<dyn Pager>,
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

impl<S: StatementSigner, T: StatementStore> Statements<S, T> {
    /// The publisher. A chain that is stuck pages through the service's log until
    /// [`Statements::with_pager`] names another pager.
    pub fn new(pool: PgPool, signer: S, store: T) -> Self {
        Self {
            pool,
            signer,
            store,
            pager: Box::new(LogPager),
        }
    }

    /// The publisher, paging `pager` when the chain is stuck (review SF4).
    #[must_use]
    pub fn with_pager(mut self, pager: impl Pager + 'static) -> Self {
        self.pager = Box::new(pager);
        self
    }

    /// The store.
    pub fn store(&self) -> &T {
        &self.store
    }

    /// Publishes the statement of every asset whose last one is a day old or more at `now`
    /// (`REQ-PR-1`). An asset with no recent complete evaluation is skipped and logged; one that
    /// fails is logged and the others are still published (review S7). Only a store that cannot
    /// list the assets is an error.
    pub async fn publish_due(&self, now: DateTime<Utc>) -> Result<Vec<Published>, StatementError> {
        let assets: Vec<(String, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT a.asset, (SELECT max(s.issued_at) FROM reserve_statements s \
                              WHERE s.asset = a.asset) \
             FROM assets a ORDER BY a.asset",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::new();
        for (asset, last) in assets {
            if last.is_some_and(|l| now - l < period()) {
                continue;
            }
            match self.publish(&asset, now).await {
                Ok(p) => out.push(p),
                Err(StatementError::NoEvaluation(a)) => {
                    tracing::warn!(asset = %a, "REQ-PR-1: no recent complete evaluation to state yet");
                }
                Err(e) => {
                    tracing::error!(asset = %asset, error = %e, "REQ-PR-1: statement not published; tried again next time");
                }
            }
        }
        self.page_overdue(now).await?;
        Ok(out)
    }

    /// Pages `statement-overdue` for every asset whose statement fell due more than
    /// [`overdue_grace`] before `now` and was not produced, once per due time (`REQ-PR-1`; PR #50
    /// review M2): a store or a signer that keeps failing, or a check with nothing complete to
    /// state, never leaves the statement missing unheard. A statement falls due a [`period`]
    /// after the asset's last one, or, before its first, when the asset was first evaluated.
    /// Returns the pages.
    pub async fn page_overdue(&self, now: DateTime<Utc>) -> Result<Vec<Page>, StatementError> {
        #[allow(clippy::type_complexity)]
        let assets: Vec<(String, Option<DateTime<Utc>>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "SELECT a.asset, \
                    (SELECT max(s.issued_at) FROM reserve_statements s WHERE s.asset = a.asset), \
                    (SELECT min(v.evaluated_at) FROM reserve_evaluations v \
                      WHERE v.asset = a.asset) \
                 FROM assets a ORDER BY a.asset",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut pages = Vec::new();
        for (asset, last, first) in assets {
            let Some(due) = last.map(|l| l + period()).or(first) else {
                continue;
            };
            if now - due <= overdue_grace() {
                continue;
            }
            let due_at = due.to_rfc3339();
            let mut conn = self.pool.acquire().await?;
            let paged: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = 'statement-overdue' \
                   AND payload ->> 'asset' = $1 AND payload ->> 'due_at' = $2)",
            )
            .bind(&asset)
            .bind(&due_at)
            .fetch_one(&mut *conn)
            .await?;
            if paged {
                continue;
            }
            let page = Page {
                kind: "statement-overdue".into(),
                asset: asset.clone(),
                detail: json!({
                    "spec": "REQ-PR-1, REQ-PR-2",
                    "asset": asset,
                    "due_at": due_at,
                    "last_issued_at": last.map(|l| l.to_rfc3339()),
                    "grace_seconds": overdue_grace().num_seconds(),
                    "action": "the asset's statement is due and was not published: check the statement store, the signer and the reserve check",
                }),
            };
            crate::store::event(&mut conn, &page.kind, None, &page.detail).await?;
            self.pager.page(&page);
            pages.push(page);
        }
        Ok(pages)
    }

    /// Composes, signs and publishes `asset`'s next statement at `now`, from its latest complete
    /// evaluation no older than [`max_evaluation_age`]. If an earlier attempt left the statement's
    /// name in the store and did not record it, that statement is adopted, never replaced, so a
    /// publish cut short never blocks the chain (`REQ-PR-2`; review S7); but only a statement this
    /// key already signed, or one whose bytes are exactly what bezent re-derives from the
    /// evaluation it names (review SF4). Anything else is [`StatementError::Stuck`], and a human is
    /// paged, once per [`stuck_page_window`] (PR #50 re-check S-3).
    ///
    /// A statement the key signed for the place that never reached the store, and that cannot be
    /// stored as it is (it no longer re-derives, or another key signed it), is abandoned: the place
    /// is recorded with the hash of what was signed, why and by whom, a human is told once, and the
    /// next place's statement is published naming it (PR #50 re-check S-1, S-3). Nothing is stored
    /// twice for a place, nothing is signed twice for it, and no key rotation is needed.
    pub async fn publish(
        &self,
        asset: &str,
        now: DateTime<Utc>,
    ) -> Result<Published, StatementError> {
        // Each abandonment moves the chain one place on; a second in a row is already unusual.
        const ROUNDS: usize = 3;
        let mut published = Err(StatementError::Stuck(format!(
            "{asset}: {ROUNDS} places abandoned in a row; tried again next time"
        )));
        for _ in 0..ROUNDS {
            match self.publish_once(asset, now).await {
                Ok(Step::Abandoned) => {}
                Ok(Step::Published(p)) => return Ok(*p),
                Err(e) => {
                    published = Err(e);
                    break;
                }
            }
        }
        match &published {
            Err(StatementError::Stuck(why)) => self.page_stuck(asset, why, now).await,
            // The store could not take the statement (the bucket refused or was unreachable after
            // every retry): a condition to page on at once, never a silent skip (issue #58).
            Err(StatementError::Store(e)) => self.page_store_failure(asset, e, now).await,
            _ => {}
        }
        published
    }

    /// Pages `statement-stuck` for `asset`, unless the same was paged within the last
    /// [`stuck_page_window`] (PR #50 re-check S-3: not on every cycle).
    async fn page_stuck(&self, asset: &str, why: &str, now: DateTime<Utc>) {
        let page = Page {
            kind: "statement-stuck".into(),
            asset: asset.to_owned(),
            detail: json!({
                "spec": "REQ-PR-2, REQ-IC-2",
                "asset": asset,
                "why": why,
                "paged_at": now.to_rfc3339(),
                "repeats_after_seconds": stuck_page_window().num_seconds(),
                "action": "the store holds the next statement's name with a document bezent did not sign and cannot re-derive; nothing was signed",
            }),
        };
        self.page_once(page, asset, why, now).await;
    }

    /// Pages `statement-publish-failed` for `asset`, once per [`stuck_page_window`] (issue #58):
    /// the statement store refused or failed to take a signed statement after every retry. The
    /// error is in the page; the dedupe key is the fixed reason, so a changing error text does not
    /// page every cycle.
    async fn page_store_failure(&self, asset: &str, error: &StoreError, now: DateTime<Utc>) {
        let why = "the statement store did not take the statement";
        let page = Page {
            kind: "statement-publish-failed".into(),
            asset: asset.to_owned(),
            detail: json!({
                "spec": "REQ-PR-1, REQ-PR-2",
                "asset": asset,
                "why": why,
                "error": error.to_string(),
                "refused_to_overwrite": matches!(error, StoreError::Exists(_)),
                "paged_at": now.to_rfc3339(),
                "repeats_after_seconds": stuck_page_window().num_seconds(),
                "action": "the signed statement is kept and written again every cycle; check the bucket (existence, object lock, versioning), the task role's policy and the KMS key",
            }),
        };
        self.page_once(page, asset, why, now).await;
    }

    /// Logs and pages `page` unless one of its kind with the same asset and `why` was paged within
    /// the last [`stuck_page_window`] (PR #50 re-check S-3: not on every cycle).
    async fn page_once(&self, page: Page, asset: &str, why: &str, now: DateTime<Utc>) {
        let logged = async {
            let mut conn = self.pool.acquire().await?;
            let paged: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM event_log WHERE kind = $4 \
                   AND payload ->> 'asset' = $1 AND payload ->> 'why' = $2 \
                   AND (payload ->> 'paged_at')::timestamptz > $3)",
            )
            .bind(asset)
            .bind(why)
            .bind(now - stuck_page_window())
            .bind(&page.kind)
            .fetch_one(&mut *conn)
            .await?;
            if !paged {
                crate::store::event(&mut conn, &page.kind, None, &page.detail).await?;
            }
            Ok::<bool, sqlx::Error>(!paged)
        };
        match logged.await {
            Ok(false) => {}
            Ok(true) => self.pager.page(&page),
            Err(e) => {
                tracing::error!(asset, error = %e, "REQ-PR-2: a statement page could not be logged");
                self.pager.page(&page);
            }
        }
    }

    async fn publish_once(&self, asset: &str, now: DateTime<Utc>) -> Result<Step, StatementError> {
        let mut tx = self.pool.begin().await?;
        // One publisher at a time per asset: the chain has no fork (REQ-PR-2).
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("bezent/statement/{asset}"))
            .execute(&mut *tx)
            .await?;
        let Some(a) = Asset::load(&mut tx, asset).await? else {
            return Err(StatementError::NoEvaluation(asset.to_owned()));
        };
        let evaluation: Option<Stated> = sqlx::query_as(&format!(
            "SELECT {STATED} FROM reserve_evaluations \
             WHERE asset = $1 AND outcome <> 'incomplete' AND evaluated_at >= $2 \
             ORDER BY evaluated_at DESC, id DESC LIMIT 1"
        ))
        .bind(asset)
        .bind(now - max_evaluation_age())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(evaluation) = evaluation else {
            return Err(StatementError::NoEvaluation(asset.to_owned()));
        };
        let previous: Option<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT sequence, sha256 FROM reserve_statements WHERE asset = $1 \
             ORDER BY sequence DESC LIMIT 1",
        )
        .bind(asset)
        .fetch_optional(&mut *tx)
        .await?;
        // The places abandoned since the last published statement: the next one names each
        // (PR #50 re-check S-1, S-3).
        let abandoned: Vec<(i64, Vec<u8>, Vec<u8>, String)> = sqlx::query_as(
            "SELECT sequence, sha256, public_key, reason FROM reserve_statement_abandoned \
             WHERE asset = $1 AND sequence > $2 ORDER BY sequence",
        )
        .bind(asset)
        .bind(previous.as_ref().map_or(0, |(s, _)| *s))
        .fetch_all(&mut *tx)
        .await?;
        let sequence = 1 + abandoned
            .last()
            .map(|(s, ..)| *s)
            .or(previous.as_ref().map(|(s, _)| *s))
            .unwrap_or(0);
        let abandoned: Vec<Value> = abandoned
            .iter()
            .map(|(s, h, k, reason)| {
                json!({"sequence": s, "sha256": hex0x(h), "signing_key": hex0x(k), "reason": reason})
            })
            .collect();
        let public_key = self.signer.public_key().map_err(StatementError::Key)?;
        let name = format!("{asset}/{sequence:010}.json");
        let previous = previous.as_ref().map(|(_, h)| h.as_slice());
        let next = Next {
            a: &a,
            sequence,
            previous,
            abandoned: &abandoned,
            public_key: &public_key,
            name: name.clone(),
        };
        // A statement the key signed for this place and that never reached the store (a publish
        // cut short after signing) is this place's statement: it is published as signed, never
        // replaced by another, so one statement is signed per place, whatever the key (REQ-PR-2;
        // issue #42). Nothing reaches the append-only store before it is vetted (PR #50 review
        // M3): its signature verifies with the current statement key, and its bytes are exactly
        // what bezent re-derives from the book at the evaluation and issue time it states.
        let signed_before: Vec<SignedBefore> = sqlx::query_as(
            "SELECT i.id, i.payload ->> 'message', i.payload -> 'statement' ->> 'sha256', \
                    g.signature, g.public_key FROM intents i \
             JOIN signatures g ON g.intent_id = i.id \
             WHERE i.purpose = 'statement' AND g.scheme = 'ed25519' \
               AND i.payload -> 'statement' ->> 'asset' = $1 \
               AND i.payload -> 'statement' ->> 'sequence' = $2 \
             ORDER BY i.id",
        )
        .bind(asset)
        .bind(sequence.to_string())
        .fetch_all(&mut *tx)
        .await?;
        let candidate = signed_before
            .iter()
            .find(|(.., k)| k.as_slice() == public_key.as_slice())
            .or(signed_before.first());
        // The bucket keys a statement by the date it was issued: that of the statement this key
        // already signed for the place, if one was, never the date of this attempt (issue #58).
        let issued_for_place = candidate
            .and_then(|(_, message, ..)| issued_at_of(message))
            .unwrap_or(now);
        if let Some(stored) = self
            .store
            .get_at(&name, issued_for_place)
            .await
            .map_err(StatementError::Store)?
        {
            return self
                .adopt(tx, next, stored)
                .await
                .map(|p| Step::Published(Box::new(p)));
        }
        if let Some((intent, message, sha256, signature, key)) = candidate {
            let why = if key.as_slice() == public_key.as_slice() {
                match self.storable(&mut tx, &next, message, signature).await {
                    Ok((bytes, evaluation, issued_at)) => {
                        return self
                            .store_and_record(
                                tx,
                                next,
                                bytes,
                                signature.clone(),
                                evaluation,
                                issued_at,
                            )
                            .await
                            .map(|p| Step::Published(Box::new(p)));
                    }
                    Err(StatementError::Stuck(why)) => why,
                    Err(e) => return Err(e),
                }
            } else {
                format!(
                    "{name}: signed under {}, which is not the statement key {}",
                    hex0x(key),
                    hex0x(&public_key)
                )
            };
            // It cannot be stored as it is: the place is abandoned, and the next one names it
            // (PR #50 re-check S-1, S-3). Nothing is signed again for this place.
            let abandonment = Abandonment {
                intent: *intent,
                sha256,
                public_key: key,
                reason: &why,
            };
            self.abandon(tx, &next, abandonment, now).await?;
            return Ok(Step::Abandoned);
        }
        let bytes = document(&mut tx, &next, now, &evaluation).await?;
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        let signature = signed(&self.signer, &public_key, &bytes).await?;
        let signature_name = format!("{name}.sig");
        let signature_file = signature_file(&name, &sha256, &public_key, &signature);
        // The row first, inside the open transaction: the store's guards refuse a statement that
        // does not extend the chain before anything reaches an append-only bucket.
        sqlx::query(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, previous_sha256, public_key, signature) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(asset)
        .bind(sequence)
        .bind(evaluation.0)
        .bind(now)
        .bind(&name)
        .bind(sha256.as_slice())
        .bind(previous)
        .bind(&public_key)
        .bind(&signature)
        .execute(&mut *tx)
        .await?;
        let receipt = self
            .store
            .put_at(&name, now, &bytes)
            .await
            .map_err(StatementError::Store)?;
        let signature_receipt = self
            .store
            .put_at(&signature_name, now, &signature_file)
            .await
            .map_err(StatementError::Store)?;
        // Recorded only now that both writes succeeded (issue #58).
        record_objects(
            &mut tx,
            asset,
            sequence,
            &[
                (&name, &receipt, &bytes),
                (&signature_name, &signature_receipt, &signature_file),
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(Step::Published(Box::new(Published {
            asset: asset.to_owned(),
            sequence,
            name,
            signature_name,
            bytes,
            sha256,
            receipt,
            signature_receipt,
        })))
    }

    /// Whether `message` (hex), signed by the current key with `signature` for `next`'s place and
    /// never stored, may be stored as it is: it is this place's statement, its bytes are exactly
    /// what bezent re-derives, and the signature verifies. Returns the bytes, and the evaluation
    /// and issue time they state; [`StatementError::Stuck`] says why not.
    async fn storable(
        &self,
        tx: &mut sqlx::PgConnection,
        next: &Next<'_>,
        message: &str,
        signature: &[u8],
    ) -> Result<(Vec<u8>, i64, DateTime<Utc>), StatementError> {
        let name = &next.name;
        let bytes = hex::decode(message.strip_prefix("0x").unwrap_or(message))
            .map_err(|_| StatementError::Stuck(format!("{name}: the signed intent's bytes")))?;
        let (evaluation, issued_at, derived) = self.vet(tx, next, &bytes).await?;
        if derived != bytes {
            return Err(StatementError::Stuck(format!(
                "{name}: the signed statement is not the one bezent re-derives from \
                 evaluation {evaluation}; nothing was stored"
            )));
        }
        if !verifies_exactly("ed25519", next.public_key, &bytes, signature) {
            return Err(StatementError::Stuck(format!(
                "{name}: the recorded signature does not verify with the statement key; \
                 nothing was stored"
            )));
        }
        Ok((bytes, evaluation, issued_at))
    }

    /// Abandons `next`'s place (PR #50 re-check S-1, S-3): records it with the hash and key of
    /// what was signed for it, the intent and the reason (the store records who), logs it, commits,
    /// and tells a human once. The next place's statement names it.
    async fn abandon(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        next: &Next<'_>,
        abandonment: Abandonment<'_>,
        now: DateTime<Utc>,
    ) -> Result<(), StatementError> {
        let Abandonment {
            intent,
            sha256,
            public_key,
            reason,
        } = abandonment;
        let hash = hex::decode(sha256.strip_prefix("0x").unwrap_or(sha256)).map_err(|_| {
            StatementError::Stuck(format!("{}: the signed intent's hash", next.name))
        })?;
        let actor: String = sqlx::query_scalar(
            "INSERT INTO reserve_statement_abandoned (asset, sequence, sha256, public_key, \
                 intent_id, reason, actor) \
             VALUES ($1, $2, $3, $4, $5, $6, current_user) RETURNING actor",
        )
        .bind(&next.a.asset)
        .bind(next.sequence)
        .bind(&hash)
        .bind(public_key)
        .bind(intent)
        .bind(reason)
        .fetch_one(&mut *tx)
        .await?;
        let page = Page {
            kind: "statement-abandoned".into(),
            asset: next.a.asset.clone(),
            detail: json!({
                "spec": "REQ-PR-2",
                "asset": next.a.asset,
                "sequence": next.sequence,
                "sha256": sha256,
                "signing_key": hex0x(public_key),
                "intent": intent,
                "reason": reason,
                "actor": actor,
                "abandoned_at": now.to_rfc3339(),
                "action": "the statement signed for this place was never published and cannot be as it is; the next statement names it as abandoned. Check why the publish was cut short",
            }),
        };
        crate::store::event(&mut tx, &page.kind, None, &page.detail).await?;
        tx.commit().await?;
        tracing::warn!(
            asset = %next.a.asset,
            sequence = next.sequence,
            reason,
            "REQ-PR-2: a signed statement that never reached the store was abandoned; the next place names it"
        );
        self.pager.page(&page);
        Ok(())
    }
}

/// A statement intent signed for a place: its id, bytes (hex), SHA-256 (as it names it), the
/// signature and the key that made it.
type SignedBefore = (i64, String, String, Vec<u8>, Vec<u8>);

/// One step of [`Statements::publish`].
enum Step {
    /// The place's statement was published.
    Published(Box<Published>),
    /// The place was abandoned; the next one is due at once.
    Abandoned,
}

/// What is recorded of an abandoned place: the intent its statement was signed through, that
/// statement's SHA-256 (as the intent names it), the key that signed it, and why it is abandoned.
struct Abandonment<'a> {
    intent: i64,
    sha256: &'a str,
    public_key: &'a [u8],
    reason: &'a str,
}

/// The columns of an evaluation a statement states.
const STATED: &str =
    "id, evaluated_at, outcome, points, chain, book, book_entry_cut, book_snapshot::text";

/// An evaluation as a statement states it: id, time, outcome, points, the networks' terms, the
/// book's, the last book entry it read and the snapshot it read the book under.
type Stated = (
    i64,
    DateTime<Utc>,
    String,
    Value,
    Value,
    Value,
    Option<i64>,
    Option<String>,
);

/// The place a statement takes in its asset's chain.
struct Next<'a> {
    a: &'a Asset,
    sequence: i64,
    /// The SHA-256 of the last published statement, if any.
    previous: Option<&'a [u8]>,
    /// The places abandoned since then, as the statement names them (PR #50 re-check S-1, S-3).
    abandoned: &'a [Value],
    public_key: &'a [u8],
    name: String,
}

impl Next<'_> {
    /// Whether `doc` claims this place: this asset, sequence, predecessor and abandoned places.
    fn claimed_by(&self, doc: &Value) -> bool {
        doc["asset"] == self.a.asset.as_str()
            && doc["sequence"] == self.sequence
            && doc["previous_sha256"] == json!(self.previous.map(hex0x))
            && doc.get("abandoned").cloned().unwrap_or_else(|| json!([])) == json!(self.abandoned)
    }
}

/// The exact bytes of the statement at `next`'s place, issued at `issued_at`, stating
/// `evaluation`: the same inputs give the same bytes, so a statement left in the store can be
/// re-derived.
async fn document(
    conn: &mut sqlx::PgConnection,
    next: &Next<'_>,
    issued_at: DateTime<Utc>,
    (evaluation, evaluated_at, outcome, points, chain, book, entry_cut, snapshot): &Stated,
) -> sqlx::Result<Vec<u8>> {
    let a = next.a;
    let cuts = points
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(n, p)| Some((n.clone(), p["block_number"].as_i64()?)))
                .collect()
        })
        .unwrap_or_default();
    // Cut at the evaluation, by commit order (review SF5; issue #42): the movements add up to the
    // book's surplus it states.
    let cut = entry_cut.map(|id| book::EntryCut {
        id,
        snapshot: snapshot.clone(),
    });
    let movements = book::surplus_movements(conn, &a.asset, &cuts, cut.as_ref()).await?;
    let mut document = compose(
        a,
        next.sequence,
        next.previous,
        issued_at,
        next.public_key,
        (*evaluation, *evaluated_at, outcome),
        points,
        chain,
        book,
        &movements,
    );
    // The places abandoned since the previous statement, each with the hash of what was signed
    // for it and never published (REQ-PR-2; PR #50 re-check S-1, S-3). Named only when there are
    // any, so a chain without them reads as it always has.
    if !next.abandoned.is_empty() {
        document["abandoned"] = json!(next.abandoned);
    }
    Ok(serde_json::to_vec_pretty(&document).unwrap_or_default())
}

/// The statement at `next`'s place, re-derived from evaluation `evaluation` at `issued_at`: the
/// book as that evaluation cut it, so the same inputs give the same bytes.
async fn rederive(
    conn: &mut sqlx::PgConnection,
    next: &Next<'_>,
    evaluation: i64,
    issued_at: DateTime<Utc>,
) -> Result<Vec<u8>, StatementError> {
    let stated: Option<Stated> = sqlx::query_as(&format!(
        "SELECT {STATED} FROM reserve_evaluations \
         WHERE id = $1 AND asset = $2 AND outcome <> 'incomplete'"
    ))
    .bind(evaluation)
    .bind(&next.a.asset)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(stated) = stated else {
        return Err(StatementError::Stuck(format!(
            "{}: it names no complete evaluation of this asset",
            next.name
        )));
    };
    Ok(document(conn, next, issued_at, &stated).await?)
}

impl<S: StatementSigner, T: StatementStore> Statements<S, T> {
    /// Vets `bytes`, a statement the current key signed for `next`'s place that never reached the
    /// store, before anything of it is written (PR #50 review M3): it must claim this place (this
    /// asset, sequence, predecessor and abandoned places) and name the current statement key.
    /// Returns the evaluation and issue time it states, and the statement bezent re-derives from
    /// them.
    async fn vet(
        &self,
        tx: &mut sqlx::PgConnection,
        next: &Next<'_>,
        bytes: &[u8],
    ) -> Result<(i64, DateTime<Utc>, Vec<u8>), StatementError> {
        let stuck = |why: &str| StatementError::Stuck(format!("{}: {why}", next.name));
        let doc: Value = serde_json::from_slice(bytes).map_err(|_| stuck("not a statement"))?;
        if !next.claimed_by(&doc) || doc["signing_key"]["public_key"] != hex0x(next.public_key) {
            return Err(stuck(
                "the signed statement is not this asset's next statement under this key; nothing was stored",
            ));
        }
        let (Some(evaluation), Some(issued_at)) = (
            doc["evaluation"]["id"].as_i64(),
            doc["issued_at"]
                .as_str()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc)),
        ) else {
            return Err(stuck("no evaluation or issue time"));
        };
        let derived = rederive(tx, next, evaluation, issued_at).await?;
        Ok((evaluation, issued_at, derived))
    }

    /// Writes `bytes` and their `signature` (vetted) to the store under `next.name`, records the
    /// statement, and commits.
    async fn store_and_record(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        next: Next<'_>,
        bytes: Vec<u8>,
        signature: Vec<u8>,
        evaluation: i64,
        issued_at: DateTime<Utc>,
    ) -> Result<Published, StatementError> {
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        let signature_name = format!("{}.sig", next.name);
        let signature_file = signature_file(&next.name, &sha256, next.public_key, &signature);
        // The row first, so the store's guards refuse a statement that does not extend the chain
        // before anything reaches an append-only bucket.
        sqlx::query(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, previous_sha256, public_key, signature) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(&next.a.asset)
        .bind(next.sequence)
        .bind(evaluation)
        .bind(issued_at)
        .bind(&next.name)
        .bind(sha256.as_slice())
        .bind(next.previous)
        .bind(next.public_key)
        .bind(&signature)
        .execute(&mut *tx)
        .await?;
        // The date is the signed statement's own, not this attempt's: a retry on a later day
        // finds the object a cut-short attempt left (issue #58).
        let receipt = self
            .store
            .put_at(&next.name, issued_at, &bytes)
            .await
            .map_err(StatementError::Store)?;
        let signature_receipt = self
            .store
            .put_at(&signature_name, issued_at, &signature_file)
            .await
            .map_err(StatementError::Store)?;
        record_objects(
            &mut tx,
            &next.a.asset,
            next.sequence,
            &[
                (&next.name, &receipt, &bytes),
                (&signature_name, &signature_receipt, &signature_file),
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(Published {
            asset: next.a.asset.clone(),
            sequence: next.sequence,
            name: next.name,
            signature_name,
            bytes,
            sha256,
            receipt,
            signature_receipt,
        })
    }

    /// Adopts `bytes`, the statement an earlier attempt left under `next.name` without recording
    /// it, only if it is this asset's statement `next.sequence`, names `next.previous` and the
    /// places abandoned since, names this key, and either its signature file already verifies with this key, or its bytes are exactly
    /// the statement re-derived from the evaluation and issue time it names, which is then signed.
    /// Whatever else is in the store is never signed (review SF4).
    async fn adopt(
        &self,
        mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
        next: Next<'_>,
        stored: Stored,
    ) -> Result<Published, StatementError> {
        let Stored {
            bytes,
            receipt: statement_receipt,
        } = stored;
        let doc: Value = serde_json::from_slice(&bytes)
            .map_err(|_| StatementError::Stuck(format!("{}: not a statement", next.name)))?;
        let claimed =
            next.claimed_by(&doc) && doc["signing_key"]["public_key"] == hex0x(next.public_key);
        let (a, sequence, previous, public_key) =
            (next.a, next.sequence, next.previous, next.public_key);
        let name = next.name.clone();
        let asset = a.asset.as_str();
        let stuck = |why: &str| StatementError::Stuck(format!("{name}: {why}"));
        if !claimed {
            return Err(stuck("not this asset's next statement under this key"));
        }
        let (Some(evaluation), Some(issued_at)) = (
            doc["evaluation"]["id"].as_i64(),
            doc["issued_at"]
                .as_str()
                .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
                .map(|t| t.with_timezone(&Utc)),
        ) else {
            return Err(stuck("no evaluation or issue time"));
        };
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        let signature_name = format!("{name}.sig");
        let held = self
            .store
            .get_at(&signature_name, issued_at)
            .await
            .map_err(StatementError::Store)?;
        let recorded = match held {
            Some(_) => None,
            None => self.recorded_signature(&sha256, &bytes, public_key).await?,
        };
        let (signature, signature_receipt, signature_bytes) = match held {
            // Signed with this key already: adopted as it is, nothing new signed.
            Some(held) => {
                if verify(&bytes, &held.bytes, public_key).is_err() {
                    return Err(stuck("its signature file does not verify"));
                }
                let sig: Value = serde_json::from_slice(&held.bytes)
                    .map_err(|_| stuck("its signature file is malformed"))?;
                let signature = unhex(&sig["signature"])
                    .ok_or_else(|| stuck("its signature file is malformed"))?;
                (signature, held.receipt, held.bytes)
            }
            // Unsigned: a signature file is written only for exactly the bytes bezent re-derives
            // (PR #50 review M3). Signed through a recorded statement intent over exactly these
            // bytes (issue #42), that signature, verified, is the one published; otherwise the
            // statement key signs them now.
            None => {
                let derived = rederive(&mut tx, &next, evaluation, issued_at).await?;
                if derived != bytes {
                    return Err(stuck(&format!(
                        "it is not the statement bezent re-derives from evaluation {evaluation}"
                    )));
                }
                let signature = match recorded {
                    Some(signature) => signature,
                    None => signed(&self.signer, public_key, &bytes).await?,
                };
                let file = signature_file(&name, &sha256, public_key, &signature);
                let receipt = self
                    .store
                    .put_at(&signature_name, issued_at, &file)
                    .await
                    .map_err(StatementError::Store)?;
                (signature, receipt, file)
            }
        };
        sqlx::query(
            "INSERT INTO reserve_statements (asset, sequence, evaluation_id, issued_at, name, \
                 sha256, previous_sha256, public_key, signature) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(asset)
        .bind(sequence)
        .bind(evaluation)
        .bind(issued_at)
        .bind(&name)
        .bind(sha256.as_slice())
        .bind(previous)
        .bind(public_key)
        .bind(&signature)
        .execute(&mut *tx)
        .await?;
        record_objects(
            &mut tx,
            asset,
            sequence,
            &[
                (&name, &statement_receipt, &bytes),
                (&signature_name, &signature_receipt, &signature_bytes),
            ],
        )
        .await?;
        tx.commit().await?;
        tracing::warn!(asset, name = %name, "REQ-PR-2: adopted a statement an earlier attempt left in the store");
        Ok(Published {
            asset: asset.to_owned(),
            sequence,
            name,
            signature_name,
            bytes,
            sha256,
            receipt: statement_receipt,
            signature_receipt,
        })
    }
}

impl<S: StatementSigner, T: StatementStore> Statements<S, T> {
    /// The statement key's signature recorded over exactly `bytes` (SHA-256 `sha256`), through a
    /// statement intent, if one verifies with `public_key`.
    async fn recorded_signature(
        &self,
        sha256: &[u8],
        bytes: &[u8],
        public_key: &[u8],
    ) -> Result<Option<Vec<u8>>, StatementError> {
        let signatures: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT g.signature FROM intents i JOIN signatures g ON g.intent_id = i.id \
             WHERE i.purpose = 'statement' AND i.payload -> 'statement' ->> 'sha256' = $1 \
             ORDER BY i.id",
        )
        .bind(hex0x(sha256))
        .fetch_all(&self.pool)
        .await?;
        Ok(signatures
            .into_iter()
            .find(|g| verifies_exactly("ed25519", public_key, bytes, g)))
    }
}

/// The issue time a signed statement's bytes (the intent's `message`, hex) state.
fn issued_at_of(message: &str) -> Option<DateTime<Utc>> {
    let bytes = hex::decode(message.strip_prefix("0x").unwrap_or(message)).ok()?;
    let doc: Value = serde_json::from_slice(&bytes).ok()?;
    DateTime::parse_from_rfc3339(doc["issued_at"].as_str()?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Records, in the open transaction, where the store wrote each object of the statement at
/// (`asset`, `sequence`) and its receipt (migration 0053; issue #58): called only after every
/// write succeeded, so a row means the object is in the store.
async fn record_objects(
    tx: &mut sqlx::PgConnection,
    asset: &str,
    sequence: i64,
    objects: &[(&str, &Receipt, &[u8])],
) -> sqlx::Result<()> {
    for (name, receipt, bytes) in objects {
        sqlx::query(
            "INSERT INTO reserve_statement_objects \
                 (asset, sequence, name, location, sha256, etag, version_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(asset)
        .bind(sequence)
        .bind(name)
        .bind(&receipt.location)
        .bind(Sha256::digest(bytes).as_slice())
        .bind(&receipt.etag)
        .bind(&receipt.version_id)
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// The signature file published next to a statement.
fn signature_file(name: &str, sha256: &[u8], public_key: &[u8], signature: &[u8]) -> Vec<u8> {
    serde_json::to_vec_pretty(&json!({
        "statement": name,
        "sha256": hex0x(sha256),
        "scheme": "ed25519",
        "public_key": hex0x(public_key),
        "signature": hex0x(signature),
    }))
    .unwrap_or_default()
}

/// The statement document.
#[allow(clippy::too_many_arguments)]
fn compose(
    a: &Asset,
    sequence: i64,
    previous: Option<&[u8]>,
    now: DateTime<Utc>,
    public_key: &[u8],
    (evaluation, evaluated_at, outcome): (i64, DateTime<Utc>, &str),
    points: &Value,
    chain: &Value,
    book: &Value,
    movements: &[Value],
) -> Value {
    let attested = a.positions.iter().any(super::Position::attested);
    let parties: Vec<Value> = chain["reserve"]
        .as_array()
        .map(|r| {
            r.iter()
                .filter_map(|p| p.get("attested_by").cloned())
                .collect()
        })
        .unwrap_or_default();
    let (recomputable, on_reports): (Vec<&str>, Vec<&str>) = if attested {
        (
            vec!["supply", "in_transit"],
            vec!["reserve", "owed_mint", "owed_payout", "surplus"],
        )
    } else {
        (
            vec![
                "reserve",
                "in_transit",
                "supply",
                "owed_mint",
                "owed_payout",
                "surplus",
            ],
            vec![],
        )
    };
    json!({
        "statement": "bezent reserve statement",
        "version": 1,
        "asset": a.asset,
        "sequence": sequence,
        "previous_sha256": previous.map(hex0x),
        "issued_at": now.to_rfc3339(),
        "claim": CLAIM,
        "reserve_book": BOOK,
        "backend": {
            "assurance_class": a.assurance_class,
            "networks": a.positions.iter().map(|p| json!({
                "network": p.network, "backend": p.backend,
                "assurance_class": if p.attested() { "attested" } else { "public" },
            })).collect::<Vec<_>>(),
            "attested_by": parties,
        },
        "published_addresses": a.positions.iter().map(|p| json!({
            "network": p.network, "address": p.address,
            "token": p.token.as_ref().map(|(t, _)| t), "holds": "reserve",
        })).collect::<Vec<_>>(),
        // REQ-BK-5, REQ-BK-6: an arrival at a reserve address from one of these, at or after the
        // block it applies from, is the operator's contribution to surplus; any other arrival is
        // owed mint. A declaration applies from no block (`null`) when it predates the rule.
        "operator_funding_addresses": a.positions.iter().flat_map(|p| p.funding.iter().map(|f| json!({
            "network": p.network, "address": f.address,
            "token": p.token.as_ref().map(|(t, _)| t),
            "funds": p.address, "from_block": f.from_block,
        }))).collect::<Vec<_>>(),
        "units": {
            "book_unit": "the minor unit of the origin token",
            "book_precision": a.book_precision,
            "issuing_asset": a.issuing_asset,
            "issuing_precision": a.precision,
        },
        "equation": "sum(reserve) + in_transit = supply + owed_mint + owed_payout + surplus",
        "final_blocks": points,
        "terms": {
            "reserve": chain["reserve"],
            "in_transit": chain["in_transit"],
            "supply": chain["supply"],
            "owed_mint": chain["owed_mint"],
            "owed_payout": chain["owed_payout"],
            "surplus": {"amount": chain["surplus"]["amount"],
                        "book_amount": book["surplus"],
                        "how": "the equation's balance: the operator's own value in reserve, recorded per contribution and per cost; `amount` is what the other terms imply, `book_amount` what the reserve book records, equal when the evaluation holds",
                        "contributions": chain["surplus"]["contributions"],
                        "movements": movements},
        },
        "recomputable_from_public_data": recomputable,
        "rest_on_reports": on_reports,
        "unconverted_holdings": {
            "note": HOLDINGS,
            "items": chain["convertible"],
        },
        "evaluation": {
            "id": evaluation,
            "evaluated_at": evaluated_at.to_rfc3339(),
            "outcome": outcome,
        },
        "signing_key": {"scheme": "ed25519", "public_key": hex0x(public_key)},
    })
}

/// Why a statement does not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// A document or signature file is not what it should be.
    Malformed(String),
    /// The signature does not verify with the key.
    BadSignature,
    /// A later statement does not name the earlier one's hash: the earlier one was revised, or
    /// one is missing (`REQ-PR-2`, `AC-E1.2`).
    Revised {
        /// The sequence of the statement whose predecessor does not match.
        sequence: i64,
    },
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "malformed: {why}"),
            Self::BadSignature => f.write_str("the signature does not verify"),
            Self::Revised { sequence } => write!(
                f,
                "statement {sequence} does not name its predecessor's hash: an earlier statement was revised or is missing"
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

fn unhex(v: &Value) -> Option<Vec<u8>> {
    let s = v.as_str()?;
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).ok()
}

/// Verifies a statement's exact `bytes` against its signature file with `public_key`, the
/// statement key as bezent publishes it, and returns the document (`REQ-PR-1`).
pub fn verify(
    bytes: &[u8],
    signature_file: &[u8],
    public_key: &[u8],
) -> Result<Value, VerifyError> {
    let sig: Value = serde_json::from_slice(signature_file)
        .map_err(|e| VerifyError::Malformed(format!("signature file: {e}")))?;
    if sig["scheme"] != "ed25519" {
        return Err(VerifyError::Malformed("not an ed25519 signature".into()));
    }
    let signature =
        unhex(&sig["signature"]).ok_or_else(|| VerifyError::Malformed("signature".into()))?;
    if !bezent_signer::verify::verifies_exactly("ed25519", public_key, bytes, &signature) {
        return Err(VerifyError::BadSignature);
    }
    serde_json::from_slice(bytes).map_err(|e| VerifyError::Malformed(format!("document: {e}")))
}

/// Checks that each of an asset's statements, from its first, in order, names the SHA-256 of the
/// one before (`REQ-PR-2`, `AC-E1.2`), and that every place it skips is one it names as abandoned
/// (PR #50 re-check S-1, S-3). A revision of any earlier statement breaks the link from the next.
pub fn verify_chain(statements: &[Vec<u8>]) -> Result<(), VerifyError> {
    let mut before: Option<(&[u8], i64)> = None;
    for bytes in statements {
        let doc: Value = serde_json::from_slice(bytes)
            .map_err(|e| VerifyError::Malformed(format!("document: {e}")))?;
        let sequence = doc["sequence"]
            .as_i64()
            .ok_or_else(|| VerifyError::Malformed("sequence".into()))?;
        let named = unhex(&doc["previous_sha256"]);
        let expected = before.map(|(b, _)| Sha256::digest(b).to_vec());
        let skipped: Vec<i64> = doc
            .get("abandoned")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|e| e["sequence"].as_i64()).collect())
            .unwrap_or_default();
        let gap: Vec<i64> = (before.map_or(0, |(_, s)| s) + 1..sequence).collect();
        if skipped != gap || named != expected {
            return Err(VerifyError::Revised { sequence });
        }
        before = Some((bytes, sequence));
    }
    Ok(())
}

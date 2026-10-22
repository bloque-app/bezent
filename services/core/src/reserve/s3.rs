// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The S3 statement sink (issue #58; `REQ-PR-1`, `REQ-PR-2`; PLAN `DEC-16`): the core writes each
//! daily signed statement, and its signature file, to an object-locked bucket itself.
//!
//! - **Key.** `<prefix>/<YYYY>/<MM>/<DD>/statement-<asset>-<date>-<sequence>.json`, and the
//!   signature file next to it with `.sig` appended. The date is the statement's own `issued_at`
//!   (UTC), never the clock of the retry: a statement stored on one day and recorded on the next is
//!   found again. The sequence is the statement's own (ten digits): a place abandoned and replaced
//!   the same UTC day gets a key of its own, never the abandoned one's. The asset is named in the
//!   key; there is no combined statement.
//! - **Never overwritten.** Every `PutObject` carries `If-None-Match: *`. A `412` is not retried:
//!   the sink reads the object back and succeeds only when it holds exactly the bytes being
//!   written (a write that landed and whose answer was lost); different bytes are
//!   [`StoreError::Exists`], which the core surfaces and pages (`REQ-PR-2`).
//! - **Misconfiguration is an alert.** S3 answers `GetObject` of a missing key with `403` when the
//!   caller lacks `s3:ListBucket` on the whole bucket (a prefix condition does not apply to
//!   `GetObject`), and answers `404` with `x-amz-delete-marker` when the key's latest version is a
//!   delete marker (after which `If-None-Match: *` would accept a new version). Both are
//!   [`StoreError::Misconfigured`], paged by name, never a silent permanent failure of the day.
//! - **Retained.** Each put carries an Object Lock mode and a retain-until date, a SHA-256
//!   checksum the bucket verifies, and, when configured, SSE-KMS.
//! - **Retried.** Throttling, `409`, `5xx`, timeouts and connection failures are retried with a
//!   doubling backoff, three attempts of at most three seconds each. What still fails is an error
//!   the core pages on; a statement is never silently skipped. The core's 60 s cycle budget covers
//!   all assets together, so a slow bucket can leave later assets for the next cycle, and
//!   `statement-overdue` is the backstop.
//! - **Recorded after.** The sink only writes. The core records the key, ETag and version id in its
//!   store after the write succeeded (migration 0053), never before.
//!
//! The sink is generic over an [`ObjectClient`]: [`AwsClient`] in deployment, a fake in tests.

use std::fmt;
use std::future::Future;
use std::time::Duration;

use aws_sdk_s3::error::ProvideErrorMetadata;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use super::statement::{DirectoryStore, Receipt, StatementStore, StoreError, Stored};
use crate::config::{ObjectLockMode, S3Config};

/// What one attempt at an object operation came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    /// `412 Precondition Failed`: the key exists (`If-None-Match: *`).
    PreconditionFailed,
    /// Worth trying again: throttling, `409`, `5xx`, a timeout, a lost connection.
    Transient(String),
    /// Not worth trying again: a bad request, a missing bucket.
    Permanent(String),
    /// `403` on a `GetObject`: without `s3:ListBucket` on the whole bucket S3 answers a missing key
    /// so. A configuration error of the bucket or the role, not a verdict on the key.
    Forbidden(String),
    /// `404` with `x-amz-delete-marker: true`: the key's latest version is a delete marker.
    DeleteMarker,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PreconditionFailed => f.write_str("412 precondition failed: the key exists"),
            Self::Transient(e) => write!(f, "transient: {e}"),
            Self::Permanent(e) => write!(f, "permanent: {e}"),
            Self::Forbidden(e) => write!(f, "403 on a read: {e}"),
            Self::DeleteMarker => f.write_str("the key's latest version is a delete marker"),
        }
    }
}

/// An object's retention, as sent with the put.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    /// `GOVERNANCE` or `COMPLIANCE`.
    pub mode: ObjectLockMode,
    /// The date before which the object cannot be deleted or overwritten.
    pub until: DateTime<Utc>,
}

/// One `PutObject`, as the sink asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PutRequest {
    /// The object key.
    pub key: String,
    /// The object's bytes.
    pub body: Vec<u8>,
    /// Its media type.
    pub content_type: &'static str,
    /// The base64 of the SHA-256 of `body`, sent as the checksum the bucket verifies.
    pub checksum_sha256: String,
    /// Always set: `If-None-Match: *`, so an existing key is never overwritten.
    pub if_none_match: bool,
    /// The retention headers, unless the lock mode is `off`.
    pub retention: Option<Retention>,
    /// The SSE-KMS key, if one is configured.
    pub kms_key_id: Option<String>,
}

/// What the bucket answered a successful put with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PutOutcome {
    /// The ETag.
    pub etag: Option<String>,
    /// The version id (the bucket is versioned).
    pub version_id: Option<String>,
}

/// An object read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// Its bytes.
    pub bytes: Vec<u8>,
    /// The ETag.
    pub etag: Option<String>,
    /// The version id.
    pub version_id: Option<String>,
}

/// The two bucket operations the sink needs. Nothing lists, deletes or changes retention.
pub trait ObjectClient: Send + Sync {
    /// One attempt at `PutObject`.
    fn put(
        &self,
        request: &PutRequest,
    ) -> impl Future<Output = Result<PutOutcome, ClientError>> + Send;

    /// One attempt at `GetObject`; `Ok(None)` when the key does not exist.
    fn get(&self, key: &str) -> impl Future<Output = Result<Option<Fetched>, ClientError>> + Send;
}

/// How often and how fast the sink tries again.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    /// Attempts in all, the first included.
    pub attempts: u32,
    /// The wait after the first failed attempt; doubled after each.
    pub first_delay: Duration,
}

impl Backoff {
    /// Three attempts, waiting 0.5 s and 1 s between them: 1.5 s, and with the client's
    /// [`ATTEMPT_TIMEOUT`] a worst case of 10.5 s an operation. A statement is a read and two
    /// writes, about 32 s, so one asset fits the core's 60 s cycle budget, which covers all assets
    /// together; when a slow bucket outlasts it, the assets not reached are retried next cycle and
    /// `statement-overdue` is the backstop.
    pub const DEFAULT: Self = Self {
        attempts: 3,
        first_delay: Duration::from_millis(500),
    };
}

/// How long one attempt may take in the real client.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);

/// The statement store backed by an object-locked bucket.
pub struct S3Store<C> {
    client: C,
    prefix: String,
    mode: ObjectLockMode,
    retention_days: u32,
    kms_key_id: Option<String>,
    backoff: Backoff,
}

impl<C: ObjectClient> S3Store<C> {
    /// The store for `config` (validated by [`S3Config::validate`]) over `client`.
    pub fn new(client: C, config: &S3Config) -> Self {
        Self {
            client,
            prefix: config.prefix.clone(),
            mode: config.object_lock_mode,
            retention_days: config.retention_days(),
            kms_key_id: config.kms_key_id.clone(),
            backoff: Backoff::DEFAULT,
        }
    }

    /// The same store with another backoff (tests).
    #[must_use]
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// The object key of the statement object `name` (`<asset>/<sequence>.json`, or that and
    /// `.sig`) issued at `issued_at`: the asset, the issue date and the sequence are all in it.
    pub fn key(&self, name: &str, issued_at: DateTime<Utc>) -> Result<String, StoreError> {
        let bad = || StoreError::Io(format!("{name} is not a statement name"));
        let (asset, file) = name.split_once('/').ok_or_else(bad)?;
        let suffix = if file.ends_with(".json.sig") {
            ".json.sig"
        } else if file.ends_with(".json") {
            ".json"
        } else {
            return Err(bad());
        };
        let sequence = file.strip_suffix(suffix).unwrap_or_default();
        if sequence.is_empty()
            || !sequence.bytes().all(|c| c.is_ascii_digit())
            || asset.is_empty()
            || !asset
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            || asset == "."
            || asset == ".."
        {
            return Err(bad());
        }
        let path = issued_at.format("%Y/%m/%d");
        let date = issued_at.format("%Y-%m-%d");
        let head = if self.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", self.prefix)
        };
        Ok(format!(
            "{head}{path}/statement-{asset}-{date}-{sequence}{suffix}"
        ))
    }

    fn request(&self, key: String, bytes: &[u8], now: DateTime<Utc>) -> PutRequest {
        let retention = match self.mode {
            ObjectLockMode::Off => None,
            mode => Some(Retention {
                mode,
                until: now + chrono::Duration::days(i64::from(self.retention_days)),
            }),
        };
        PutRequest {
            key,
            body: bytes.to_vec(),
            content_type: "application/json",
            checksum_sha256: BASE64.encode(Sha256::digest(bytes)),
            if_none_match: true,
            retention,
            kms_key_id: self.kms_key_id.clone(),
        }
    }

    /// What a failed read of `key` is to the caller: a refusal that is the bucket's or the role's
    /// configuration, named as such, or an ordinary store failure.
    fn unreadable(key: &str, e: &ClientError) -> StoreError {
        match e {
            ClientError::Forbidden(why) => StoreError::Misconfigured(format!(
                "{key}: the bucket answered 403 to a read of the key about to be written ({why}); \
                 check s3:ListBucket on the whole bucket (no s3:prefix condition) and s3:GetObject on the prefix"
            )),
            ClientError::DeleteMarker => StoreError::Misconfigured(format!(
                "{key}: the key's latest version is a delete marker; something deleted a statement \
                 object, and a new version here would hide it. Check the bucket policy's deny of s3:DeleteObject"
            )),
            other => StoreError::Io(format!("{key}: {other}")),
        }
    }

    async fn pause(&self, failed: u32) {
        let wait = self.backoff.first_delay * 2u32.saturating_pow(failed.saturating_sub(1));
        tokio::time::sleep(wait).await;
    }

    async fn write(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
        bytes: &[u8],
    ) -> Result<Receipt, StoreError> {
        let key = self.key(name, issued_at)?;
        let request = self.request(key.clone(), bytes, Utc::now());
        let mut last = String::new();
        for attempt in 1..=self.backoff.attempts.max(1) {
            if attempt > 1 {
                self.pause(attempt - 1).await;
            }
            match self.client.put(&request).await {
                Ok(done) => {
                    return Ok(Receipt {
                        location: key,
                        etag: done.etag,
                        version_id: done.version_id,
                    });
                }
                // The key exists. Never retried: read it back. The same bytes are a write that
                // landed and whose answer was lost; anything else is a refusal to overwrite.
                Err(ClientError::PreconditionFailed) => match self.client.get(&key).await {
                    Ok(Some(held)) if held.bytes == bytes => {
                        tracing::warn!(key = %key, "REQ-PR-2: the object was already written with exactly these bytes");
                        return Ok(Receipt {
                            location: key,
                            etag: held.etag,
                            version_id: held.version_id,
                        });
                    }
                    Ok(Some(_)) => return Err(StoreError::Exists(key)),
                    Ok(None) => {
                        return Err(StoreError::Io(format!(
                            "{key}: the bucket refused the write as existing, yet holds no object"
                        )));
                    }
                    Err(
                        e @ (ClientError::Permanent(_)
                        | ClientError::Forbidden(_)
                        | ClientError::DeleteMarker),
                    ) => {
                        return Err(Self::unreadable(&key, &e));
                    }
                    Err(e) => last = e.to_string(),
                },
                Err(ClientError::Permanent(e) | ClientError::Forbidden(e)) => {
                    return Err(StoreError::Io(format!("{key}: {e}")));
                }
                Err(e @ (ClientError::Transient(_) | ClientError::DeleteMarker)) => {
                    last = e.to_string();
                }
            }
            tracing::warn!(key = %key, attempt, error = %last, "REQ-PR-2: writing a statement to the bucket failed");
        }
        Err(StoreError::Io(format!(
            "{key}: not written after {} attempts: {last}",
            self.backoff.attempts.max(1)
        )))
    }

    async fn read(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
    ) -> Result<Option<Stored>, StoreError> {
        let key = self.key(name, issued_at)?;
        let mut last = String::new();
        for attempt in 1..=self.backoff.attempts.max(1) {
            if attempt > 1 {
                self.pause(attempt - 1).await;
            }
            match self.client.get(&key).await {
                Ok(held) => {
                    return Ok(held.map(|h| Stored {
                        bytes: h.bytes,
                        receipt: Receipt {
                            location: key.clone(),
                            etag: h.etag,
                            version_id: h.version_id,
                        },
                    }));
                }
                Err(
                    e @ (ClientError::Permanent(_)
                    | ClientError::Forbidden(_)
                    | ClientError::DeleteMarker),
                ) => {
                    return Err(Self::unreadable(&key, &e));
                }
                Err(e) => last = e.to_string(),
            }
        }
        Err(StoreError::Io(format!(
            "{key}: not read after {} attempts: {last}",
            self.backoff.attempts.max(1)
        )))
    }
}

impl<C: ObjectClient> StatementStore for S3Store<C> {
    fn put(
        &self,
        name: &str,
        _bytes: &[u8],
    ) -> impl Future<Output = Result<(), StoreError>> + Send {
        std::future::ready(Err(StoreError::Io(format!(
            "{name}: the bucket keys a statement by its issue date; use put_at"
        ))))
    }

    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        std::future::ready(Err(StoreError::Io(format!(
            "{name}: the bucket keys a statement by its issue date; use get_at"
        ))))
    }

    fn put_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
        bytes: &[u8],
    ) -> impl Future<Output = Result<Receipt, StoreError>> + Send {
        self.write(name, issued_at, bytes)
    }

    fn get_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<Option<Stored>, StoreError>> + Send {
        self.read(name, issued_at)
    }
}

/// Credentials never come from keys in the environment (`REQ-KY-1`): the task role only.
const FORBIDDEN_ENV: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
    "AWS_SHARED_CREDENTIALS_FILE",
];

/// The real bucket, through the AWS SDK.
pub struct AwsClient {
    client: aws_sdk_s3::Client,
    bucket: String,
}

impl AwsClient {
    /// A client for `config`'s bucket. Credentials come from the ECS task role and nowhere else:
    /// not the instance role (a container on the host must not hand its task the host's role),
    /// not the SDK's default chain, which reads keys from the environment first. A key in the
    /// environment, or no task role, stops the core (`REQ-KY-1`).
    pub async fn connect(config: &S3Config) -> Result<Self, String> {
        for var in FORBIDDEN_ENV {
            if std::env::var_os(var).is_some() {
                return Err(format!(
                    "{var} is set: the statement sink takes its credentials from the task role only (REQ-KY-1)"
                ));
            }
        }
        if std::env::var_os("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_none()
            && std::env::var_os("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_none()
        {
            return Err("no ECS task role: AWS_CONTAINER_CREDENTIALS_RELATIVE_URI is not set; the statement sink takes its credentials from the task role only, never from the instance role (REQ-KY-1)".into());
        }
        let chain = aws_config::ecs::EcsCredentialsProvider::builder().build();
        let shared = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()))
            .credentials_provider(chain)
            // The sink does its own retrying, with its own bound.
            .retry_config(aws_config::retry::RetryConfig::disabled())
            .timeout_config(
                aws_config::timeout::TimeoutConfig::builder()
                    .operation_attempt_timeout(ATTEMPT_TIMEOUT)
                    .build(),
            )
            .load()
            .await;
        Ok(Self {
            client: aws_sdk_s3::Client::new(&shared),
            bucket: config.bucket.clone(),
        })
    }
}

impl AwsClient {
    /// The client over an SDK client already built (the wire test's canned HTTP client).
    pub fn from_client(client: aws_sdk_s3::Client, bucket: &str) -> Self {
        Self {
            client,
            bucket: bucket.to_owned(),
        }
    }
}

/// Sorts an SDK failure into what the sink does about it.
fn classify<E, R>(e: &aws_sdk_s3::error::SdkError<E, R>, status: Option<u16>) -> ClientError
where
    E: ProvideErrorMetadata + std::error::Error + 'static,
    R: fmt::Debug,
{
    use aws_sdk_s3::error::{DisplayErrorContext, SdkError};
    let text = DisplayErrorContext(e).to_string();
    let code = e.code().unwrap_or_default();
    match status {
        Some(412) => ClientError::PreconditionFailed,
        Some(409 | 429 | 500..=599) => ClientError::Transient(text),
        _ if matches!(
            code,
            "SlowDown" | "RequestTimeout" | "InternalError" | "ServiceUnavailable"
        ) || matches!(e, SdkError::TimeoutError(_) | SdkError::DispatchFailure(_)) =>
        {
            ClientError::Transient(text)
        }
        _ => ClientError::Permanent(text),
    }
}

fn text_of(e: &impl fmt::Display) -> String {
    e.to_string()
}

impl ObjectClient for AwsClient {
    async fn put(&self, request: &PutRequest) -> Result<PutOutcome, ClientError> {
        use aws_sdk_s3::primitives::{ByteStream, DateTime as AwsDateTime};
        use aws_sdk_s3::types::{ObjectLockMode as Mode, ServerSideEncryption};
        let mut put = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&request.key)
            .body(ByteStream::from(request.body.clone()))
            .content_type(request.content_type)
            .checksum_sha256(&request.checksum_sha256);
        if request.if_none_match {
            put = put.if_none_match("*");
        }
        if let Some(r) = request.retention {
            let mode = match r.mode {
                ObjectLockMode::Compliance => Mode::Compliance,
                _ => Mode::Governance,
            };
            put = put
                .object_lock_mode(mode)
                .object_lock_retain_until_date(AwsDateTime::from_secs(r.until.timestamp()));
        }
        if let Some(key) = &request.kms_key_id {
            put = put
                .server_side_encryption(ServerSideEncryption::AwsKms)
                .ssekms_key_id(key);
        }
        match put.send().await {
            Ok(done) => Ok(PutOutcome {
                etag: done.e_tag().map(str::to_owned),
                version_id: done.version_id().map(str::to_owned),
            }),
            Err(e) => Err(classify(&e, e.raw_response().map(|r| r.status().as_u16()))),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Fetched>, ClientError> {
        use aws_sdk_s3::operation::get_object::GetObjectError;
        let got = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(got) => got,
            Err(e) => {
                let raw = e.raw_response();
                if matches!(e.as_service_error(), Some(GetObjectError::NoSuchKey(_))) {
                    // A 404 carrying `x-amz-delete-marker` is a key whose latest version is a
                    // delete marker, not a key never written.
                    let marker = raw
                        .and_then(|r| r.headers().get("x-amz-delete-marker"))
                        .is_some_and(|v| v.eq_ignore_ascii_case("true"));
                    return if marker {
                        Err(ClientError::DeleteMarker)
                    } else {
                        Ok(None)
                    };
                }
                let status = raw.map(|r| r.status().as_u16());
                return Err(match classify(&e, status) {
                    ClientError::Permanent(text) if status == Some(403) => {
                        ClientError::Forbidden(text)
                    }
                    other => other,
                });
            }
        };
        let (etag, version_id) = (
            got.e_tag().map(str::to_owned),
            got.version_id().map(str::to_owned),
        );
        let bytes = got
            .body
            .collect()
            .await
            .map_err(|e| ClientError::Transient(text_of(&e)))?
            .into_bytes()
            .to_vec();
        Ok(Some(Fetched {
            bytes,
            etag,
            version_id,
        }))
    }
}

/// The statement sink a deployment is configured with (issue #58): the object-locked bucket, or,
/// in tests only, a directory. The configuration admits the directory only in a test deployment.
pub enum StatementSink {
    /// A local directory, files created once.
    Directory(DirectoryStore),
    /// The object-locked bucket.
    S3(S3Store<AwsClient>),
}

impl StatementStore for StatementSink {
    fn put(&self, name: &str, bytes: &[u8]) -> impl Future<Output = Result<(), StoreError>> + Send {
        let name = name.to_owned();
        let bytes = bytes.to_vec();
        async move {
            match self {
                Self::Directory(d) => d.put(&name, &bytes).await,
                Self::S3(s) => s.put(&name, &bytes).await,
            }
        }
    }

    fn get(&self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, StoreError>> + Send {
        let name = name.to_owned();
        async move {
            match self {
                Self::Directory(d) => d.get(&name).await,
                Self::S3(s) => s.get(&name).await,
            }
        }
    }

    fn put_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
        bytes: &[u8],
    ) -> impl Future<Output = Result<Receipt, StoreError>> + Send {
        let name = name.to_owned();
        let bytes = bytes.to_vec();
        async move {
            match self {
                Self::Directory(d) => d.put_at(&name, issued_at, &bytes).await,
                Self::S3(s) => s.put_at(&name, issued_at, &bytes).await,
            }
        }
    }

    fn get_at(
        &self,
        name: &str,
        issued_at: DateTime<Utc>,
    ) -> impl Future<Output = Result<Option<Stored>, StoreError>> + Send {
        let name = name.to_owned();
        async move {
            match self {
                Self::Directory(d) => d.get_at(&name, issued_at).await,
                Self::S3(s) => s.get_at(&name, issued_at).await,
            }
        }
    }
}

/// An in-memory bucket with the semantics the sink relies on, for tests: `If-None-Match: *`
/// refuses a key that has a live object, every successful put gets a new version id, failures can
/// be scripted, and, as the real bucket does:
///
/// - **Object Lock.** Retention headers are refused when the bucket has Object Lock off
///   ([`State::no_object_lock`]) or ask for less than the bucket's minimum
///   ([`State::minimum_retention`]); a retention is never shortened
///   ([`State::shorten_retention`]) and a version under retention cannot be deleted
///   ([`State::delete_version`]).
/// - **Versioning.** A delete adds a delete marker ([`State::delete_marker`]); the key then answers
///   a read with [`ClientError::DeleteMarker`], yet `If-None-Match: *` accepts a new version there
///   (that is S3's behaviour, and why the bucket policy must deny deletes).
/// - **`ListBucket`.** With [`State::no_list_bucket`] set, a read of a missing key is a `403`, not
///   a `404`, whatever a prefix condition says.
///
/// It is no stand-in for S3's behaviour beyond that.
pub mod fake {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};
    use std::sync::{Arc, Mutex};

    use chrono::{DateTime, Utc};
    use sha2::{Digest, Sha256};

    use super::{ClientError, Fetched, ObjectClient, PutOutcome, PutRequest, Retention};
    use crate::config::ObjectLockMode;

    /// What the fake bucket holds and was asked.
    #[derive(Default)]
    pub struct State {
        /// The live (current) objects by key: bytes, ETag, version id.
        pub objects: BTreeMap<String, (Vec<u8>, String, String)>,
        /// Versions no longer current (deleted behind a marker), by key, with their version id
        /// and bytes: still held, and still under their retention.
        pub noncurrent: Vec<(String, String, Vec<u8>)>,
        /// Keys whose latest version is a delete marker.
        pub markers: BTreeSet<String>,
        /// The retention of each version, by version id.
        pub retention: BTreeMap<String, Retention>,
        /// Every put the bucket received, accepted or not.
        pub puts: Vec<PutRequest>,
        /// Answers for the next puts, in order, before the bucket looks at them.
        pub script: VecDeque<ClientError>,
        /// The next put is stored, and answered with a transient error: an answer lost on the way.
        pub lose_next_answer: bool,
        /// Every put fails permanently while set (the bucket is unreachable or forbidden).
        pub forbidden: bool,
        /// Only puts of keys ending in this fail permanently (a cut-short publish).
        pub forbid_suffix: Option<String>,
        /// The caller has no `s3:ListBucket` on the whole bucket: a read of a missing key is 403.
        pub no_list_bucket: bool,
        /// The bucket was created without Object Lock: retention headers are refused.
        pub no_object_lock: bool,
        /// The least retention, in days, a put may ask for; a shorter one is refused.
        pub minimum_retention: Option<i64>,
        /// Gets issued.
        pub gets: usize,
    }

    impl State {
        /// Adds a delete marker on `key`: its live object becomes a noncurrent version.
        pub fn delete_marker(&mut self, key: &str) {
            if let Some((bytes, _, version)) = self.objects.remove(key) {
                self.noncurrent.push((key.to_owned(), version, bytes));
            }
            self.markers.insert(key.to_owned());
        }

        /// Asks to move the retention of the live object at `key` to `until`. Refused unless it
        /// lengthens it, as Object Lock refuses to shorten a COMPLIANCE retention, and a
        /// GOVERNANCE one without the bypass permission the core never holds.
        pub fn shorten_retention(&mut self, key: &str, until: DateTime<Utc>) -> Result<(), String> {
            let version = self
                .objects
                .get(key)
                .map(|o| o.2.clone())
                .ok_or_else(|| "NoSuchKey".to_owned())?;
            let held = self
                .retention
                .get_mut(&version)
                .ok_or_else(|| "the object has no retention".to_owned())?;
            if until < held.until {
                return Err("AccessDenied: a retention is not shortened".into());
            }
            held.until = until;
            Ok(())
        }

        /// Deletes a version for good: refused while it is under retention.
        pub fn delete_version(&mut self, version: &str, now: DateTime<Utc>) -> Result<(), String> {
            if self.retention.get(version).is_some_and(|r| r.until > now) {
                return Err("AccessDenied: the version is under retention".into());
            }
            self.objects.retain(|_, o| o.2 != version);
            self.noncurrent.retain(|(_, v, _)| v != version);
            Ok(())
        }
    }

    /// A cloneable handle on the shared state.
    #[derive(Clone, Default)]
    pub struct FakeBucket(pub Arc<Mutex<State>>);

    impl FakeBucket {
        /// Locks the state.
        pub fn state(&self) -> std::sync::MutexGuard<'_, State> {
            self.0.lock().unwrap()
        }
    }

    impl ObjectClient for FakeBucket {
        async fn put(&self, request: &PutRequest) -> Result<PutOutcome, ClientError> {
            let mut s = self.state();
            s.puts.push(request.clone());
            if s.forbidden
                || s.forbid_suffix
                    .as_ref()
                    .is_some_and(|x| request.key.ends_with(x.as_str()))
            {
                return Err(ClientError::Permanent("AccessDenied".into()));
            }
            if let Some(e) = s.script.pop_front() {
                return Err(e);
            }
            if let Some(r) = request.retention {
                if s.no_object_lock {
                    return Err(ClientError::Permanent(
                        "InvalidRequest: Bucket is missing Object Lock Configuration".into(),
                    ));
                }
                if r.mode == ObjectLockMode::Off {
                    return Err(ClientError::Permanent("InvalidArgument: mode".into()));
                }
                if let Some(days) = s.minimum_retention
                    && r.until
                        < Utc::now() + chrono::Duration::days(days) - chrono::Duration::minutes(1)
                {
                    return Err(ClientError::Permanent(
                        "InvalidArgument: a retention shorter than the bucket's minimum".into(),
                    ));
                }
            }
            // A delete marker is no object: the condition accepts a new version there.
            if request.if_none_match && s.objects.contains_key(&request.key) {
                return Err(ClientError::PreconditionFailed);
            }
            let version = format!("v{}", s.objects.len() + s.noncurrent.len() + 1);
            let etag = format!("\"{}\"", hex::encode(&Sha256::digest(&request.body)[..16]));
            s.markers.remove(&request.key);
            s.objects.insert(
                request.key.clone(),
                (request.body.clone(), etag.clone(), version.clone()),
            );
            if let Some(r) = request.retention {
                s.retention.insert(version.clone(), r);
            }
            if std::mem::take(&mut s.lose_next_answer) {
                return Err(ClientError::Transient("connection reset".into()));
            }
            Ok(PutOutcome {
                etag: Some(etag),
                version_id: Some(version),
            })
        }

        async fn get(&self, key: &str) -> Result<Option<Fetched>, ClientError> {
            let mut s = self.state();
            s.gets += 1;
            if s.markers.contains(key) {
                return Err(ClientError::DeleteMarker);
            }
            match s.objects.get(key) {
                Some((b, e, v)) => Ok(Some(Fetched {
                    bytes: b.clone(),
                    etag: Some(e.clone()),
                    version_id: Some(v.clone()),
                })),
                None if s.no_list_bucket => Err(ClientError::Forbidden("AccessDenied".into())),
                None => Ok(None),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeBucket;
    use super::*;

    fn config(extra: &str) -> S3Config {
        toml::from_str(&format!(
            "bucket = \"bezent-statements\"\nprefix = \"statements\"\nregion = \"us-east-2\"\n{}{extra}",
            if extra.contains("object_lock_mode") {
                ""
            } else {
                "object_lock_mode = \"GOVERNANCE\"\n"
            }
        ))
        .unwrap()
    }

    fn store(bucket: &FakeBucket, extra: &str) -> S3Store<FakeBucket> {
        S3Store::new(bucket.clone(), &config(extra)).with_backoff(Backoff {
            attempts: 4,
            first_delay: Duration::from_millis(1),
        })
    }

    fn day() -> DateTime<Utc> {
        "2026-10-13T00:00:00Z".parse().unwrap()
    }

    const NAME: &str = "bUSDC/0000000001.json";

    #[test]
    fn req_pr_2_the_key_is_the_asset_and_the_issue_date_and_the_signature_sits_beside_it() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        assert_eq!(
            s.key(NAME, day()).unwrap(),
            "statements/2026/10/13/statement-bUSDC-2026-10-13-0000000001.json"
        );
        assert_eq!(
            s.key(&format!("{NAME}.sig"), day()).unwrap(),
            "statements/2026/10/13/statement-bUSDC-2026-10-13-0000000001.json.sig"
        );
        // No prefix: the date directories are the bucket's top level.
        let bare = S3Store::new(
            b,
            &S3Config {
                prefix: String::new(),
                ..config("")
            },
        );
        assert_eq!(
            bare.key(NAME, day()).unwrap(),
            "2026/10/13/statement-bUSDC-2026-10-13-0000000001.json"
        );
        // Names that are not a statement's are refused, never turned into a key.
        for bad in [
            "x.json",
            "../a/0001.json",
            "a/b/0001.json",
            "a/0001.txt",
            "/0001.json",
        ] {
            assert!(s.key(bad, day()).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn req_pr_2_a_put_is_conditional_retained_checksummed_and_encrypted() {
        let b = FakeBucket::default();
        let s = store(&b, "kms_key_id = \"alias/bezent-statements\"\n");
        let r = s.put_at(NAME, day(), b"{}").await.unwrap();
        assert_eq!(
            r.location,
            "statements/2026/10/13/statement-bUSDC-2026-10-13-0000000001.json"
        );
        assert!(r.etag.is_some() && r.version_id.is_some());
        let st = b.state();
        let p = &st.puts[0];
        assert!(p.if_none_match, "If-None-Match: * is always sent");
        assert_eq!(p.checksum_sha256, BASE64.encode(Sha256::digest(b"{}")));
        assert_eq!(p.kms_key_id.as_deref(), Some("alias/bezent-statements"));
        let ret = p.retention.expect("retention headers");
        assert_eq!(ret.mode, ObjectLockMode::Governance);
        let days = (ret.until - Utc::now()).num_days();
        assert!((363..=365).contains(&days), "{days}");
    }

    #[tokio::test]
    async fn req_pr_2_compliance_and_the_configured_retention_are_sent_and_off_sends_none() {
        let b = FakeBucket::default();
        let s = store(
            &b,
            "object_lock_mode = \"COMPLIANCE\"\nretention_days = 30\n",
        );
        s.put_at(NAME, day(), b"{}").await.unwrap();
        let ret = b.state().puts[0].retention.unwrap();
        assert_eq!(ret.mode, ObjectLockMode::Compliance);
        assert!((28..=30).contains(&(ret.until - Utc::now()).num_days()));
        let b = FakeBucket::default();
        let s = store(&b, "object_lock_mode = \"off\"\n");
        s.put_at(NAME, day(), b"{}").await.unwrap();
        assert!(b.state().puts[0].retention.is_none());
    }

    #[tokio::test]
    async fn req_pr_2_an_existing_key_is_never_overwritten_and_the_refusal_is_surfaced() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        s.put_at(NAME, day(), b"first").await.unwrap();
        let key = s.key(NAME, day()).unwrap();
        // Other bytes under the same key: refused, surfaced, not retried, object untouched.
        let err = s.put_at(NAME, day(), b"second").await.unwrap_err();
        assert_eq!(err, StoreError::Exists(key.clone()));
        let st = b.state();
        assert_eq!(st.puts.len(), 2, "a 412 is not retried");
        assert_eq!(st.objects[&key].0, b"first");
        assert_eq!(st.objects.len(), 1);
    }

    #[tokio::test]
    async fn req_pr_2_the_same_bytes_again_are_a_write_that_landed_not_a_second_version() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        let first = s.put_at(NAME, day(), b"same").await.unwrap();
        let again = s.put_at(NAME, day(), b"same").await.unwrap();
        assert_eq!(first, again, "the receipt is the object's own");
        assert_eq!(b.state().objects.len(), 1);
    }

    #[tokio::test]
    async fn req_pr_2_a_transient_error_is_retried_and_a_lost_answer_is_recognised() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        {
            let mut st = b.state();
            st.script
                .push_back(ClientError::Transient("503 SlowDown".into()));
            st.script
                .push_back(ClientError::Transient("timeout".into()));
        }
        let r = s.put_at(NAME, day(), b"x").await.unwrap();
        assert!(r.version_id.is_some());
        assert_eq!(b.state().puts.len(), 3, "two failures, then the write");
        // The write lands and the answer is lost: the retry finds its own bytes and succeeds.
        let b = FakeBucket::default();
        let s = store(&b, "");
        b.state().lose_next_answer = true;
        let r = s.put_at(NAME, day(), b"x").await.unwrap();
        assert_eq!(b.state().objects.len(), 1);
        assert_eq!(
            r.etag,
            b.state().objects.values().next().map(|o| o.1.clone())
        );
    }

    #[tokio::test]
    async fn req_pr_2_a_failure_that_outlasts_the_retries_is_an_error_never_a_skip() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        for _ in 0..10 {
            b.state()
                .script
                .push_back(ClientError::Transient("503".into()));
        }
        let err = s.put_at(NAME, day(), b"x").await.unwrap_err();
        assert!(
            matches!(&err, StoreError::Io(e) if e.contains("4 attempts")),
            "{err}"
        );
        assert_eq!(b.state().puts.len(), 4);
        assert!(b.state().objects.is_empty());
        // A permanent refusal is not retried at all.
        let b = FakeBucket::default();
        let s = store(&b, "");
        b.state().forbidden = true;
        let err = s.put_at(NAME, day(), b"x").await.unwrap_err();
        assert!(
            matches!(&err, StoreError::Io(e) if e.contains("AccessDenied")),
            "{err}"
        );
        assert_eq!(b.state().puts.len(), 1);
    }

    #[tokio::test]
    async fn req_pr_2_get_at_reads_by_the_issue_date_and_a_missing_object_is_none() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        s.put_at(NAME, day(), b"held").await.unwrap();
        let held = s.get_at(NAME, day()).await.unwrap().unwrap();
        assert_eq!(held.bytes, b"held");
        assert!(held.receipt.version_id.is_some());
        // The next day's key is another key.
        let next = day() + chrono::Duration::days(1);
        assert!(s.get_at(NAME, next).await.unwrap().is_none());
        // The undated calls refuse rather than guess a date.
        assert!(s.get(NAME).await.is_err() && s.put(NAME, b"x").await.is_err());
    }

    #[test]
    fn req_pr_2_the_sequence_is_in_the_key_so_a_replaced_place_has_a_key_of_its_own() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        let first = s.key("bUSDC/0000000007.json", day()).unwrap();
        let replacement = s.key("bUSDC/0000000008.json", day()).unwrap();
        assert_ne!(first, replacement, "the same UTC day, two places, two keys");
        assert!(first.ends_with("-2026-10-13-0000000007.json"), "{first}");
        assert_eq!(
            s.key("bUSDC/0000000008.json.sig", day()).unwrap(),
            format!("{replacement}.sig")
        );
    }

    #[tokio::test]
    async fn req_pr_2_a_replaced_place_the_same_day_is_written_beside_the_abandoned_one() {
        let b = FakeBucket::default();
        let s = store(&b, "");
        s.put_at("bUSDC/0000000007.json", day(), b"abandoned")
            .await
            .unwrap();
        // Before the fix both places named one key: a 412 with other bytes, `Exists`, forever.
        let r = s
            .put_at("bUSDC/0000000008.json", day(), b"replacement")
            .await
            .unwrap();
        assert!(r.location.ends_with("-0000000008.json"), "{}", r.location);
        assert_eq!(b.state().objects.len(), 2);
        assert!(
            s.get_at("bUSDC/0000000008.json", day())
                .await
                .unwrap()
                .is_some_and(|h| h.bytes == b"replacement")
        );
    }

    #[tokio::test]
    async fn req_pr_2_a_403_for_the_key_about_to_be_written_is_a_misconfiguration_not_a_missing_key()
     {
        let b = FakeBucket::default();
        let s = store(&b, "");
        b.state().no_list_bucket = true;
        // An existing key is still readable; a missing one is a 403 in the real bucket.
        let err = s.get_at(NAME, day()).await.unwrap_err();
        let StoreError::Misconfigured(why) = &err else {
            panic!("{err}")
        };
        assert!(why.contains("s3:ListBucket on the whole bucket"), "{why}");
        assert_eq!(b.state().gets, 1, "a 403 is not retried");
        // The same on the read-back after a refused overwrite: no object is invented.
        let b = FakeBucket::default();
        let s = store(&b, "");
        s.put_at(NAME, day(), b"x").await.unwrap();
        b.state().no_list_bucket = true;
        assert!(s.get_at(NAME, day()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn req_pr_2_a_key_whose_latest_version_is_a_delete_marker_is_an_alert_and_is_not_written()
    {
        let b = FakeBucket::default();
        let s = store(&b, "");
        let key = s.put_at(NAME, day(), b"first").await.unwrap().location;
        b.state().delete_marker(&key);
        // The fake models S3: a conditional put would be accepted at the marker...
        let err = s.get_at(NAME, day()).await.unwrap_err();
        assert!(
            matches!(&err, StoreError::Misconfigured(w) if w.contains("delete marker")),
            "{err}"
        );
        // ...so the sink, asked to write, never gets as far as a new version when it reads first,
        // and a write on its own that reaches the read-back also names the marker.
        assert!(b.state().objects.is_empty() && b.state().noncurrent.len() == 1);
        let direct = b.put(&s.request(key.clone(), b"second", Utc::now())).await;
        assert!(
            direct.is_ok(),
            "S3 accepts If-None-Match: * at a delete marker"
        );
    }

    #[tokio::test]
    async fn req_pr_2_the_fake_models_object_lock() {
        let b = FakeBucket::default();
        let s = store(&b, "retention_days = 30\n");
        let key = s.put_at(NAME, day(), b"x").await.unwrap();
        let version = key.version_id.unwrap();
        // A retention is never shortened; a version under retention is never deleted.
        let sooner = Utc::now() + chrono::Duration::days(1);
        assert!(b.state().shorten_retention(&key.location, sooner).is_err());
        assert!(b.state().delete_version(&version, Utc::now()).is_err());
        assert!(
            b.state()
                .shorten_retention(&key.location, Utc::now() + chrono::Duration::days(90))
                .is_ok()
        );
        // A bucket without Object Lock refuses the retention headers, and so does a minimum.
        let nolock = FakeBucket::default();
        nolock.state().no_object_lock = true;
        let err = store(&nolock, "")
            .put_at(NAME, day(), b"x")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::Io(e) if e.contains("Object Lock")),
            "{err}"
        );
        let strict = FakeBucket::default();
        strict.state().minimum_retention = Some(365);
        let err = store(&strict, "retention_days = 30\n")
            .put_at(NAME, day(), b"x")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, StoreError::Io(e) if e.contains("minimum")),
            "{err}"
        );
        assert!(store(&strict, "").put_at(NAME, day(), b"x").await.is_ok());
    }

    /// The real SDK client against a canned HTTP client: what goes on the wire, and how each
    /// answer is sorted.
    mod wire {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex};

        use aws_sdk_s3::config::{Credentials, Region};
        use aws_smithy_http_client::test_util::infallible_client_fn;

        use super::super::*;

        type Seen = Arc<Mutex<Vec<(String, String, Vec<(String, String)>)>>>;
        type Canned = Arc<Mutex<VecDeque<(u16, Vec<(&'static str, &'static str)>, String)>>>;

        fn client(canned: Canned, seen: Seen) -> AwsClient {
            let http = infallible_client_fn(move |req| {
                let headers = req
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_owned()))
                    .collect();
                seen.lock().unwrap().push((
                    req.method().to_string(),
                    req.uri().to_string(),
                    headers,
                ));
                let (status, headers, body) = canned.lock().unwrap().pop_front().unwrap();
                let mut response = http::Response::builder().status(status);
                for (k, v) in headers {
                    response = response.header(k, v);
                }
                response.body(body).unwrap()
            });
            let conf = aws_sdk_s3::Config::builder()
                .behavior_version_latest()
                .region(Region::new("us-east-2"))
                .credentials_provider(Credentials::new(
                    "AKIDEXAMPLE",
                    "secret",
                    None,
                    None,
                    "test",
                ))
                .http_client(http)
                .retry_config(aws_config::retry::RetryConfig::disabled())
                .build();
            AwsClient::from_client(aws_sdk_s3::Client::from_conf(conf), "example-bucket")
        }

        type Item<'a> = (u16, Vec<(&'static str, &'static str)>, &'a str);

        fn canned(items: Vec<Item<'_>>) -> Canned {
            Arc::new(Mutex::new(
                items
                    .into_iter()
                    .map(|(s, h, b)| (s, h, b.to_owned()))
                    .collect(),
            ))
        }

        fn request() -> PutRequest {
            PutRequest {
                key: "statements/2026/10/13/statement-bUSDC-2026-10-13.json".into(),
                body: b"{}".to_vec(),
                content_type: "application/json",
                checksum_sha256: BASE64.encode(Sha256::digest(b"{}")),
                if_none_match: true,
                retention: Some(Retention {
                    mode: ObjectLockMode::Compliance,
                    until: "2027-10-13T00:00:00Z".parse().unwrap(),
                }),
                kms_key_id: Some("alias/example".into()),
            }
        }

        fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
            h.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
        }

        #[tokio::test]
        async fn req_pr_2_the_put_carries_the_headers_the_bucket_enforces() {
            let seen = Seen::default();
            let c = client(
                canned(vec![(
                    200,
                    vec![("etag", "\"abc\""), ("x-amz-version-id", "ver1")],
                    "",
                )]),
                seen.clone(),
            );
            let done = c.put(&request()).await.unwrap();
            assert_eq!(done.etag.as_deref(), Some("\"abc\""));
            assert_eq!(done.version_id.as_deref(), Some("ver1"));
            let seen = seen.lock().unwrap();
            let (method, uri, h) = &seen[0];
            assert_eq!(method, "PUT");
            assert!(uri.contains("statement-bUSDC-2026-10-13.json"), "{uri}");
            assert_eq!(header(h, "if-none-match"), Some("*"), "{h:?}");
            assert_eq!(
                header(h, "x-amz-object-lock-mode"),
                Some("COMPLIANCE"),
                "{h:?}"
            );
            assert!(
                header(h, "x-amz-object-lock-retain-until-date")
                    .is_some_and(|d| d.contains("2027-10-13")),
                "{h:?}"
            );
            assert_eq!(
                header(h, "x-amz-checksum-sha256"),
                Some(BASE64.encode(Sha256::digest(b"{}")).as_str()),
                "{h:?}"
            );
            assert_eq!(
                header(h, "x-amz-server-side-encryption"),
                Some("aws:kms"),
                "{h:?}"
            );
            assert_eq!(
                header(h, "x-amz-server-side-encryption-aws-kms-key-id"),
                Some("alias/example"),
                "{h:?}"
            );
            assert_eq!(header(h, "content-type"), Some("application/json"), "{h:?}");
            assert!(
                h.iter().all(|(k, _)| k != "x-amz-checksum-crc32"),
                "the SDK added its own checksum next to ours: {h:?}"
            );
        }

        #[tokio::test]
        async fn req_pr_2_each_answer_is_sorted_as_the_sink_acts_on_it() {
            let seen = Seen::default();
            let xml = |code: &str| {
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code>\
                     <Message>m</Message></Error>"
                )
            };
            let items = vec![
                (412, vec![], xml("PreconditionFailed")),
                (409, vec![], xml("ConditionalRequestConflict")),
                (503, vec![], xml("SlowDown")),
                (403, vec![], xml("AccessDenied")),
                (404, vec![], xml("NoSuchKey")),
                (403, vec![], xml("AccessDenied")),
                (500, vec![], xml("InternalError")),
            ];
            let c = client(Arc::new(Mutex::new(items.into_iter().collect())), seen);
            assert_eq!(
                c.put(&request()).await,
                Err(ClientError::PreconditionFailed)
            );
            assert!(
                matches!(c.put(&request()).await, Err(ClientError::Transient(_))),
                "409"
            );
            assert!(
                matches!(c.put(&request()).await, Err(ClientError::Transient(_))),
                "503"
            );
            assert!(
                matches!(c.put(&request()).await, Err(ClientError::Permanent(_))),
                "403"
            );
            // A missing key is not an error; without s3:ListBucket it would be a 403.
            assert_eq!(c.get("k").await, Ok(None));
            assert!(
                matches!(c.get("k").await, Err(ClientError::Forbidden(_))),
                "403 get"
            );
            assert!(
                matches!(c.get("k").await, Err(ClientError::Transient(_))),
                "500 get"
            );
        }

        #[tokio::test]
        async fn req_pr_2_a_404_with_a_delete_marker_is_not_a_missing_key() {
            let nosuch = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>NoSuchKey</Code>\
                          <Message>m</Message></Error>";
            let c = client(
                canned(vec![
                    (
                        404,
                        vec![("x-amz-delete-marker", "true"), ("x-amz-version-id", "dm1")],
                        nosuch,
                    ),
                    (404, vec![], nosuch),
                ]),
                Seen::default(),
            );
            assert_eq!(c.get("k").await, Err(ClientError::DeleteMarker));
            assert_eq!(c.get("k").await, Ok(None), "a never-written key is None");
        }

        #[tokio::test]
        async fn req_pr_2_a_get_returns_the_bytes_and_the_receipt() {
            let c = client(
                canned(vec![(
                    200,
                    vec![("etag", "\"e1\""), ("x-amz-version-id", "v9")],
                    "held",
                )]),
                Seen::default(),
            );
            let got = c.get("k").await.unwrap().unwrap();
            assert_eq!(got.bytes, b"held");
            assert_eq!(got.etag.as_deref(), Some("\"e1\""));
            assert_eq!(got.version_id.as_deref(), Some("v9"));
        }
    }
}

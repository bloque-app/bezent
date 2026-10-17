// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The authenticated link between the core and the signer (`T-005-02`; `REQ-AR-3`, `REQ-CU-1`).
//!
//! A Unix socket, as aledgent's `F-035` built it: the core and the signer run on one host (or in
//! one task), so mutual TLS buys nothing a socket's peer credentials do not.
//!
//! - **The signer authenticates the caller** twice: by the peer's user id, which the kernel
//!   reports and the caller cannot forge, and by a token whose SHA-256 the configuration lists for
//!   that user id, compared in constant time. The token is the connection's first frame and must
//!   arrive within [`HELLO_TIMEOUT`]. A connection that fails either check is answered
//!   `unauthenticated` and closed before any request is read.
//! - **The caller authenticates the signer** by the peer's user id: [`Client::connect`] refuses a
//!   socket served by any other user.
//! - **The caller and the signer never share a user id** (`REQ-CU-1`, `REQ-AR-3`; review S4): a
//!   process of the signer's user could read its key file, its memory or a core dump. [`Callers::new`]
//!   refuses a caller whose user id is the signer's own, so the signer does not start beside one.
//! - **The caller verifies what the signer returns** (`INV-9`, `AC-I1.2`; review M1):
//!   [`Client::sign`] checks every signature against the recorded intent's exact bytes before it
//!   hands it on; one over other bytes starts the compromise path ([`compromise::returned`]).
//!
//! Frames are a 4-byte big-endian length and a JSON body of at most [`MAX_FRAME`] bytes. The core
//! sends `{"sign": {"intent": <id>}}` and nothing else: the signer reads the rest from the store
//! (`INV-9`).

use std::fmt;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use zeroize::Zeroizing;

use crate::compromise::{self, Answer, Returned};
use crate::config::Caller;
use crate::keys::KeyBackend;
use crate::signer::{Outcome, Signer};

/// The largest frame either side accepts.
pub const MAX_FRAME: usize = 64 * 1024;

/// How long the signer waits for a connection's token.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// What the core sends.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// The connection's first frame: the caller's token. Wiped on drop.
    Hello {
        /// The token.
        token: Zeroizing<String>,
    },
    /// Sign recorded intent `intent`.
    Sign {
        /// The intent id.
        intent: i64,
    },
}

/// What the signer answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// The caller is authenticated.
    Welcome,
    /// The caller is not; the connection closes.
    Unauthenticated,
    /// The intent is signed (now, or earlier and returned).
    Signed {
        /// The intent id.
        intent: i64,
        /// The key id.
        key: String,
        /// The scheme.
        scheme: String,
        /// The public key, `0x` hex.
        public_key: String,
        /// The signature, `0x` hex.
        signature: String,
        /// Whether it was recorded before this request.
        replayed: bool,
    },
    /// A guard refused the intent.
    Refused {
        /// The intent id.
        intent: i64,
        /// The spec id that refuses.
        spec: String,
        /// Why.
        reason: String,
    },
    /// Nothing was signed; try again.
    Unavailable {
        /// The intent id.
        intent: i64,
        /// Why.
        reason: String,
    },
}

impl Response {
    fn of(intent: i64, outcome: Outcome) -> Self {
        match outcome {
            Outcome::Signed(s) => Self::Signed {
                intent,
                key: s.key_id,
                scheme: s.scheme,
                public_key: format!("0x{}", hex::encode(s.public_key)),
                signature: format!("0x{}", hex::encode(s.signature)),
                replayed: s.replayed,
            },
            Outcome::Refused(r) => Self::Refused {
                intent,
                spec: r.spec().to_owned(),
                reason: r.to_string(),
            },
            Outcome::Unavailable(reason) => Self::Unavailable { intent, reason },
        }
    }
}

/// A link failure.
#[derive(Debug)]
pub enum LinkError {
    /// The socket failed.
    Io(io::Error),
    /// The signer refused the caller's credentials.
    Unauthenticated,
    /// The socket is served by another user than the signer's.
    WrongPeer {
        /// The peer's user id.
        uid: Option<u32>,
    },
    /// A frame was too large or not understood.
    Protocol(String),
    /// The signer returned a signature that is not over the recorded intent's bytes. The
    /// compromise path is started; the signature must not be used (`AC-I1.2`, `INV-9`).
    Forged {
        /// The intent requested.
        intent: i64,
        /// The event log position of the alert record.
        alert_seq: i64,
        /// The event log position of the pause request.
        pause_seq: i64,
    },
    /// The store failed while the core checked a returned signature; nothing may be used.
    Store(String),
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "signer link: {e}"),
            Self::Unauthenticated => f.write_str("signer link: unauthenticated (REQ-AR-3)"),
            Self::WrongPeer { uid } => {
                write!(
                    f,
                    "signer link: the socket is served by uid {uid:?}, not the signer's"
                )
            }
            Self::Protocol(why) => write!(f, "signer link: {why}"),
            Self::Forged {
                intent,
                alert_seq,
                pause_seq,
            } => write!(
                f,
                "signer link: the signature returned for intent {intent} is not over its recorded \
                 bytes (AC-I1.2, INV-9); key-compromise at {alert_seq}, pause requested at {pause_seq}"
            ),
            Self::Store(why) => write!(f, "signer link: store: {why}"),
        }
    }
}

impl std::error::Error for LinkError {}

impl From<io::Error> for LinkError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

async fn write_frame<T: Serialize>(stream: &mut UnixStream, body: &T) -> Result<(), LinkError> {
    let bytes = Zeroizing::new(
        serde_json::to_vec(body).map_err(|_| LinkError::Protocol("unencodable frame".into()))?,
    );
    let len = u32::try_from(bytes.len())
        .ok()
        .filter(|&l| l as usize <= MAX_FRAME)
        .ok_or_else(|| LinkError::Protocol("frame too large".into()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(stream: &mut UnixStream) -> Result<T, LinkError> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(LinkError::Protocol("frame too large".into()));
    }
    let mut bytes = Zeroizing::new(vec![0u8; len]);
    stream.read_exact(&mut bytes).await?;
    // serde_json's message could quote the frame (a token); it is not passed on.
    serde_json::from_slice(&bytes).map_err(|_| LinkError::Protocol("frame not understood".into()))
}

/// The callers the signer accepts: user ids and token hashes.
#[derive(Debug, Clone)]
pub struct Callers(Vec<(String, u32, Vec<[u8; 32]>)>);

/// A caller runs as the signer's own user (`REQ-CU-1`, `REQ-AR-3`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedUid {
    /// The caller's name.
    pub caller: String,
    /// The user id both run as.
    pub uid: u32,
}

impl fmt::Display for SharedUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "configuration refused: caller {} runs as uid {}, the signer's own; the signer's key \
             must not be readable by any other process (REQ-CU-1, REQ-AR-3)",
            self.caller, self.uid
        )
    }
}

impl std::error::Error for SharedUid {}

impl Callers {
    /// From the configuration (already checked: every hash is 64 hex digits), for a signer running
    /// as user `signer_uid`. A caller with the signer's own user id is refused.
    pub fn new(callers: &[Caller], signer_uid: u32) -> Result<Self, SharedUid> {
        if let Some(c) = callers.iter().find(|c| c.uid == signer_uid) {
            return Err(SharedUid {
                caller: c.name.clone(),
                uid: c.uid,
            });
        }
        Ok(Self::sharing_the_signer_uid_in_tests(callers))
    }

    /// Like [`Callers::new`], without the user id check. **Tests only**: a test runs the core and
    /// the signer as one user. The signer binary never calls it.
    #[doc(hidden)]
    pub fn sharing_the_signer_uid_in_tests(callers: &[Caller]) -> Self {
        Self(
            callers
                .iter()
                .map(|c| {
                    let hashes = c
                        .token_sha256
                        .iter()
                        .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
                        .collect();
                    (c.name.clone(), c.uid, hashes)
                })
                .collect(),
        )
    }

    /// The name of the caller with user id `uid` and token `token`, if there is one.
    pub fn authenticate(&self, uid: Option<u32>, token: &[u8]) -> Option<&str> {
        let uid = uid?;
        let digest: [u8; 32] = Sha256::digest(token).into();
        let mut found = None;
        for (name, caller_uid, hashes) in &self.0 {
            // Every hash is compared, matched or not.
            let mut matched = subtle::Choice::from(0);
            for hash in hashes {
                matched |= hash.ct_eq(&digest);
            }
            if bool::from(matched) && *caller_uid == uid && found.is_none() {
                found = Some(name.as_str());
            }
        }
        found
    }
}

/// Binds the signer's socket at `path`, replacing a stale socket file. The socket is readable and
/// writable by its owner and group only; the peer checks do the rest.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        std::fs::remove_file(path)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(listener)
}

/// Serves `listener` until it fails.
pub async fn serve<B: KeyBackend>(
    listener: UnixListener,
    callers: Arc<Callers>,
    signer: Arc<Signer<B>>,
) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let (callers, signer) = (callers.clone(), signer.clone());
        tokio::spawn(async move { handle(stream, &callers, &signer).await });
    }
}

async fn handle<B: KeyBackend>(mut stream: UnixStream, callers: &Callers, signer: &Signer<B>) {
    let uid = stream.peer_cred().ok().map(|c| c.uid());
    let hello = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<Request>(&mut stream)).await;
    let caller = match hello {
        Ok(Ok(Request::Hello { token })) => callers.authenticate(uid, token.as_bytes()),
        _ => None,
    };
    let Some(caller) = caller else {
        signer.log().line(format_args!(
            "refused connection uid={uid:?} REQ-AR-3: unauthenticated"
        ));
        let _ = write_frame(&mut stream, &Response::Unauthenticated).await;
        return;
    };
    if write_frame(&mut stream, &Response::Welcome).await.is_err() {
        return;
    }
    loop {
        match read_frame::<Request>(&mut stream).await {
            Ok(Request::Sign { intent }) => {
                let outcome = signer.sign(intent).await;
                if write_frame(&mut stream, &Response::of(intent, outcome))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(Request::Hello { .. }) | Err(LinkError::Protocol(_)) => {
                signer.log().line(format_args!(
                    "closed connection caller={caller}: not a sign request"
                ));
                return;
            }
            Err(_) => return,
        }
    }
}

/// The core's end of the link.
#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
}

impl Client {
    /// Connects to the signer at `path`, served by user `signer_uid`, and presents `token`.
    pub async fn connect(path: &Path, token: &str, signer_uid: u32) -> Result<Self, LinkError> {
        let mut stream = UnixStream::connect(path).await?;
        let uid = stream.peer_cred().ok().map(|c| c.uid());
        if uid != Some(signer_uid) {
            return Err(LinkError::WrongPeer { uid });
        }
        let hello = Request::Hello {
            token: Zeroizing::new(token.to_owned()),
        };
        write_frame(&mut stream, &hello).await?;
        match read_frame::<Response>(&mut stream).await? {
            Response::Welcome => Ok(Self { stream }),
            Response::Unauthenticated => Err(LinkError::Unauthenticated),
            _ => Err(LinkError::Protocol("unexpected answer to hello".into())),
        }
    }

    /// Asks the signer to sign recorded intent `intent`, and verifies a returned signature against
    /// the intent's recorded bytes, read through `core` (the service role), before returning it
    /// (`INV-9`). A signature over anything else starts the compromise path and is never returned:
    /// the answer is [`LinkError::Forged`].
    pub async fn sign(&mut self, core: &PgPool, intent: i64) -> Result<Response, LinkError> {
        let response = self.request(intent).await?;
        if let Response::Signed {
            intent: answered,
            key,
            scheme,
            public_key,
            signature,
            ..
        } = &response
        {
            let answer = Answer {
                intent: *answered,
                key,
                scheme,
                public_key,
                signature,
            };
            match compromise::returned(core, intent, answer).await {
                Ok(Returned::Verified) => {}
                Ok(Returned::Compromise {
                    alert_seq,
                    pause_seq,
                }) => {
                    return Err(LinkError::Forged {
                        intent,
                        alert_seq,
                        pause_seq,
                    });
                }
                Err(e) => return Err(LinkError::Store(e.to_string())),
            }
        }
        Ok(response)
    }

    /// Asks the signer to sign recorded intent `intent`, and returns its answer **unverified**:
    /// for tests of the signer. The core uses [`Client::sign`].
    #[doc(hidden)]
    pub async fn request(&mut self, intent: i64) -> Result<Response, LinkError> {
        write_frame(&mut self.stream, &Request::Sign { intent }).await?;
        read_frame(&mut self.stream).await
    }

    /// Sends a raw request: for tests of what the signer refuses.
    pub async fn raw(stream: &mut UnixStream, request: &Request) -> Result<Response, LinkError> {
        write_frame(stream, request).await?;
        read_frame(stream).await
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The authenticated link between the core and the signer (`T-005-02`; `REQ-AR-3`, `REQ-CU-1`).
//!
//! Two transports carry the same frames (`transport = "unix" | "tls"`):
//!
//! - **A Unix socket**, as aledgent's `F-035` built it: the caller and the signer run on one host
//!   (or in one task, the signer a sidecar of its caller: `prepare`), so a socket's peer
//!   credentials are enough. For local and e2e use.
//! - **Mutual TLS over TCP** ([`crate::tls`], `PUB-1b`): the signer is a task of its own that the
//!   callers (core, kreivo-endpoint, evm-endpoint) reach across the network. The signer presents
//!   its certificate and requires one that chains to the link's root; the caller's identity is
//!   the one DNS name in that certificate ([`Caller::dns_name`](crate::config::Caller)); the
//!   caller verifies that the server's certificate carries exactly the signer's name.
//!
//! Whichever the transport, the signer also:
//!
//! - **authorises the caller per intent** (`REQ-AR-3`): a caller whose configuration lists what it
//!   may ask for (`allow`: a network and purposes) is refused any other intent before the signer
//!   looks at it, so one caller can neither mint through another's lane nor sign on a network it
//!   does not serve. A caller with no list asks for anything (the sidecar, which has one caller).
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio_rustls::TlsAcceptor;
use zeroize::Zeroizing;

use crate::compromise::{self, Answer, Returned};
use crate::config::{Caller, Transport};
use crate::keys::SigningEngine;
use crate::signer::{Outcome, Signer};
use crate::tls::{self, ClientLink, DialTls};

/// The largest frame either side accepts.
pub const MAX_FRAME: usize = 64 * 1024;

/// How long the signer waits for a connection's token.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest a caller spends reaching the signer: the connection, the TLS handshake and the
/// hello that is answered with a welcome. The signer bounds its own end with [`HELLO_TIMEOUT`],
/// but a peer that never answers (a blackholed address, a stopped process that still holds the
/// socket) would otherwise hold the caller for good, and with it the endpoint's whole pass.
///
/// It ends before any `Sign` frame is written, so a dial that times out asked the signer for
/// nothing: the intent is plainly not signed and is asked for again. The wait for the answer to a
/// `Sign` is not bounded here: a request that was written may have been signed.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

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
                 bytes (AC-I1.2, INV-9); key-compromise at {alert_seq}, pause recorded at {pause_seq}"
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

async fn write_frame<S: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut S,
    body: &T,
) -> Result<(), LinkError> {
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

async fn read_frame<S: AsyncRead + Unpin, T: for<'de> Deserialize<'de>>(
    stream: &mut S,
) -> Result<T, LinkError> {
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

/// One configured caller, resolved.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    uid: Option<u32>,
    dns_name: Option<String>,
    hashes: Vec<[u8; 32]>,
    /// What it may ask for: `None` is anything.
    allow: Option<Vec<(String, Vec<String>)>>,
}

/// The callers the signer accepts: how each is recognised (a peer user id on the socket, a
/// certificate's DNS name over TLS), the SHA-256 of its tokens, and what it may ask for.
#[derive(Debug, Clone)]
pub struct Callers(Vec<Entry>);

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
    /// as user `signer_uid`. A socket caller with the signer's own user id is refused; a TLS caller
    /// has no user id.
    pub fn new(callers: &[Caller], signer_uid: u32) -> Result<Self, SharedUid> {
        if let Some(c) = callers.iter().find(|c| c.uid == Some(signer_uid)) {
            return Err(SharedUid {
                caller: c.name.clone(),
                uid: signer_uid,
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
                .map(|c| Entry {
                    name: c.name.clone(),
                    uid: c.uid,
                    dns_name: c.dns_name.as_ref().map(|n| n.to_ascii_lowercase()),
                    hashes: c
                        .token_sha256
                        .iter()
                        .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
                        .collect(),
                    allow: c.allow.as_ref().map(|list| {
                        list.iter()
                            .map(|a| (a.network.clone(), a.purposes.clone()))
                            .collect()
                    }),
                })
                .collect(),
        )
    }

    /// The first entry `is` accepts whose token is `token`. Every hash of every entry is compared
    /// in constant time, matched or not.
    fn find(&self, is: impl Fn(&Entry) -> bool, token: &[u8]) -> Option<&str> {
        let digest: [u8; 32] = Sha256::digest(token).into();
        let mut found = None;
        for entry in &self.0 {
            let mut matched = subtle::Choice::from(0);
            for hash in &entry.hashes {
                matched |= hash.ct_eq(&digest);
            }
            if bool::from(matched) && is(entry) && found.is_none() {
                found = Some(entry.name.as_str());
            }
        }
        found
    }

    /// The name of the caller with user id `uid` and token `token`, if there is one (the socket).
    pub fn authenticate(&self, uid: Option<u32>, token: &[u8]) -> Option<&str> {
        let uid = uid?;
        self.find(|e| e.uid == Some(uid), token)
    }

    /// The name of the caller whose certificate carries the DNS name `dns` and whose token is
    /// `token`, if there is one (TLS). The certificate was verified by the handshake; the name
    /// must be exactly a configured caller's.
    pub fn authenticate_dns(&self, dns: Option<&str>, token: &[u8]) -> Option<&str> {
        let dns = dns?.to_ascii_lowercase();
        self.find(|e| e.dns_name.as_deref() == Some(dns.as_str()), token)
    }

    /// Whether caller `name` may ask for an intent of `purpose` on `network` (`REQ-AR-3`): a caller
    /// with no `allow` list may ask for anything, one with a list only for what it names (`*` as a
    /// network is any network).
    pub fn permits(&self, name: &str, network: &str, purpose: &str) -> bool {
        self.0
            .iter()
            .find(|e| e.name == name)
            .is_some_and(|e| match &e.allow {
                None => true,
                Some(list) => list.iter().any(|(n, purposes)| {
                    (n == "*" || n == network) && purposes.iter().any(|p| p == purpose)
                }),
            })
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

/// Who a connection says it is, as the transport established it.
enum Who {
    /// The socket's peer user id.
    Uid(Option<u32>),
    /// The DNS name of the certificate the handshake verified.
    Dns(Option<String>),
}

impl fmt::Display for Who {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uid(uid) => write!(f, "uid={uid:?}"),
            Self::Dns(name) => write!(f, "name={name:?}"),
        }
    }
}

/// Serves `listener` (the Unix socket) until it fails.
pub async fn serve<B: SigningEngine>(
    listener: UnixListener,
    callers: Arc<Callers>,
    signer: Arc<Signer<B>>,
) -> io::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let (callers, signer) = (callers.clone(), signer.clone());
        tokio::spawn(async move {
            let who = Who::Uid(stream.peer_cred().ok().map(|c| c.uid()));
            handle(stream, who, &callers, &signer).await;
        });
    }
}

/// Serves mutual TLS on `listener` until it fails: `acceptor` holds the signer's identity and
/// requires a client certificate that chains to the link's root (`PUB-1b`). A connection whose
/// handshake fails, or does not finish within [`HELLO_TIMEOUT`], is dropped.
pub async fn serve_tls<B: SigningEngine>(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    callers: Arc<Callers>,
    signer: Arc<Signer<B>>,
) -> io::Result<()> {
    loop {
        let (tcp, _) = listener.accept().await?;
        let (acceptor, callers, signer) = (acceptor.clone(), callers.clone(), signer.clone());
        tokio::spawn(async move {
            let handshake = tokio::time::timeout(HELLO_TIMEOUT, acceptor.accept(tcp)).await;
            let stream = match handshake {
                Ok(Ok(stream)) => stream,
                // A connection closed before it said anything (the container health check opens
                // one and drops it) is not a refusal worth a line.
                Ok(Err(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return,
                _ => {
                    signer.log().line(format_args!(
                        "refused connection REQ-AR-3: no valid client certificate"
                    ));
                    return;
                }
            };
            let who = Who::Dns(tls::peer_name(stream.get_ref().1.peer_certificates()));
            handle(stream, who, &callers, &signer).await;
        });
    }
}

async fn handle<S, B>(mut stream: S, who: Who, callers: &Callers, signer: &Signer<B>)
where
    S: AsyncRead + AsyncWrite + Unpin,
    B: SigningEngine,
{
    let hello = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, Request>(&mut stream)).await;
    let caller = match (hello, &who) {
        (Ok(Ok(Request::Hello { token })), Who::Uid(uid)) => {
            callers.authenticate(*uid, token.as_bytes())
        }
        (Ok(Ok(Request::Hello { token })), Who::Dns(name)) => {
            callers.authenticate_dns(name.as_deref(), token.as_bytes())
        }
        _ => None,
    };
    let Some(caller) = caller else {
        signer.log().line(format_args!(
            "refused connection {who} REQ-AR-3: unauthenticated"
        ));
        let _ = write_frame(&mut stream, &Response::Unauthenticated).await;
        return;
    };
    if write_frame(&mut stream, &Response::Welcome).await.is_err() {
        return;
    }
    loop {
        match read_frame::<_, Request>(&mut stream).await {
            Ok(Request::Sign { intent }) => {
                let response = match signer.scope_of(intent).await {
                    Err(reason) => Response::Unavailable { intent, reason },
                    Ok(Some((network, purpose)))
                        if !callers.permits(caller, &network, &purpose) =>
                    {
                        signer.log().line(format_args!(
                            "refused intent={intent} caller={caller} REQ-AR-3: not allowed to ask \
                             for {purpose} on {network}"
                        ));
                        Response::Refused {
                            intent,
                            spec: "REQ-AR-3".to_owned(),
                            reason: format!(
                                "caller {caller} may not ask for {purpose} on {network}"
                            ),
                        }
                    }
                    // An unrecorded intent is the signer's own refusal to make.
                    Ok(_) => Response::of(intent, signer.sign(intent).await),
                };
                if write_frame(&mut stream, &response).await.is_err() {
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

/// A byte stream the link runs over.
trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// `dial`, which must end within `within`: a dial that does not is [`LinkError::Io`] of kind
/// `TimedOut`, like any other failure to reach the signer.
async fn dialed(
    within: Duration,
    dial: impl std::future::Future<Output = Result<Client, LinkError>>,
) -> Result<Client, LinkError> {
    tokio::time::timeout(within, dial)
        .await
        .unwrap_or_else(|_| {
            Err(LinkError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "the signer did not answer in time",
            )))
        })
}

/// The core's end of the link.
pub struct Client {
    stream: Box<dyn Duplex>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Client")
    }
}

/// How a caller reaches the signer: the socket (`transport = "unix"`) or mutual TLS
/// (`transport = "tls"`), built once from a service's configuration and used for every request.
#[derive(Clone, Debug)]
pub enum Dialer {
    /// The signer's Unix socket and the user id that must serve it.
    Unix {
        /// The socket.
        socket: std::path::PathBuf,
        /// The signer's user id.
        uid: u32,
    },
    /// The signer over mutual TLS.
    Tls(ClientLink),
}

impl Dialer {
    /// Checks a caller's `transport` settings, touching nothing: a socket needs `socket` and
    /// `uid` and no `tls`, TLS needs `tls` and the Vault login table (`has_vault`) its
    /// certificate is issued under, and no socket or uid.
    pub fn check(
        transport: Transport,
        socket: Option<&Path>,
        uid: Option<u32>,
        tls: Option<&DialTls>,
        has_vault: bool,
    ) -> Result<(), String> {
        match transport {
            Transport::Unix => {
                if socket.is_none() || uid.is_none() {
                    return Err("transport unix needs signer socket and uid".into());
                }
                if tls.is_some() {
                    return Err("a tls table is set but the signer transport is unix".into());
                }
            }
            Transport::Tls => {
                if socket.is_some() || uid.is_some() {
                    return Err("socket and uid are set but the signer transport is tls".into());
                }
                if tls.is_none() {
                    return Err("transport tls needs a tls table".into());
                }
                if !has_vault {
                    return Err(
                        "transport tls issues the certificate from Vault, so needs the Vault \
                         login table"
                            .into(),
                    );
                }
            }
        }
        Ok(())
    }

    /// The dialer a configuration names. Over TLS it issues this process's certificate from Vault
    /// now (a Vault that does not answer fails the start) and renews it in the background.
    pub async fn build(
        transport: Transport,
        socket: Option<&Path>,
        uid: Option<u32>,
        tls: Option<&DialTls>,
        vault: Option<&bezent_vault::VaultConfig>,
        log: crate::log::Log,
    ) -> Result<Self, String> {
        Self::check(transport, socket, uid, tls, vault.is_some())?;
        match (transport, socket, uid, tls, vault) {
            (Transport::Unix, Some(socket), Some(uid), _, _) => Ok(Self::Unix {
                socket: socket.to_path_buf(),
                uid,
            }),
            (Transport::Tls, _, _, Some(tls), Some(vault)) => tls
                .link(vault, log)
                .await
                .map(Self::Tls)
                .map_err(|e| e.to_string()),
            _ => Err("the signer link is not configured".into()),
        }
    }

    /// Connects and presents `token`.
    pub async fn connect(&self, token: &str) -> Result<Client, LinkError> {
        match self {
            Self::Unix { socket, uid } => Client::connect(socket, token, *uid).await,
            Self::Tls(link) => Client::connect_tls(link, token).await,
        }
    }
}

impl Client {
    /// Connects to the signer at `path`, served by user `signer_uid`, and presents `token`.
    pub async fn connect(path: &Path, token: &str, signer_uid: u32) -> Result<Self, LinkError> {
        Self::connect_within(path, token, signer_uid, DIAL_TIMEOUT).await
    }

    /// [`Client::connect`], giving up after `within` ([`DIAL_TIMEOUT`] is the bound callers use).
    pub async fn connect_within(
        path: &Path,
        token: &str,
        signer_uid: u32,
        within: Duration,
    ) -> Result<Self, LinkError> {
        let dial = async {
            let stream = UnixStream::connect(path).await?;
            let uid = stream.peer_cred().ok().map(|c| c.uid());
            if uid != Some(signer_uid) {
                return Err(LinkError::WrongPeer { uid });
            }
            let hello = Request::Hello {
                token: Zeroizing::new(token.to_owned()),
            };
            Self::hello(Box::new(stream), hello).await
        };
        dialed(within, dial).await
    }

    /// Connects over mutual TLS to the signer `link` names, which must present a certificate for
    /// exactly that name, and presents `token`. A refused client certificate shows as an error
    /// here: TLS 1.3 reports it on the first read.
    pub async fn connect_tls(link: &ClientLink, token: &str) -> Result<Self, LinkError> {
        Self::connect_tls_within(link, token, DIAL_TIMEOUT).await
    }

    /// [`Client::connect_tls`], giving up after `within` ([`DIAL_TIMEOUT`] is the bound callers
    /// use).
    pub async fn connect_tls_within(
        link: &ClientLink,
        token: &str,
        within: Duration,
    ) -> Result<Self, LinkError> {
        let dial = async {
            let tcp = TcpStream::connect(&link.address).await?;
            let connector = tokio_rustls::TlsConnector::from(link.config.clone());
            let stream = connector
                .connect(link.server_name.clone(), tcp)
                .await
                .map_err(LinkError::Io)?;
            let hello = Request::Hello {
                token: Zeroizing::new(token.to_owned()),
            };
            Self::hello(Box::new(stream), hello).await
        };
        dialed(within, dial).await
    }

    async fn hello(mut stream: Box<dyn Duplex>, hello: Request) -> Result<Self, LinkError> {
        write_frame(&mut stream, &hello).await?;
        match read_frame::<_, Response>(&mut stream).await? {
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
    pub async fn raw<S: AsyncRead + AsyncWrite + Unpin>(
        stream: &mut S,
        request: &Request,
    ) -> Result<Response, LinkError> {
        write_frame(stream, request).await?;
        read_frame(stream).await
    }
}

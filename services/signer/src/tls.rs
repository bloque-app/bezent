// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The mutual TLS of the signer link (`PUB-1b`; `REQ-AR-3`, `REQ-CU-1`): certificates from Vault's
//! PKI, held and renewed in the process, and the rustls configurations built on them.
//!
//! - **One identity per process.** An [`Identity`] holds the process's current certificate and
//!   key. It is issued at start (a Vault that does not answer then is a start that fails), and a
//!   background task issues the next when about a third of the lifetime is left. The new
//!   certificate is swapped in under the listener or the dialer: new handshakes use it, nothing is
//!   dropped, and the configurations are built once. A renewal that fails (a Vault outage) is
//!   logged and retried, and the process keeps serving with the certificate it holds.
//! - **Issuing** goes through the [`Issuer`] trait. [`VaultIssuer`] logs in with the task role
//!   (a fresh login for each issue: the tokens last a minute) and asks `<mount>/issue/<role>` for
//!   the name, with the lifetime the role allows; tests give a fake.
//! - **The root is pinned from a file** (`ca_file`), never taken from what Vault returns with a
//!   certificate. The leaf and the chain above it are presented, so the peer needs the root only.
//! - **TLS 1.3 only, on ring.** The server requires a client certificate that chains to the root
//!   and carries the client usage; the client verifies the server's certificate against the root
//!   and against the one name it expects ([`ClientLink::server_name`]), which the certificate must
//!   carry exactly.
//! - **Who the caller is** comes from the verified certificate: [`peer_name`] reads its one DNS
//!   name, which the signer maps to a configured caller.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, SystemTime};

use bezent_vault::{Client, IssueRequest, Issued, Usage, VaultConfig};
use rustls::client::ResolvesClientCert;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::{ResolvesServerCert, WebPkiClientVerifier};
use rustls::sign::CertifiedKey;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use serde::Deserialize;
use tokio::task::JoinHandle;

use crate::log::Log;

/// How long to wait before asking again after a failed renewal.
pub const RETRY: Duration = Duration::from_secs(30);

/// The lifetime asked of the role: exactly what it allows, since a certificate shorter than asked
/// is refused as a mistake ([`bezent_vault::pki`]).
pub const TTL_SECONDS: u64 = 2_592_000;

/// A failure to set up or renew the link's identity. Never carries a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsError(pub String);

impl fmt::Display for TlsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "signer link TLS: {}", self.0)
    }
}

impl std::error::Error for TlsError {}

fn fail<T>(why: impl Into<String>) -> Result<T, TlsError> {
    Err(TlsError(why.into()))
}

/// Where a process's link certificate comes from: the PKI mount and the role (`pki-bezent-link`,
/// `core`), and the one name the role issues.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PkiConfig {
    /// The PKI engine's mount, for example `pki-bezent-link`.
    pub mount: String,
    /// The PKI role, for example `signer` or `core`.
    pub role: String,
    /// The name the role issues, the certificate's one DNS name, for example
    /// `core.link.bezent.prod.bloque.network`.
    pub common_name: String,
}

type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Where certificates come from.
pub trait Issuer: Send + Sync + 'static {
    /// A new certificate with its key. Fails with words, never with a key.
    fn issue(&self) -> Boxed<'_, Result<Issued, TlsError>>;
}

/// Vault's PKI, under the process's task role.
pub struct VaultIssuer {
    client: Client,
    pki: PkiConfig,
    usage: Usage,
}

impl VaultIssuer {
    /// An issuer for `pki`: certificates for a server (`Usage::Server`) or a client.
    pub fn new(vault: &VaultConfig, pki: &PkiConfig, usage: Usage) -> Result<Self, TlsError> {
        let client = Client::new(vault).map_err(|e| TlsError(e.to_string()))?;
        Ok(Self {
            client,
            pki: pki.clone(),
            usage,
        })
    }
}

impl Issuer for VaultIssuer {
    fn issue(&self) -> Boxed<'_, Result<Issued, TlsError>> {
        Box::pin(async move {
            let session = self
                .client
                .login()
                .await
                .map_err(|e| TlsError(e.to_string()))?;
            let request = IssueRequest {
                common_name: self.pki.common_name.clone(),
                ttl: Duration::from_secs(TTL_SECONDS),
                usage: self.usage,
            };
            session
                .issue(&self.pki.mount, &self.pki.role, &request)
                .await
                .map_err(|e| TlsError(e.to_string()))
        })
    }
}

/// The ring provider, named explicitly: the process also links aws-lc-rs, so there is no
/// process-wide default to rely on.
fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn certified(issued: &Issued) -> Result<CertifiedKey, TlsError> {
    let mut chain = vec![CertificateDer::from(issued.certificate_der.clone())];
    for pem in &issued.ca_chain_pem {
        chain.extend(pem_certificates(pem.as_bytes())?);
    }
    let key = PrivateKeyDer::try_from(issued.private_key_pkcs8.to_vec())
        .map_err(|_| TlsError("the private key is not usable".into()))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|_| TlsError("the private key is of an unsupported kind".into()))?;
    Ok(CertifiedKey::new(chain, signing))
}

/// The certificates in `pem`, in order.
pub fn pem_certificates(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    use rustls::pki_types::pem::PemObject;
    CertificateDer::pem_slice_iter(pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| TlsError("a certificate is not PEM".into()))
}

/// The roots in the PEM file at `path`: the trust anchor, pinned from configuration.
pub fn load_roots(path: &Path) -> Result<RootCertStore, TlsError> {
    let pem = std::fs::read(path)
        .map_err(|e| TlsError(format!("cannot read {}: {}", path.display(), e.kind())))?;
    roots_from_pem(&pem)
}

/// The roots in `pem`.
pub fn roots_from_pem(pem: &[u8]) -> Result<RootCertStore, TlsError> {
    let mut roots = RootCertStore::empty();
    let certificates = pem_certificates(pem)?;
    if certificates.is_empty() {
        return fail("the root file holds no certificate");
    }
    for certificate in certificates {
        roots
            .add(certificate)
            .map_err(|_| TlsError("the root certificate is not usable".into()))?;
    }
    Ok(roots)
}

struct Current {
    key: Arc<CertifiedKey>,
    serial: String,
    not_after: SystemTime,
}

/// The process's certificate and key, replaced in the background before it expires. It is what
/// the rustls configurations ask for a certificate at each handshake.
pub struct Identity {
    current: RwLock<Current>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let current = self.current.read().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("Identity")
            .field("serial", &current.serial)
            .field("not_after", &current.not_after)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// Issues the first certificate and starts renewing it. A failure to issue now is an error:
    /// a process that cannot have a certificate does not start. After that, a failed renewal is
    /// retried every `retry` and the certificate in hand keeps serving.
    pub async fn start(
        issuer: Arc<dyn Issuer>,
        retry: Duration,
        log: Log,
    ) -> Result<Arc<Self>, TlsError> {
        let issued = issuer.issue().await?;
        let (key, serial, not_after) =
            (certified(&issued)?, issued.serial_number, issued.not_after);
        let renew_at = renewal_time(SystemTime::now(), not_after);
        let identity = Arc::new(Self {
            current: RwLock::new(Current {
                key: Arc::new(key),
                serial,
                not_after,
            }),
            task: Mutex::new(None),
        });
        let handle = tokio::spawn(renew(
            Arc::downgrade(&identity),
            issuer,
            retry,
            log,
            renew_at,
        ));
        *identity.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(identity)
    }

    /// The serial number of the certificate in use.
    pub fn serial(&self) -> String {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .serial
            .clone()
    }

    /// When the certificate in use expires.
    pub fn not_after(&self) -> SystemTime {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .not_after
    }

    fn key(&self) -> Arc<CertifiedKey> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .key
            .clone()
    }

    fn replace(&self, issued: &Issued) -> Result<(), TlsError> {
        let key = Arc::new(certified(issued)?);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = Current {
            key,
            serial: issued.serial_number.clone(),
            not_after: issued.not_after,
        };
        Ok(())
    }
}

impl Drop for Identity {
    fn drop(&mut self) {
        if let Some(handle) = self
            .task
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            handle.abort();
        }
    }
}

impl ResolvesServerCert for Identity {
    fn resolve(&self, _hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.key())
    }
}

impl ResolvesClientCert for Identity {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        Some(self.key())
    }

    fn has_certs(&self) -> bool {
        true
    }
}

/// When to renew a certificate issued at `now` that expires at `not_after`: with a third of its
/// lifetime left.
pub fn renewal_time(now: SystemTime, not_after: SystemTime) -> SystemTime {
    let life = not_after.duration_since(now).unwrap_or_default();
    now + life * 2 / 3
}

async fn renew(
    identity: Weak<Identity>,
    issuer: Arc<dyn Issuer>,
    retry: Duration,
    log: Log,
    mut renew_at: SystemTime,
) {
    loop {
        let wait = renew_at
            .duration_since(SystemTime::now())
            .unwrap_or_default();
        tokio::time::sleep(wait).await;
        let Some(strong) = identity.upgrade() else {
            return;
        };
        drop(strong);
        let outcome = match issuer.issue().await {
            Ok(issued) => match identity.upgrade() {
                Some(identity) => identity.replace(&issued).map(|()| issued.not_after),
                None => return,
            },
            Err(e) => Err(e),
        };
        match outcome {
            Ok(not_after) => {
                renew_at = renewal_time(SystemTime::now(), not_after);
                log.line(format_args!("signer link: certificate renewed"));
            }
            Err(e) => {
                // The certificate in hand keeps serving until it expires.
                log.line(format_args!(
                    "signer link: renewal failed, retrying in {}s: {e}",
                    retry.as_secs()
                ));
                renew_at = SystemTime::now() + retry;
            }
        }
    }
}

/// The server's configuration: its identity, and a required client certificate that chains to
/// `roots`.
pub fn server_config(
    identity: Arc<Identity>,
    roots: RootCertStore,
) -> Result<Arc<ServerConfig>, TlsError> {
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .map_err(|_| TlsError("the client verifier could not be built".into()))?;
    let config = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|_| TlsError("TLS 1.3 is not available".into()))?
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(identity);
    Ok(Arc::new(config))
}

/// A caller's end of the link: the configuration, the address to dial and the one name the
/// server's certificate must carry.
#[derive(Clone)]
pub struct ClientLink {
    /// `host:port` to dial.
    pub address: String,
    /// The name the server's certificate must carry, exactly.
    pub server_name: ServerName<'static>,
    /// The caller's configuration.
    pub config: Arc<ClientConfig>,
}

impl fmt::Debug for ClientLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientLink")
            .field("address", &self.address)
            .field("server_name", &self.server_name)
            .finish_non_exhaustive()
    }
}

impl ClientLink {
    /// A link to the server at `address`, which must be `server_name`, trusting `roots` and
    /// presenting `identity`.
    pub fn new(
        address: &str,
        server_name: &str,
        identity: Arc<Identity>,
        roots: RootCertStore,
    ) -> Result<Self, TlsError> {
        let name = ServerName::try_from(server_name.to_owned())
            .map_err(|_| TlsError("server_name is not a DNS name".into()))?;
        if !matches!(name, ServerName::DnsName(_)) {
            return fail("server_name is not a DNS name");
        }
        let config = ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|_| TlsError("TLS 1.3 is not available".into()))?
            .with_root_certificates(roots)
            .with_client_cert_resolver(identity);
        Ok(Self {
            address: address.to_owned(),
            server_name: name,
            config: Arc::new(config),
        })
    }
}

/// The one DNS name of the certificate a peer presented, after the handshake verified it.
pub fn peer_name(certificates: Option<&[CertificateDer<'_>]>) -> Option<String> {
    bezent_vault::pki::sole_dns_name(certificates?.first()?)
}

/// A caller's TLS settings, as the callers' configurations spell them (`[signer.tls]`): where to
/// dial, the server's name, the pinned root, and where its own certificate comes from.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DialTls {
    /// `host:port` of the signer.
    pub address: String,
    /// The name the signer's certificate must carry, for example
    /// `signer.bezent.prod.bloque.network`.
    pub server_name: String,
    /// The PEM file of the root (`pki-bezent-root`).
    pub ca_file: std::path::PathBuf,
    /// This caller's certificate.
    pub pki: PkiConfig,
}

impl DialTls {
    /// Makes the relative `ca_file` relative to `base`.
    pub fn rebase(&mut self, base: &Path) {
        if self.ca_file.is_relative() {
            self.ca_file = base.join(&self.ca_file);
        }
    }

    /// Issues this caller's certificate (Vault, under the task role), starts renewing it and
    /// returns the link.
    pub async fn link(&self, vault: &VaultConfig, log: Log) -> Result<ClientLink, TlsError> {
        let issuer = Arc::new(VaultIssuer::new(vault, &self.pki, Usage::Client)?);
        self.link_with(issuer, RETRY, log).await
    }

    /// As [`DialTls::link`], with the given issuer.
    pub async fn link_with(
        &self,
        issuer: Arc<dyn Issuer>,
        retry: Duration,
        log: Log,
    ) -> Result<ClientLink, TlsError> {
        let roots = load_roots(&self.ca_file)?;
        let identity = Identity::start(issuer, retry, log).await?;
        ClientLink::new(&self.address, &self.server_name, identity, roots)
    }
}

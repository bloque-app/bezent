// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The mutual TLS signer link (`PUB-1b`; `REQ-AR-3`): an unauthenticated caller is refused, a caller
//! whose certificate is for another name is refused the intents it may not ask for, a server whose
//! certificate is for another name is refused by the caller, and a renewed certificate is swapped in
//! under a live listener. Certificates are made in the process (a root, an intermediate, leaves);
//! nothing here talks to Vault.

mod common;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bezent_signer::config::{Allow, Caller};
use bezent_signer::link::{self, Callers, Client, LinkError, Request, Response};
use bezent_signer::log::Log;
use bezent_signer::tls::{
    ClientLink, Identity, Issuer, TlsError, peer_name, roots_from_pem, server_config,
};
use bezent_vault::{Issued, Usage, Zeroizing};
use common::{Fixture, TOKEN};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    KeyPair, KeyUsagePurpose,
};
use sha2::{Digest, Sha256};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::net::TcpListener;
use tokio_rustls::{TlsAcceptor, TlsConnector};

const SIGNER: &str = "signer.bezent.test.example";
const CORE: &str = "core.link.bezent.test.example";
const EVM: &str = "evm-endpoint.link.bezent.test.example";

/// A root and an intermediate, as `pki-bezent-root` and `pki-bezent-link`.
struct Authority {
    root: (Certificate, KeyPair),
    link: (Certificate, KeyPair),
}

impl Authority {
    fn new() -> Self {
        let ca = |name: &str, issuer: Option<&(Certificate, KeyPair)>| {
            let mut params = CertificateParams::new(vec![]).unwrap();
            params.distinguished_name.push(DnType::CommonName, name);
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let key = KeyPair::generate().unwrap();
            let cert = match issuer {
                Some((c, k)) => params.signed_by(&key, c, k).unwrap(),
                None => params.self_signed(&key).unwrap(),
            };
            (cert, key)
        };
        let root = ca("test root", None);
        let link = ca("test link", Some(&root));
        Self { root, link }
    }

    fn root_pem(&self) -> String {
        self.root.0.pem()
    }

    /// A leaf for `name` (its one DNS name), for a server or a client, that Vault would call
    /// `serial`, and whose renewal clock says it expires `lives` from now.
    fn issue(&self, name: &str, usage: Usage, serial: &str, lives: Duration) -> Issued {
        let mut params = CertificateParams::new(vec![name.to_owned()]).unwrap();
        params.distinguished_name.push(DnType::CommonName, name);
        params.extended_key_usages = vec![match usage {
            Usage::Server => ExtendedKeyUsagePurpose::ServerAuth,
            Usage::Client => ExtendedKeyUsagePurpose::ClientAuth,
        }];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        let key = KeyPair::generate().unwrap();
        let cert = params.signed_by(&key, &self.link.0, &self.link.1).unwrap();
        Issued {
            certificate_pem: cert.pem(),
            certificate_der: cert.der().to_vec(),
            ca_chain_pem: vec![self.link.0.pem(), self.root.0.pem()],
            private_key_pkcs8: Zeroizing::new(key.serialize_der()),
            serial_number: serial.to_owned(),
            not_after: SystemTime::now() + lives,
        }
    }
}

/// Vault's PKI role, made in the process: a new certificate for one name at each call, until told
/// to fail (a Vault outage).
struct FakeIssuer {
    authority: Arc<Authority>,
    name: String,
    usage: Usage,
    lives: Duration,
    count: AtomicU32,
    down: AtomicBool,
    der: Mutex<Vec<Vec<u8>>>,
}

impl FakeIssuer {
    fn new(authority: &Arc<Authority>, name: &str, usage: Usage, lives: Duration) -> Arc<Self> {
        Arc::new(Self {
            authority: authority.clone(),
            name: name.to_owned(),
            usage,
            lives,
            count: AtomicU32::new(0),
            down: AtomicBool::new(false),
            der: Mutex::new(Vec::new()),
        })
    }

    fn issued(&self) -> Vec<Vec<u8>> {
        self.der.lock().unwrap().clone()
    }
}

impl Issuer for FakeIssuer {
    fn issue(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Issued, TlsError>> + Send + '_>>
    {
        Box::pin(async move {
            if self.down.load(Ordering::SeqCst) {
                return Err(TlsError("Vault did not answer".into()));
            }
            let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
            let issued =
                self.authority
                    .issue(&self.name, self.usage, &format!("serial-{n}"), self.lives);
            self.der
                .lock()
                .unwrap()
                .push(issued.certificate_der.clone());
            Ok(issued)
        })
    }
}

const LONG: Duration = Duration::from_secs(3600);
const RETRY: Duration = Duration::from_millis(200);

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

async fn identity(issuer: &Arc<FakeIssuer>) -> Arc<Identity> {
    Identity::start(issuer.clone(), RETRY, Log::capture().0)
        .await
        .unwrap()
}

fn caller(name: &str, dns: &str, allow: &[(&str, &[&str])]) -> Caller {
    Caller {
        name: name.into(),
        uid: None,
        dns_name: Some(dns.into()),
        token_sha256: vec![hex::encode(Sha256::digest(TOKEN.as_bytes()))],
        allow: Some(
            allow
                .iter()
                .map(|(network, purposes)| Allow {
                    network: (*network).into(),
                    purposes: purposes.iter().map(|p| (*p).into()).collect(),
                })
                .collect(),
        ),
    }
}

/// The signer's TLS listener on a free port, with certificate for `server_name`, serving
/// `callers`.
async fn listen(
    f: &Fixture,
    authority: &Arc<Authority>,
    server_name: &str,
    callers: &[Caller],
) -> SocketAddr {
    let issuer = FakeIssuer::new(authority, server_name, Usage::Server, LONG);
    let roots = roots_from_pem(authority.root_pem().as_bytes()).unwrap();
    let config = server_config(identity(&issuer).await, roots).unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    tokio::spawn(link::serve_tls(
        tcp,
        TlsAcceptor::from(config),
        Arc::new(Callers::sharing_the_signer_uid_in_tests(callers)),
        f.signer.clone(),
    ));
    addr
}

/// A caller's link to `addr`, presenting a certificate for `own_name` from `authority`, and
/// expecting the server to be `expected`.
async fn dial(
    addr: SocketAddr,
    authority: &Arc<Authority>,
    own_name: &str,
    expected: &str,
) -> ClientLink {
    let issuer = FakeIssuer::new(authority, own_name, Usage::Client, LONG);
    // The caller pins the root of the authority the signer's belongs to.
    let roots = roots_from_pem(authority.root_pem().as_bytes()).unwrap();
    ClientLink::new(&addr.to_string(), expected, identity(&issuer).await, roots).unwrap()
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_an_unauthenticated_caller_is_refused(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let authority = Arc::new(Authority::new());
    let strangers = Arc::new(Authority::new());
    let id = f.intent("kreivo-test", "mint", None, "1", None).await;
    let callers = [caller("core", CORE, &[("kreivo-test", &["mint"])])];
    let addr = listen(&f, &authority, SIGNER, &callers).await;

    // A client certificate from another authority: the handshake does not hold.
    let foreign = {
        let issuer = FakeIssuer::new(&strangers, CORE, Usage::Client, LONG);
        let roots = roots_from_pem(authority.root_pem().as_bytes()).unwrap();
        ClientLink::new(&addr.to_string(), SIGNER, identity(&issuer).await, roots).unwrap()
    };
    assert!(Client::connect_tls(&foreign, TOKEN).await.is_err());

    // No certificate at all: TLS 1.3 completes on the client's side and the server then closes, so
    // the first request fails.
    let none = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots_from_pem(authority.root_pem().as_bytes()).unwrap())
        .with_no_client_auth();
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from(SIGNER.to_owned()).unwrap();
    let mut stream = TlsConnector::from(Arc::new(none))
        .connect(name, tcp)
        .await
        .unwrap();
    assert!(
        Client::raw(&mut stream, &Request::Sign { intent: id })
            .await
            .is_err()
    );

    // A good certificate for a name no caller has: the token is not even considered.
    let stranger = dial(addr, &authority, "other.link.bezent.test.example", SIGNER).await;
    let err = Client::connect_tls(&stranger, TOKEN).await.unwrap_err();
    assert!(matches!(err, LinkError::Unauthenticated), "{err}");

    // The right certificate and a wrong token.
    let core = dial(addr, &authority, CORE, SIGNER).await;
    let err = Client::connect_tls(&core, "not-the-token")
        .await
        .unwrap_err();
    assert!(matches!(err, LinkError::Unauthenticated), "{err}");

    assert_eq!(f.signatures().await, 0, "nothing was signed");
    assert!(
        f.log.text().contains("REQ-AR-3: unauthenticated"),
        "{}",
        f.log.text()
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_a_certificate_for_another_name_is_refused_the_intents_it_may_not_ask_for(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let authority = Arc::new(Authority::new());
    let callers = [
        caller("core", CORE, &[("kreivo-test", &["mint", "refund-mint"])]),
        caller(
            "evm-endpoint",
            EVM,
            &[("polygon-test", &["payout", "cancel"])],
        ),
    ];
    let addr = listen(&f, &authority, SIGNER, &callers).await;
    let mint = f.intent("kreivo-test", "mint", None, "1", None).await;
    let payout = f.intent("polygon-test", "payout", None, "10", None).await;
    let pool = f.core_pool().await;

    // The EVM endpoint, authenticated, asks for a Kreivo mint: refused by the link, unread.
    let evm = dial(addr, &authority, EVM, SIGNER).await;
    let mut client = Client::connect_tls(&evm, TOKEN).await.unwrap();
    let Response::Refused { spec, reason, .. } = client.request(mint).await.unwrap() else {
        panic!("a Kreivo mint was not refused to the EVM endpoint");
    };
    assert_eq!(spec, "REQ-AR-3");
    assert!(reason.contains("evm-endpoint"), "{reason}");
    assert_eq!(f.signatures().await, 0);

    // The core asks for a Polygon payout: refused the same way.
    let core = dial(addr, &authority, CORE, SIGNER).await;
    let mut client = Client::connect_tls(&core, TOKEN).await.unwrap();
    assert!(matches!(
        client.request(payout).await.unwrap(),
        Response::Refused { spec, .. } if spec == "REQ-AR-3"
    ));
    assert_eq!(f.signatures().await, 0);

    // What each may ask for is signed, and verifies over the recorded bytes.
    let Response::Signed { key, .. } = client.sign(&pool, mint).await.unwrap() else {
        panic!("the core's mint was not signed");
    };
    assert_eq!(key, "kreivo-mint");
    let mut evm = Client::connect_tls(&evm_link(addr, &authority).await, TOKEN)
        .await
        .unwrap();
    let Response::Signed { key, .. } = evm.sign(&pool, payout).await.unwrap() else {
        panic!("the EVM endpoint's payout was not signed");
    };
    assert_eq!(key, "polygon-payout");
    assert_eq!(f.signatures().await, 2);
}

async fn evm_link(addr: SocketAddr, authority: &Arc<Authority>) -> ClientLink {
    dial(addr, authority, EVM, SIGNER).await
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn req_ar_3_a_server_certificate_for_the_wrong_name_is_refused_by_the_caller(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let f = Fixture::new(pool, connect).await;
    let authority = Arc::new(Authority::new());
    let callers = [caller("core", CORE, &[("kreivo-test", &["mint"])])];
    // A server the authority vouches for, under another name: a caller's own certificate, say.
    let addr = listen(
        &f,
        &authority,
        "impostor.link.bezent.test.example",
        &callers,
    )
    .await;
    let core = dial(addr, &authority, CORE, SIGNER).await;
    let err = Client::connect_tls(&core, TOKEN).await.unwrap_err();
    assert!(matches!(err, LinkError::Io(_)), "{err}");
    assert!(
        err.to_string().to_lowercase().contains("certificate"),
        "{err}"
    );
    // The caller sent no token to it.
    assert_eq!(f.signatures().await, 0);
}

#[tokio::test]
async fn req_cu_1_a_renewed_certificate_is_swapped_in_under_a_live_listener() {
    let authority = Arc::new(Authority::new());
    // The certificate "lives" 3 seconds, so the renewal falls due after 2.
    let issuer = FakeIssuer::new(&authority, SIGNER, Usage::Server, Duration::from_secs(3));
    let held = identity(&issuer).await;
    assert_eq!(held.serial(), "serial-1");
    let roots = roots_from_pem(authority.root_pem().as_bytes()).unwrap();
    let acceptor = TlsAcceptor::from(server_config(held.clone(), roots.clone()).unwrap());
    let tcp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = tcp.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = tcp.accept().await.unwrap();
            let acceptor = acceptor.clone();
            // The handshake is all this test needs of the listener.
            tokio::spawn(async move {
                let _ = acceptor.accept(stream).await;
            });
        }
    });

    // The client presents a client certificate too, as the server requires.
    let client_issuer = FakeIssuer::new(&authority, CORE, Usage::Client, LONG);
    let link = ClientLink::new(
        &addr.to_string(),
        SIGNER,
        identity(&client_issuer).await,
        roots,
    )
    .unwrap();
    let seen = || async {
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let tls = TlsConnector::from(link.config.clone())
            .connect(link.server_name.clone(), tcp)
            .await
            .unwrap();
        tls.get_ref().1.peer_certificates().unwrap()[0].to_vec()
    };
    assert_eq!(seen().await, issuer.issued()[0]);

    // A Vault outage at renewal time: the listener keeps the certificate it has, and serves.
    issuer.down.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(2600)).await;
    assert_eq!(
        held.serial(),
        "serial-1",
        "nothing to swap in while Vault is down"
    );
    assert_eq!(seen().await, issuer.issued()[0], "still serving");

    // Vault answers again: the next certificate replaces it, without a restart of the listener.
    issuer.down.store(false, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while held.serial() == "serial-1" {
        assert!(std::time::Instant::now() < deadline, "no renewal");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let issued = issuer.issued();
    assert_eq!(issued.len(), 2);
    assert_ne!(issued[0], issued[1]);
    assert_eq!(
        seen().await,
        issued[1],
        "new handshakes use the new certificate"
    );
}

#[test]
fn req_ar_3_a_caller_is_known_by_the_one_dns_name_of_its_certificate() {
    let authority = Authority::new();
    let one = authority.issue(CORE, Usage::Client, "s", LONG);
    let der = rustls::pki_types::CertificateDer::from(one.certificate_der);
    assert_eq!(peer_name(Some(&[der])).as_deref(), Some(CORE));
    assert_eq!(peer_name(None), None);

    // Two names, or a name that is not DNS, map to nobody.
    let mut params = CertificateParams::new(vec![CORE.to_owned(), EVM.to_owned()]).unwrap();
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().unwrap();
    let two = params
        .signed_by(&key, &authority.link.0, &authority.link.1)
        .unwrap();
    assert_eq!(peer_name(Some(&[two.der().clone()])), None);
}

/// The TLS end of the dial's bound (`dial_timeout.rs` has the socket's): a server that accepts the
/// connection and says nothing is an error within the bound, not a caller held for good.
#[tokio::test]
async fn a_server_that_never_answers_the_handshake_is_an_error_within_the_bound() {
    let authority = Arc::new(Authority::new());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let core = dial(addr, &authority, CORE, SIGNER).await;

    let started = std::time::Instant::now();
    let answer = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect_tls_within(&core, TOKEN, Duration::from_millis(300)),
    )
    .await
    .expect("the dial's own bound ended it: it is not left waiting");
    let error = answer.expect_err("a silent server does not welcome");
    assert!(
        matches!(&error, LinkError::Io(e) if e.kind() == std::io::ErrorKind::TimedOut),
        "{error:?}"
    );
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(280), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

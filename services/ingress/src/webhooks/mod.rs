// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks to requesters (`REQ-ST-5`, `REQ-IC-5`, `REQ-ID-4`; PLAN `DEC-15`; issue #2).
//!
//! The sequenced log is the source of truth (`REQ-ST-4`). A notification only says "look at
//! position N": it carries the log position, the entry's stable de-duplication key and the
//! references of what it concerns, and never an outcome. The requester re-reads the status, and
//! a requester that receives nothing loses nothing, because it can read the log from its last
//! position ("no requester may need one to arrive").
//!
//! - **Registration** ([`api`]): a requester may set one callback URL per transfer, by its
//!   request reference or its bridge reference, only for a transfer its own request made or
//!   reached (`REQ-ID-4`), and may replace it later (Swaps issues a new callback token on each
//!   retry): every change is recorded append-only (hashes of the old and new URL, the actor, the
//!   time) and a delivery goes to the URL current when it is attempted; and a URL for conditions (`REQ-IC-5`), the latest of
//!   which is used. URLs are `https`; where a webhook may go, and what is checked when, is
//!   [`egress`]'s (PR #55 review M1).
//! - **Signing:** each requester has a secret it obtains from `POST /v0/webhooks/secret` (each
//!   call rotates it). Every delivery is signed: `Bezent-Signature: v1=<hex>` is HMAC-SHA256 of
//!   `<Bezent-Timestamp>.<body>` under that secret ([`signature`]). A delivery is never sent
//!   unsigned.
//! - **At rest:** a signing secret must be readable to sign, so it cannot be hashed like a
//!   credential; it moves no value, so it is not the signer's (`REQ-AR-3` is about keys that move
//!   value). It is sealed (ChaCha20-Poly1305, the requester as associated data) under the
//!   ingress's webhook key, which the ingress reads from a file and the store never holds. The
//!   callback URLs are sealed the same way: a requester's URL may carry its own access token.
//! - **Delivery** ([`dispatch`]): a dispatcher fans the log out into an outbox, one row per
//!   (requester, de-duplication key), and posts each until it is acknowledged with a 2xx
//!   (at-least-once within the declared retry bound), with exponential back-off, dead-lettering it
//!   after [`Webhooks::max_attempts`], where it stays visible. Requesters see their deliveries at `GET /v0/webhooks/deliveries`.
//! - **Conditions** (`REQ-IC-5`): a shared gate condition (`CND-Paused`, `CND-CutOff`,
//!   `CND-ReserveShort`) on a route is notified to the condition URL of every requester granted
//!   on the route, whether or not it holds a transfer there, once when the condition starts and
//!   once when it clears; the in-place holds themselves are not notified per transfer.

pub mod api;
pub mod dispatch;
pub mod egress;

use crate::error::ApiError;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;
use url::{Host, Url};

pub use dispatch::{Dispatcher, Step};
pub use egress::{Deny, Reach, class};

/// The longest a webhook waits for a connection (TLS included), within [`Webhooks::timeout`].
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// The header carrying the signature.
pub const SIGNATURE_HEADER: &str = "bezent-signature";
/// The header carrying the Unix time, in seconds, the signature covers.
pub const TIMESTAMP_HEADER: &str = "bezent-timestamp";
/// The header carrying the delivery's de-duplication key (also in the body).
pub const DEDUPE_HEADER: &str = "bezent-dedupe-key";

/// The longest callback URL taken, in bytes.
pub const MAX_URL_LEN: usize = 2048;

/// The shared gate conditions notified once per (condition, route) (`REQ-IC-5`): the core's gates
/// before a delivery, which hold every transfer of a route alike.
pub const GATE_CONDITIONS: &[&str] = &["CND-Paused", "CND-CutOff", "CND-ReserveShort"];

/// The signature of a delivery: `v1=` and the hex HMAC-SHA256, under the requester's secret, of
/// the timestamp, a full stop, and the exact body bytes.
pub fn signature(secret: &[u8], timestamp: i64, body: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes a key of any length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    format!("v1={}", hex::encode(mac.finalize().into_bytes()))
}

/// The key the ingress seals webhook secrets and callback URLs under. It never prints.
#[derive(Clone)]
pub struct SealingKey(Key);

impl fmt::Debug for SealingKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealingKey(<redacted>)")
    }
}

impl SealingKey {
    /// A key from its 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Key::from(bytes))
    }

    /// A fresh random key (tests and the TEST-only sidecar, whose sealed rows need not outlive it).
    pub fn random() -> Self {
        Self::from_bytes(random())
    }

    /// Reads a key from a file holding 64 hexadecimal characters (surrounding whitespace aside).
    /// The file must not be readable or writable by its group or by others (PR #55 review S6).
    pub fn from_file(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .map_err(|e| format!("cannot read the webhook key {}: {e}", path.display()))?
                .permissions()
                .mode();
            if mode & 0o077 != 0 {
                return Err(format!(
                    "the webhook key {} is open to its group or others (mode {:o}); chmod 600 it",
                    path.display(),
                    mode & 0o777
                ));
            }
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read the webhook key {}: {e}", path.display()))?;
        let bytes = hex::decode(text.trim())
            .ok()
            .and_then(|b| <[u8; 32]>::try_from(b).ok())
            .ok_or_else(|| {
                format!(
                    "the webhook key {} is not 64 hexadecimal characters",
                    path.display()
                )
            })?;
        Ok(Self::from_bytes(bytes))
    }

    /// A keyed digest of a requester's callback URL, recorded with each change of it (PR #55
    /// review S5): HMAC-SHA256, under a subkey of this key, of the requester's name and the URL.
    /// A plain hash would let any reader of the store test a guess of a token in the URL.
    pub(crate) fn url_digest(&self, requester: &str, url: &str) -> [u8; 32] {
        let mut sub = <Hmac<Sha256> as Mac>::new_from_slice(self.0.as_slice())
            .expect("HMAC takes a key of any length");
        sub.update(b"bezent/webhook-url-digest/v1");
        let subkey = sub.finalize().into_bytes();
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(subkey.as_slice())
            .expect("HMAC takes a key of any length");
        mac.update(requester.as_bytes());
        mac.update(&[0]);
        mac.update(url.as_bytes());
        mac.finalize().into_bytes().into()
    }

    /// Seals `plain` with `aad` as associated data; returns the nonce and the sealed bytes.
    pub(crate) fn seal(&self, aad: &[u8], plain: &[u8]) -> ([u8; 12], Vec<u8>) {
        let nonce: [u8; 12] = random();
        let sealed = ChaCha20Poly1305::new(&self.0)
            .encrypt(&Nonce::from(nonce), Payload { msg: plain, aad })
            .expect("sealing a short message does not fail");
        (nonce, sealed)
    }

    /// Opens what [`SealingKey::seal`] sealed with the same `aad`; `None` if it does not open.
    pub(crate) fn open(&self, aad: &[u8], nonce: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        let nonce = <[u8; 12]>::try_from(nonce).ok()?;
        ChaCha20Poly1305::new(&self.0)
            .decrypt(&Nonce::from(nonce), Payload { msg: sealed, aad })
            .ok()
    }
}

/// The associated data a requester's signing secret is sealed with.
pub(crate) fn secret_aad(requester: &str) -> Vec<u8> {
    [b"bezent/webhook-secret/".as_slice(), requester.as_bytes()].concat()
}

/// The associated data a requester's callback URL is sealed with.
pub(crate) fn url_aad(requester: &str) -> Vec<u8> {
    [b"bezent/webhook-url/".as_slice(), requester.as_bytes()].concat()
}

/// `N` random bytes from the operating system.
pub(crate) fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("the operating system gives random bytes");
    bytes
}

/// The webhook settings of an ingress.
#[derive(Clone, Debug)]
pub struct Webhooks {
    /// The key secrets and URLs are sealed under.
    pub key: SealingKey,
    /// The hosts (names or IP literals, as written in a URL) that may take plain `http` and
    /// resolve to loopback, private, unique-local or shared addresses: the test harness's. Never
    /// link-local or metadata addresses, never a [`Webhooks::deny`]ed one. Only on a test store:
    /// the ingress refuses to start with one on a store that declares a production network.
    pub allow_hosts: Vec<String>,
    /// bezent's own service addresses, never posted to, allowlisted or not (PR #55 review M1).
    pub deny: Vec<Deny>,
    /// Attempts before a delivery is dead-lettered.
    pub max_attempts: i32,
    /// The first retry's delay; each retry doubles it, up to [`Webhooks::backoff_cap`].
    pub backoff_base: Duration,
    /// The longest delay between two attempts.
    pub backoff_cap: Duration,
    /// How long one attempt waits for an answer.
    pub timeout: Duration,
    /// Log positions fanned out, and deliveries attempted, per step.
    pub batch: i64,
    /// The dispatcher's period.
    pub tick: Duration,
    /// How long a claimed delivery is left to one dispatcher before another may retry it.
    pub lease: Duration,
}

impl Webhooks {
    /// The defaults, under `key`.
    pub fn new(key: SealingKey) -> Self {
        Self {
            key,
            allow_hosts: Vec::new(),
            deny: Vec::new(),
            max_attempts: 12,
            backoff_base: Duration::from_secs(2),
            backoff_cap: Duration::from_secs(3600),
            timeout: Duration::from_secs(10),
            batch: 100,
            tick: Duration::from_secs(1),
            lease: Duration::from_secs(60),
        }
    }

    /// The delay before the attempt after `attempts` failed ones: the base doubled per failure,
    /// capped, with up to a quarter of it added at random so retries spread out.
    pub fn backoff(&self, attempts: i32) -> Duration {
        let base = u64::try_from(self.backoff_base.as_millis()).unwrap_or(u64::MAX);
        let cap = u64::try_from(self.backoff_cap.as_millis()).unwrap_or(u64::MAX);
        let shift = u32::try_from(attempts.saturating_sub(1).clamp(0, 30)).unwrap_or(30);
        let delay = base.saturating_mul(1u64 << shift).min(cap);
        let jitter = u64::from_le_bytes(random()) % (delay / 4 + 1);
        Duration::from_millis(delay.saturating_add(jitter).min(cap.max(delay)))
    }

    /// Checks a callback URL at registration and returns it normalised (`ERR-Malformed`
    /// otherwise): absolute, `https` (plain `http` only for an allowlisted host), with a host, no
    /// user information and no fragment; an IP literal must be one a webhook may reach, and a
    /// denied host is refused. Names are not resolved here: the send-time check
    /// ([`Webhooks::vet`]) is the guard.
    pub(crate) fn check_url(&self, raw: &str) -> Result<String, ApiError> {
        if raw.len() > MAX_URL_LEN {
            return Err(ApiError::Malformed(format!(
                "url is longer than {MAX_URL_LEN} bytes"
            )));
        }
        let url = Url::parse(raw)
            .map_err(|_| ApiError::Malformed("url is not an absolute URL".into()))?;
        if url.fragment().is_some() {
            return Err(ApiError::Malformed("url must not carry a fragment".into()));
        }
        match self.screen(&url) {
            Ok(_) => Ok(url.into()),
            Err((_, why)) => Err(ApiError::Malformed(format!("url {why} (REQ-ST-5)"))),
        }
    }

    /// Whether `host` (as a URL writes it, IPv6 without brackets) is on the allowlist.
    fn allowlisted(&self, host: &str) -> bool {
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let host = host.trim_end_matches('.');
        self.allow_hosts.iter().any(|h| {
            h.trim_start_matches('[')
                .trim_end_matches(']')
                .trim_end_matches('.')
                .eq_ignore_ascii_case(host)
        })
    }

    /// Whether `addr`, which `allowlisted` or not host resolved to, may be posted to.
    fn admits(&self, allowlisted: bool, addr: SocketAddr) -> bool {
        !self.deny.iter().any(|d| d.covers_addr(addr))
            && match egress::reach(addr.ip()) {
                Reach::Global => true,
                Reach::Internal => allowlisted,
                Reach::Never => false,
            }
    }

    /// The checks that need no resolution; for a URL that passes, where it goes.
    fn screen(&self, url: &Url) -> Result<Screened, (&'static str, &'static str)> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err((class::URL_NOT_ALLOWED, "must not carry user information"));
        }
        let Some(host) = url.host() else {
            return Err((class::URL_NOT_ALLOWED, "names no host"));
        };
        let name = url.host_str().unwrap_or_default().to_owned();
        let allowlisted = self.allowlisted(&name);
        match url.scheme() {
            "https" => {}
            "http" if allowlisted => {}
            _ => return Err((class::URL_NOT_ALLOWED, "must be https")),
        }
        let port = url.port_or_known_default().unwrap_or(443);
        match host {
            Host::Domain(domain) => {
                if self.deny.iter().any(|d| d.covers_host(domain, port)) {
                    return Err((
                        class::ADDRESS_NOT_ALLOWED,
                        "names a host bezent does not post to",
                    ));
                }
                Ok(Screened::Name {
                    domain: domain.to_owned(),
                    port,
                    allowlisted,
                })
            }
            Host::Ipv4(ip) => self.literal(allowlisted, SocketAddr::new(ip.into(), port)),
            Host::Ipv6(ip) => self.literal(allowlisted, SocketAddr::new(ip.into(), port)),
        }
    }

    fn literal(
        &self,
        allowlisted: bool,
        addr: SocketAddr,
    ) -> Result<Screened, (&'static str, &'static str)> {
        if self.admits(allowlisted, addr) {
            Ok(Screened::Literal)
        } else {
            Err((
                class::ADDRESS_NOT_ALLOWED,
                "names an address bezent does not post to",
            ))
        }
    }

    /// The send-time guard (PR #55 review M1): screens `url`, resolves its host once, and admits
    /// it only when every address it resolves to is admitted. Returns the addresses to pin the
    /// connection to (none for an IP literal, which is not resolved), or the class of refusal.
    pub(crate) async fn vet(
        &self,
        url: &Url,
    ) -> Result<Option<(String, Vec<SocketAddr>)>, &'static str> {
        let screened = self.screen(url).map_err(|(class, _)| class)?;
        let Screened::Name {
            domain,
            port,
            allowlisted,
        } = screened
        else {
            return Ok(None);
        };
        let resolved = tokio::time::timeout(
            self.timeout,
            tokio::net::lookup_host((domain.clone(), port)),
        )
        .await;
        let addrs: Vec<SocketAddr> = match resolved {
            Ok(Ok(addrs)) => addrs.collect(),
            // A name that does not resolve is reported as a refused address, so the listing is
            // no oracle for which internal names exist (S-d). The operator's log tells them apart.
            Ok(Err(_)) => {
                tracing::info!("webhook host did not resolve");
                return Err(class::ADDRESS_NOT_ALLOWED);
            }
            Err(_) => return Err(class::TIMEOUT),
        };
        if addrs.is_empty() {
            tracing::info!("webhook host did not resolve");
            return Err(class::ADDRESS_NOT_ALLOWED);
        }
        if !addrs.iter().all(|a| self.admits(allowlisted, *a)) {
            return Err(class::ADDRESS_NOT_ALLOWED);
        }
        Ok(Some((domain, addrs)))
    }

    /// The client of one attempt: connects only to `pinned` (the vetted addresses of the URL's
    /// host) and resolves nothing else, follows no redirect, ignores proxy variables, and gives up
    /// after [`CONNECT_TIMEOUT`] to connect and [`Webhooks::timeout`] in all.
    pub fn client(
        &self,
        pinned: Option<(&str, &[SocketAddr])>,
    ) -> reqwest::Result<reqwest::Client> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(self.timeout)
            .connect_timeout(self.timeout.min(CONNECT_TIMEOUT))
            .dns_resolver(NoResolution)
            .user_agent("bezent-webhooks");
        if let Some((domain, addrs)) = pinned {
            builder = builder.resolve_to_addrs(domain, addrs);
        }
        builder.build()
    }
}

/// Where a screened URL goes.
enum Screened {
    /// An IP literal, already admitted.
    Literal,
    /// A name, to resolve and vet at send time.
    Name {
        domain: String,
        port: u16,
        allowlisted: bool,
    },
}

/// The resolver of a webhook's client: it resolves nothing. Only the vetted addresses pinned on
/// the client are ever connected to.
struct NoResolution;

impl reqwest::dns::Resolve for NoResolution {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async {
            Err::<reqwest::dns::Addrs, _>("webhooks resolve only through the vetted pin".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_st_5_a_signature_covers_the_timestamp_and_the_exact_body() {
        let a = signature(b"secret", 1_700_000_000, "{\"seq\":1}");
        assert!(a.starts_with("v1="));
        assert_eq!(a.len(), 3 + 64);
        assert_ne!(a, signature(b"secret", 1_700_000_001, "{\"seq\":1}"));
        assert_ne!(a, signature(b"secret", 1_700_000_000, "{\"seq\":2}"));
        assert_ne!(a, signature(b"other", 1_700_000_000, "{\"seq\":1}"));
        // RFC 4231-style check of the construction: HMAC-SHA256 of "<t>.<body>".
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(b"secret").unwrap();
        mac.update(b"1700000000.{\"seq\":1}");
        assert_eq!(
            a,
            format!("v1={}", hex::encode(mac.finalize().into_bytes()))
        );
    }

    #[test]
    fn req_st_5_a_sealed_secret_opens_only_under_its_key_and_its_requester() {
        let key = SealingKey::random();
        let (nonce, sealed) = key.seal(&secret_aad("alpha"), b"the secret");
        assert!(!sealed.windows(10).any(|w| w == b"the secret"));
        assert_eq!(
            key.open(&secret_aad("alpha"), &nonce, &sealed).as_deref(),
            Some(b"the secret".as_slice())
        );
        assert_eq!(key.open(&secret_aad("beta"), &nonce, &sealed), None);
        assert_eq!(key.open(&url_aad("alpha"), &nonce, &sealed), None);
        assert_eq!(
            SealingKey::random().open(&secret_aad("alpha"), &nonce, &sealed),
            None
        );
        assert!(!format!("{key:?}").contains(&hex::encode(key.0)));
    }

    #[cfg(unix)]
    #[test]
    fn s6_a_key_file_open_to_its_group_or_others_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("bezent-s6-{}", hex::encode(random::<8>())));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("webhook.key");
        std::fs::write(&path, hex::encode([7u8; 32])).unwrap();
        for open in [0o644, 0o640, 0o604, 0o660] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(open)).unwrap();
            let err = SealingKey::from_file(&path).unwrap_err();
            assert!(err.contains("chmod 600"), "{open:o}: {err}");
            assert!(!err.contains(&hex::encode([7u8; 32])));
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(SealingKey::from_file(&path).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn s5_a_url_digest_is_keyed_and_bound_to_its_requester() {
        let key = SealingKey::random();
        let url = "https://swaps.example/resolve/t?access_token=abc";
        let d = key.url_digest("alpha", url);
        assert_eq!(d, key.url_digest("alpha", url));
        assert_ne!(d, key.url_digest("beta", url));
        assert_ne!(d, SealingKey::random().url_digest("alpha", url));
        assert_ne!(
            d.as_slice(),
            <Sha256 as sha2::Digest>::digest(url).as_slice()
        );
    }

    #[test]
    fn req_st_5_backoff_doubles_and_is_capped() {
        let mut w = Webhooks::new(SealingKey::random());
        w.backoff_base = Duration::from_millis(1000);
        w.backoff_cap = Duration::from_millis(10_000);
        let first = w.backoff(1);
        assert!(first >= Duration::from_millis(1000) && first <= Duration::from_millis(1250));
        let third = w.backoff(3);
        assert!(third >= Duration::from_millis(4000) && third <= Duration::from_millis(5000));
        assert!(w.backoff(40) <= Duration::from_millis(10_000));
    }
}

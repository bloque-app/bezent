// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The Vault client: a [`Client`] logs in under the process's AWS credentials and returns a
//! [`Session`], which reads KV secrets and issues PKI certificates.
//!
//! The roles the services log in to have a token TTL of a minute, so a token is never kept: a
//! caller logs in, uses the [`Session`] in a burst, and drops it (its token is zeroized on drop).
//! The next use logs in again.
//!
//! The transport is HTTPS (plain HTTP only on loopback), follows no redirect (a standby node's
//! redirect must not carry a token anywhere), and has bounded connect and request times. A login
//! or read is retried a few times on a transport failure or a 5xx; a certificate request is not
//! (it would issue twice). A 4xx is never retried.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use reqwest::Method;
use reqwest::header::{CONTENT_TYPE, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::OnceCell;
use zeroize::Zeroizing;

use crate::config::VaultConfig;
use crate::pki::{self, IssueRequest, Issued};
use crate::{Error, address, kv, login};

/// The longest answer read: a secret is small.
const MAX_BODY: usize = 1 << 20;

/// Attempts at an idempotent request.
const ATTEMPTS: u32 = 3;

/// The first retry's delay; each retry doubles it.
const BACKOFF: Duration = Duration::from_millis(200);

struct Inner {
    http: reqwest::Client,
    base: String,
    config: VaultConfig,
    credentials: OnceCell<SharedCredentialsProvider>,
}

/// A Vault server and the way to log in to it. Cheap to clone.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("role", &self.inner.config.role)
            .finish_non_exhaustive()
    }
}

/// Whether a path (a mount, a secret, a role) can go into a URL as it is.
fn plain_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~'))
        })
}

impl VaultConfig {
    /// Refuses a configuration the client could not use: an address that is not HTTPS (loopback
    /// aside), a mount or role that is not a plain path, a region or server ID login would refuse.
    /// Touches no network.
    pub fn check(&self) -> Result<(), Error> {
        address::base(&self.address)?;
        if !plain_path(&self.mount) {
            return Err(Error::Config("vault.mount is not a plain path".to_owned()));
        }
        if self.role.is_empty() || self.role.chars().any(|c| c.is_control()) {
            return Err(Error::Config("vault.role is not usable".to_owned()));
        }
        let region = &self.sts_region;
        if region.is_empty()
            || !region
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(Error::Config("vault.sts_region is not a region".to_owned()));
        }
        if self.server_id.is_empty() || self.server_id.chars().any(|c| c.is_control()) {
            return Err(Error::Config("vault.server_id is not usable".to_owned()));
        }
        Ok(())
    }
}

impl Client {
    /// A client for `config`, logging in under the default AWS credential chain (on Fargate, the
    /// task role's container credentials). Contacts nothing until [`Client::login`].
    pub fn new(config: &VaultConfig) -> Result<Self, Error> {
        Self::build(config, OnceCell::new())
    }

    /// A client that signs with `credentials` instead of the default chain: for tests.
    pub fn with_credentials(
        config: &VaultConfig,
        credentials: impl ProvideCredentials + 'static,
    ) -> Result<Self, Error> {
        Self::build(
            config,
            OnceCell::new_with(Some(SharedCredentialsProvider::new(credentials))),
        )
    }

    fn build(
        config: &VaultConfig,
        credentials: OnceCell<SharedCredentialsProvider>,
    ) -> Result<Self, Error> {
        config.check()?;
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| Error::Config("the Vault client could not be built".to_owned()))?;
        Ok(Self {
            inner: Arc::new(Inner {
                http,
                base: address::base(&config.address)?,
                config: config.clone(),
                credentials,
            }),
        })
    }

    /// Logs in with the AWS auth method and returns the session. Each call is a fresh login.
    pub async fn login(&self) -> Result<Session, Error> {
        let inner = &self.inner;
        let provider = inner
            .credentials
            .get_or_try_init(|| async {
                let chain =
                    aws_config::default_provider::credentials::DefaultCredentialsChain::builder()
                        .build()
                        .await;
                Ok::<_, Error>(SharedCredentialsProvider::new(chain))
            })
            .await?;
        let credentials = provider
            .provide_credentials()
            .await
            .map_err(|_| Error::Credentials("no AWS credentials are available"))?;
        let body = login::login(
            &credentials,
            &inner.config.role,
            &inner.config.sts_region,
            &inner.config.server_id,
            SystemTime::now(),
        )?;
        let path = format!("auth/{}/login", inner.config.mount);
        let answer = self
            .send("login", Method::POST, &path, None, Some(&body), true)
            .await?;
        #[derive(Deserialize)]
        struct Answer {
            auth: Auth,
        }
        #[derive(Deserialize)]
        struct Auth {
            client_token: Zeroizing<String>,
            #[serde(default)]
            lease_duration: u64,
        }
        let Answer { auth } = serde_json::from_slice(&answer)
            .map_err(|_| Error::Malformed("the login answer has no token"))?;
        if auth.client_token.is_empty() {
            return Err(Error::Malformed("the login answer has no token"));
        }
        Ok(Session {
            client: self.clone(),
            token: auth.client_token,
            lease: Duration::from_secs(auth.lease_duration),
            born: Instant::now(),
        })
    }

    /// One request to `/v1/<path>`. Returns the body of a 2xx answer, never anything of a failure.
    async fn send(
        &self,
        operation: &'static str,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<&[u8]>,
        retry: bool,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        let path = path.trim_matches('/');
        if !plain_path(path) {
            return Err(Error::Config(format!("{path} is not a plain path")));
        }
        let url = format!("{}/v1/{path}", self.inner.base);
        let attempts = if retry { ATTEMPTS } else { 1 };
        let mut delay = BACKOFF;
        for attempt in 1..=attempts {
            let last = attempt == attempts;
            let mut request = self.inner.http.request(method.clone(), &url);
            if let Some(token) = token {
                let mut value = HeaderValue::from_str(token)
                    .map_err(|_| Error::Malformed("the token is not a header value"))?;
                value.set_sensitive(true);
                request = request.header("X-Vault-Token", value);
            }
            if let Some(body) = body {
                request = request
                    .header(CONTENT_TYPE, "application/json")
                    .body(body.to_vec());
            }
            match request.send().await {
                Ok(mut response) => {
                    let status = response.status();
                    if status.is_success() {
                        return read_body(&mut response).await;
                    }
                    if !status.is_server_error() || last {
                        return Err(Error::Status {
                            operation,
                            status: status.as_u16(),
                        });
                    }
                }
                Err(e) if last || !(e.is_connect() || e.is_timeout()) => {
                    return Err(Error::Unreachable(if e.is_timeout() {
                        "timed out"
                    } else if e.is_connect() {
                        "the connection failed"
                    } else {
                        "the request failed"
                    }));
                }
                Err(_) => {}
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        Err(Error::Unreachable("the request failed"))
    }
}

/// The body of an answer, at most [`MAX_BODY`] bytes.
async fn read_body(response: &mut reqwest::Response) -> Result<Zeroizing<Vec<u8>>, Error> {
    let too_long = Error::Malformed("the answer is too long");
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(too_long);
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::Unreachable("the answer could not be read"))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_long);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// A logged-in Vault token. It lives for a minute: use it in one burst. Its token is zeroized when
/// it is dropped, and never printed.
#[derive(Clone)]
pub struct Session {
    client: Client,
    token: Zeroizing<String>,
    lease: Duration,
    born: Instant,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl Session {
    /// How long a [`Resolver`](crate::Resolver) may keep using this session: a third of the
    /// token's lease, at most 20 seconds, so a burst of reads never meets an expired token.
    pub(crate) fn fresh(&self) -> bool {
        let limit = (self.lease / 3).min(Duration::from_secs(20));
        self.born.elapsed() < limit
    }

    /// The secret at `path` (KV version 1 or 2) as a `T`.
    pub async fn read<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let body = self.read_body(path).await?;
        kv::secret(&body)
    }

    /// The string field `field` of the secret at `path` (KV version 1 or 2).
    pub async fn read_field(&self, path: &str, field: &str) -> Result<Zeroizing<String>, Error> {
        let body = self.read_body(path).await?;
        kv::field(&body, field)?.ok_or_else(|| Error::NoField {
            path: path.to_owned(),
            field: field.to_owned(),
        })
    }

    async fn read_body(&self, path: &str) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.client
            .send("read", Method::GET, path, Some(&self.token), None, true)
            .await
    }

    /// Asks the PKI engine at `mount` for a certificate from `role`, and checks what came back
    /// ([`crate::pki`]).
    pub async fn issue(
        &self,
        mount: &str,
        role: &str,
        request: &IssueRequest,
    ) -> Result<Issued, Error> {
        self.issue_at(mount, role, request, SystemTime::now()).await
    }

    pub(crate) async fn issue_at(
        &self,
        mount: &str,
        role: &str,
        request: &IssueRequest,
        now: SystemTime,
    ) -> Result<Issued, Error> {
        if !plain_path(mount) || !plain_path(role) || role.contains('/') {
            return Err(Error::Config(
                "the PKI mount or role is not a plain path".to_owned(),
            ));
        }
        let body = serde_json::to_vec(&pki::IssueBody::new(request))
            .map_err(|_| Error::Config("the certificate request is not usable".to_owned()))?;
        let path = format!("{mount}/issue/{role}");
        let answer = self
            .client
            .send(
                "issue",
                Method::POST,
                &path,
                Some(&self.token),
                Some(&body),
                false,
            )
            .await?;
        let answer: pki::IssueAnswer = serde_json::from_slice(&answer)
            .map_err(|_| Error::Malformed("the answer is not an issued certificate"))?;
        pki::verify(answer.data, request, now)
    }
}

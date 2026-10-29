// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The configuration types the services share: where Vault is and how to log in ([`VaultConfig`]),
//! where a secret value comes from ([`SecretRef`]), and the store's address with its login and
//! password as references ([`StoreSource`]).
//!
//! None of them holds a secret: a configuration file names a file or a Vault path and field, never
//! a value (`REQ-KY-1`). In TOML:
//!
//! ```toml
//! [vault]
//! address = "https://vault.example:8200"
//! mount = "bezent"                       # the AWS auth mount: auth/<mount>/login
//! role = "bezent-core-iam"
//! sts_region = "us-east-2"
//! server_id = "vault.example"            # the X-Vault-AWS-IAM-Server-ID header
//!
//! [store]
//! host = "db.example"
//! database = "bezent"
//! login = { vault = { path = "secret/bezent/core", field = "db_login" } }
//! password = { vault = { path = "secret/bezent/core", field = "db_password" } }
//! sslrootcert = "/etc/bezent/rds-ca.pem"
//! ```

use std::path::{Path, PathBuf};

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::Error;

/// Where Vault is and how a service logs in to it with its AWS task role.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VaultConfig {
    /// The server's base URL, for example `https://vault.example:8200`. HTTPS, except on loopback.
    pub address: String,
    /// The AWS auth method's mount: the login is `auth/<mount>/login`.
    pub mount: String,
    /// The Vault role to log in as.
    pub role: String,
    /// The region of the STS endpoint the login request is signed for and Vault sends it to.
    pub sts_region: String,
    /// The value of the `X-Vault-AWS-IAM-Server-ID` header, which binds the signature to this Vault.
    pub server_id: String,
}

/// A secret in Vault: the string field `field` of the secret at `path` (KV v1 or v2).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VaultField {
    /// The secret's path under `/v1/`, for example `secret/bezent/core`.
    pub path: String,
    /// The field of the secret.
    pub field: String,
}

/// Where a secret value comes from: a file (`{ file = "<path>" }`, as before) or Vault
/// (`{ vault = { path, field } }`, which needs the `[vault]` table).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretRef {
    /// A file holding the value (surrounding whitespace aside).
    File(PathBuf),
    /// A field of a secret in Vault.
    Vault(VaultField),
}

impl SecretRef {
    /// Makes a relative file path the configuration directory's `base`, as the other paths are.
    pub fn rebase(&mut self, base: &Path) {
        if let SecretRef::File(p) = self
            && p.is_relative()
        {
            *p = base.join(&*p);
        }
    }

    /// Whether the value is read from Vault.
    pub fn is_vault(&self) -> bool {
        matches!(self, SecretRef::Vault(_))
    }
}

/// The store: where it is, and its login and password as references. The URL is built in the
/// process ([`StoreSource::url`]), so no file holds a URL with a password in it.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StoreSource {
    /// The server's host name or IP address.
    pub host: String,
    /// Its port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// The database.
    pub database: String,
    /// The login role's name.
    pub login: SecretRef,
    /// Its password.
    pub password: SecretRef,
    /// `disable`, `require`, `verify-ca` or `verify-full` (`bezent_store::tls`). `verify-full` by
    /// default: a deployment says otherwise on purpose.
    #[serde(default = "default_sslmode")]
    pub sslmode: String,
    /// A PEM file of CA certificates for the server's certificate (`sslrootcert`).
    #[serde(default)]
    pub sslrootcert: Option<PathBuf>,
}

fn default_port() -> u16 {
    5432
}

fn default_sslmode() -> String {
    "verify-full".to_owned()
}

/// Everything but RFC 3986's unreserved characters is percent-encoded in the userinfo and the
/// database name, so `@`, `:`, `/`, `?`, `#` and `%` in a password cannot change what the URL means.
const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The same for a file path in the query: `/` stays.
const PATH: &AsciiSet = &COMPONENT.remove(b'/');

fn push_encoded(out: &mut String, value: &str, set: &'static AsciiSet) {
    for piece in utf8_percent_encode(value, set) {
        out.push_str(piece);
    }
}

impl StoreSource {
    /// Refuses a source that cannot make a URL: a bad `sslmode`, an empty host or database, or a
    /// host with characters a URL authority cannot hold.
    pub fn check(&self) -> Result<(), Error> {
        if !["disable", "require", "verify-ca", "verify-full"].contains(&self.sslmode.as_str()) {
            return Err(Error::Config(
                "store.sslmode must be disable, require, verify-ca or verify-full".to_owned(),
            ));
        }
        let host = self.host.trim_matches(['[', ']']);
        let plain = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':'));
        if !plain {
            return Err(Error::Config(
                "store.host is not a host name or address".to_owned(),
            ));
        }
        if self.database.is_empty() {
            return Err(Error::Config("store.database is empty".to_owned()));
        }
        Ok(())
    }

    /// The `postgres://` URL for `login` and `password`, with `sslmode` and `sslrootcert`. Both are
    /// percent-encoded, whatever they hold.
    pub fn url(&self, login: &str, password: &str) -> Result<Zeroizing<String>, Error> {
        self.check()?;
        if login.is_empty() {
            return Err(Error::Config("the store login is empty".to_owned()));
        }
        let host = self.host.trim_matches(['[', ']']);
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        // Pushed piece by piece into a zeroizing string: no intermediate copy of the password.
        let mut url = Zeroizing::new(String::from("postgres://"));
        push_encoded(&mut url, login, COMPONENT);
        url.push(':');
        push_encoded(&mut url, password, COMPONENT);
        url.push('@');
        url.push_str(&host);
        url.push(':');
        url.push_str(&self.port.to_string());
        url.push('/');
        push_encoded(&mut url, &self.database, COMPONENT);
        url.push_str("?sslmode=");
        url.push_str(&self.sslmode);
        if let Some(root) = &self.sslrootcert {
            let root = root
                .to_str()
                .ok_or_else(|| Error::Config("store.sslrootcert is not valid UTF-8".to_owned()))?;
            url.push_str("&sslrootcert=");
            push_encoded(&mut url, root, PATH);
        }
        Ok(url)
    }

    /// Makes a relative `sslrootcert` and file references the configuration directory's.
    pub fn rebase(&mut self, base: &Path) {
        self.login.rebase(base);
        self.password.rebase(base);
        if let Some(p) = self.sslrootcert.as_mut()
            && p.is_relative()
        {
            *p = base.join(&*p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Holder {
        a: SecretRef,
        #[serde(default)]
        vault: Option<VaultConfig>,
    }

    #[test]
    fn req_ky_1_a_secret_ref_is_a_file_or_a_vault_field() {
        let h: Holder = toml::from_str(
            "a = { file = \"/run/secrets/x\" }\n[vault]\naddress = \"https://v.example\"\nmount = \"m\"\nrole = \"r\"\nsts_region = \"us-east-2\"\nserver_id = \"s\"\n",
        )
        .unwrap();
        assert_eq!(h.a, SecretRef::File("/run/secrets/x".into()));
        assert!(h.vault.is_some());
        let h: Holder =
            toml::from_str("a = { vault = { path = \"secret/x\", field = \"f\" } }\n").unwrap();
        assert_eq!(
            h.a,
            SecretRef::Vault(VaultField {
                path: "secret/x".into(),
                field: "f".into()
            })
        );
        for bad in [
            "a = \"/run/secrets/x\"\n",
            "a = { other = \"x\" }\n",
            "a = { file = \"x\", vault = { path = \"p\", field = \"f\" } }\n",
            "a = { vault = { path = \"p\" } }\n",
            "a = { vault = { path = \"p\", field = \"f\", extra = 1 } }\n",
        ] {
            assert!(toml::from_str::<Holder>(bad).is_err(), "{bad}");
        }
        // JSON, for the signer.
        let r: SecretRef = serde_json::from_str(r#"{"file":"/x"}"#).unwrap();
        assert_eq!(r, SecretRef::File("/x".into()));
    }

    #[test]
    fn a_relative_file_is_the_configuration_directorys() {
        let mut r = SecretRef::File("token".into());
        r.rebase(Path::new("/etc/bezent"));
        assert_eq!(r, SecretRef::File("/etc/bezent/token".into()));
        let mut r = SecretRef::File("/abs".into());
        r.rebase(Path::new("/etc/bezent"));
        assert_eq!(r, SecretRef::File("/abs".into()));
    }

    fn source() -> StoreSource {
        toml::from_str(
            "host = \"db.example\"\ndatabase = \"bezent\"\nlogin = { file = \"l\" }\npassword = { file = \"p\" }\nsslrootcert = \"/etc/bezent/rds ca.pem\"\n",
        )
        .unwrap()
    }

    #[test]
    fn the_store_url_keeps_verify_full_and_the_root_certificate() {
        let url = source().url("bezent_core", "pw").unwrap();
        assert_eq!(
            url.as_str(),
            "postgres://bezent_core:pw@db.example:5432/bezent?sslmode=verify-full&sslrootcert=/etc/bezent/rds%20ca.pem"
        );
    }

    #[test]
    fn req_ky_1_the_store_url_encodes_whatever_the_login_and_password_hold() {
        let password = "p@ss:w/rd?x#y%z &=+é";
        let url = source().url("svc user@x", password).unwrap();
        let parsed = url::Url::parse(&url).unwrap();
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .unwrap()
                .into_owned()
        };
        assert_eq!(decode(parsed.username()), "svc user@x");
        assert_eq!(decode(parsed.password().unwrap()), password);
        assert_eq!(parsed.host_str(), Some("db.example"));
        assert_eq!(parsed.path(), "/bezent");
        let query: Vec<_> = parsed
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert_eq!(query[0], ("sslmode".into(), "verify-full".into()));
        assert_eq!(
            query[1],
            ("sslrootcert".into(), "/etc/bezent/rds ca.pem".into())
        );
    }

    #[test]
    fn a_source_that_cannot_make_a_url_is_refused() {
        let mut s = source();
        s.sslmode = "prefer".into();
        assert!(s.url("l", "p").is_err());
        let mut s = source();
        s.host = "db.example/evil".into();
        assert!(s.url("l", "p").is_err());
        let mut s = source();
        s.host = "[::1]".into();
        assert!(s.url("l", "p").unwrap().contains("@[::1]:5432/"));
        assert!(source().url("", "p").is_err());
    }
}

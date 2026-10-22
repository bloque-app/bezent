// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! How every service reaches PostgreSQL: TLS, driven by the database URL, failing closed.
//!
//! The pool is built with rustls (no OpenSSL). The URL carries the policy, as libpq's does:
//!
//! | `sslmode` | Behaviour |
//! |---|---|
//! | `disable` | no TLS |
//! | `require` | TLS, or the connection fails; the certificate is not checked unless `sslrootcert` names a CA bundle |
//! | `verify-ca` | TLS, certificate chain checked against `sslrootcert` (and the bundled web roots), host name not checked |
//! | `verify-full` | TLS, certificate chain and host name checked |
//!
//! `sslrootcert=/path/to/bundle.pem` names a PEM file of CA certificates (for AWS RDS,
//! `global-bundle.pem`); it is added to the bundled web roots. It is read when the connection is made.
//!
//! **No silent downgrade.** `allow` and `prefer` fall back to a plain connection when TLS cannot be
//! established, so the services refuse them, and refuse a URL with no `sslmode` at all, unless the
//! server is on this machine (a loopback address or a Unix socket: development and CI). A URL for
//! any other host must say `sslmode` explicitly. With `require`, `verify-ca` or `verify-full`, a
//! server that does not offer TLS, or whose certificate does not verify, ends the connection with an
//! error, and the service does not start.

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use std::str::FromStr;

/// Parses `url` into connection options, refusing a URL that would allow a plain connection by
/// accident (see the module documentation). The error never repeats the URL.
pub fn connect_options(url: &str) -> Result<PgConnectOptions, String> {
    let options = PgConnectOptions::from_str(url.trim())
        .map_err(|_| "the database URL is not a valid PostgreSQL URL".to_owned())?;
    let said = says_sslmode(url);
    let mode = options.get_ssl_mode();
    if !is_local(&options) {
        if !said {
            return Err(
                "the database URL names a remote server but no sslmode: add \
                 sslmode=disable, require, verify-ca or verify-full (verify-full with sslrootcert \
                 is the production setting)"
                    .to_owned(),
            );
        }
        if matches!(mode, PgSslMode::Allow | PgSslMode::Prefer) {
            return Err(
                "sslmode=allow and sslmode=prefer fall back to a plain connection: use \
                 disable, require, verify-ca or verify-full"
                    .to_owned(),
            );
        }
    }
    Ok(options)
}

/// Connects a pool made with `pool` to the store `url` names, over the TLS the URL asks for.
/// Fails closed.
pub async fn connect(pool: PgPoolOptions, url: &str) -> Result<PgPool, String> {
    let options = connect_options(url)?;
    pool.connect_with(options).await.map_err(|e| e.to_string())
}

/// Whether the URL's query carries an `sslmode` (libpq's spelling, or sqlx's `ssl-mode`).
fn says_sslmode(url: &str) -> bool {
    url.split_once('?').is_some_and(|(_, query)| {
        query
            .split('&')
            .filter_map(|pair| pair.split('=').next())
            .any(|key| key == "sslmode" || key == "ssl-mode")
    })
}

/// A server on this machine: a Unix socket or a loopback address.
fn is_local(options: &PgConnectOptions) -> bool {
    if options.get_socket().is_some() {
        return true;
    }
    let host = options.get_host().trim_matches(['[', ']']);
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(url: &str) -> String {
        connect_options(url).expect_err("the URL should be refused")
    }

    #[test]
    fn a_remote_url_must_say_its_sslmode() {
        assert!(refused("postgres://u:p@db.example.net:5432/bezent").contains("no sslmode"));
        assert!(
            refused("postgres://u:p@10.0.0.5/bezent?application_name=x").contains("no sslmode")
        );
    }

    #[test]
    fn a_remote_url_never_downgrades() {
        assert!(refused("postgres://u:p@db.example.net/b?sslmode=prefer").contains("plain"));
        assert!(refused("postgres://u:p@db.example.net/b?sslmode=allow").contains("plain"));
    }

    #[test]
    fn the_secure_modes_are_accepted_and_read_back() {
        for (spelling, mode) in [
            ("require", PgSslMode::Require),
            ("verify-ca", PgSslMode::VerifyCa),
            ("verify-full", PgSslMode::VerifyFull),
            ("disable", PgSslMode::Disable),
        ] {
            let url = format!("postgres://u:p@db.example.net/b?sslmode={spelling}");
            let options = connect_options(&url).expect(spelling);
            assert_eq!(
                format!("{:?}", options.get_ssl_mode()),
                format!("{mode:?}"),
                "{spelling}"
            );
        }
        let options = connect_options(
            "postgres://u:p@db.example.net/b?sslmode=verify-full&sslrootcert=/etc/bezent/rds-ca.pem",
        )
        .expect("verify-full with a CA bundle");
        assert!(matches!(options.get_ssl_mode(), PgSslMode::VerifyFull));
    }

    #[test]
    fn a_local_server_may_leave_it_out() {
        for url in [
            "postgres://u:p@localhost:5432/b",
            "postgres://u:p@127.0.0.1/b",
            "postgres://u:p@[::1]/b",
            "postgres://u@%2Fvar%2Frun%2Fpostgresql/b",
            "postgres://u:p@localhost/b?sslmode=prefer",
        ] {
            connect_options(url).unwrap_or_else(|e| panic!("{url}: {e}"));
        }
    }

    #[test]
    fn the_error_does_not_repeat_the_password() {
        let e = refused("not a url with secret-pw");
        assert!(!e.contains("secret-pw"), "{e}");
        let e = refused("postgres://u:secret-pw@db.example.net/b");
        assert!(!e.contains("secret-pw"), "{e}");
    }
}

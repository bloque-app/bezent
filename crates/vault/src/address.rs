// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Which Vault addresses are accepted: HTTPS, or plain HTTP to a loopback host (development and CI).
//! The check moved here from the signer's `vault.rs`, with its tests.

use crate::Error;

/// Whether `address` is plain HTTP to a loopback host. The host is the parsed one, so userinfo
/// tricks such as `http://localhost:80@evil.example` name `evil.example` and are not loopback.
pub fn loopback(address: &str) -> bool {
    let Ok(url) = url::Url::parse(address) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The base URL of the server, without a trailing slash, refusing plain HTTP off loopback.
pub fn base(address: &str) -> Result<String, Error> {
    if !(address.starts_with("https://") || loopback(address)) {
        return Err(Error::Config(
            "the Vault address must be https (REQ-KY-1)".to_owned(),
        ));
    }
    Ok(address.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_ky_1_vault_must_be_https_off_loopback() {
        assert!(base("https://vault.example:8200").is_ok());
        assert!(base("http://127.0.0.1:8200").is_ok());
        assert!(base("http://vault.example:8200").is_err());
        assert!(base("http://localhost.evil.example").is_err());
        assert_eq!(
            base("https://vault.example:8200/").unwrap(),
            "https://vault.example:8200"
        );
    }

    #[test]
    fn req_ky_1_loopback_is_judged_on_the_parsed_host() {
        for ok in [
            "http://localhost:8200",
            "http://LOCALHOST",
            "http://127.0.0.1:8200",
            "http://127.1.2.3:8200",
            "http://[::1]:8200",
            "http://[::1]",
        ] {
            assert!(loopback(ok), "{ok}");
        }
        for bad in [
            "http://localhost:80@evil.example",
            "http://localhost@evil.example:8200/",
            "http://127.0.0.1:80@evil.example",
            "http://[::1]:80@evil.example",
            "http://evil.example/@localhost",
            "http://evil.example#@localhost",
            "http://evil.example?@127.0.0.1",
            "http://vault.example:8200",
            "http://10.0.0.1:8200",
            "http://[::2]:8200",
            "http://0.0.0.0:8200",
            "ftp://localhost",
            "localhost:8200",
            "",
        ] {
            assert!(!loopback(bad), "{bad}");
        }
    }

    #[test]
    fn req_ky_1_a_userinfo_trick_is_not_accepted_as_plain_http() {
        assert!(base("http://localhost:80@evil.example").is_err());
        assert!(base("http://[::1]:8200").is_ok());
        assert!(base("http://vault.example").is_err());
    }
}

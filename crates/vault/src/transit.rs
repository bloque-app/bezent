// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Vault Transit, for an ed25519 key that never leaves Vault (the Fargate design's delivery-key
//! ruling `d-delivery-keys`): [`Session::transit_key`] reads a key's public half and
//! [`Session::transit_sign`] asks Vault to sign.
//!
//! The client checks that Vault is the signing service the plan asks for (one that signs on request
//! and never releases the key, so the key is never readable outside it, `REQ-CU-1`) before it
//! trusts the key: it must be an `ed25519` key that is not exportable, not derived, not allowed a
//! plaintext backup, and that signs. What is signed is the message itself
//! (`prehashed` false: pure Ed25519, RFC 8032), with the key version the caller names, and the
//! answer must be a signature of that version. The caller verifies the signature against the
//! public key (`INV-9`); this module only parses what Vault answers.
//!
//! Vault is asked for nothing but the public key and a signature: the message is never logged and
//! no error carries a body.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Method;
use serde::Deserialize;

use crate::Error;
use crate::client::{Session, plain_path};

/// A Transit key: its newest version and that version's public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitKey {
    /// The newest version.
    pub version: u32,
    /// Its public key, the 32 bytes of RFC 8032.
    pub public_key: [u8; 32],
}

#[derive(Deserialize)]
struct KeyAnswer {
    data: KeyData,
}

#[derive(Deserialize)]
struct KeyData {
    #[serde(rename = "type")]
    key_type: String,
    exportable: Option<bool>,
    #[serde(default)]
    derived: bool,
    #[serde(default)]
    allow_plaintext_backup: bool,
    #[serde(default = "yes")]
    supports_signing: bool,
    latest_version: u32,
    /// The versions' own descriptions: an ed25519 key's are objects with a `public_key`, another
    /// type's are not, and such a key is refused for its type before they are read.
    keys: serde_json::Value,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct SignAnswer {
    data: SignData,
}

#[derive(Deserialize)]
struct SignData {
    signature: String,
}

fn name_path(mount: &str, name: &str, operation: &str) -> Result<String, Error> {
    if !plain_path(mount) || !plain_path(name) || name.contains('/') {
        return Err(Error::Config(
            "the Transit mount or key is not a plain path".to_owned(),
        ));
    }
    Ok(format!("{mount}/{operation}/{name}"))
}

/// A Transit key as Vault describes it, checked ([`Session::transit_key`]).
fn parse_key(body: &[u8]) -> Result<TransitKey, Error> {
    let bad = Error::Malformed("the Transit key is not described as expected");
    let KeyAnswer { data } = serde_json::from_slice(body).map_err(|_| bad.clone())?;
    if data.key_type != "ed25519" {
        return Err(Error::Transit("the key is not an ed25519 key"));
    }
    // A key that can be exported has been released by Vault; one that is derived has no one public
    // key. An answer that does not say is not trusted.
    if data.exportable != Some(false) {
        return Err(Error::Transit(
            "the key may be exported, or Vault did not say",
        ));
    }
    if data.derived || data.allow_plaintext_backup {
        return Err(Error::Transit(
            "the key is derived or allows a plaintext backup",
        ));
    }
    if !data.supports_signing || data.latest_version == 0 {
        return Err(Error::Transit("the key does not sign"));
    }
    let public_key = data.keys[data.latest_version.to_string()]["public_key"]
        .as_str()
        .and_then(|b64| STANDARD.decode(b64).ok())
        .and_then(|raw| <[u8; 32]>::try_from(raw).ok())
        .ok_or(bad)?;
    Ok(TransitKey {
        version: data.latest_version,
        public_key,
    })
}

/// The 64 bytes of a Transit signature `vault:v<N>:<base64>`, if `N` is `version`.
fn parse_signature(body: &[u8], version: u32) -> Result<[u8; 64], Error> {
    let bad = Error::Malformed("the signature is not vault:v<N>:<base64> of 64 bytes");
    let SignAnswer { data } = serde_json::from_slice(body).map_err(|_| bad.clone())?;
    let mut parts = data.signature.splitn(3, ':');
    let (Some("vault"), Some(v), Some(b64)) = (parts.next(), parts.next(), parts.next()) else {
        return Err(bad);
    };
    let n: u32 = v
        .strip_prefix('v')
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| bad.clone())?;
    if n != version {
        return Err(Error::Transit(
            "Vault signed with another key version than the one asked for",
        ));
    }
    STANDARD
        .decode(b64)
        .ok()
        .and_then(|raw| <[u8; 64]>::try_from(raw).ok())
        .ok_or(bad)
}

impl Session {
    /// The ed25519 key `name` of the Transit engine at `mount`: its newest version and public key,
    /// after the checks of the module documentation.
    pub async fn transit_key(&self, mount: &str, name: &str) -> Result<TransitKey, Error> {
        let path = name_path(mount, name, "keys")?;
        let body = self
            .client
            .send("read", Method::GET, &path, Some(&self.token), None, true)
            .await?;
        parse_key(&body)
    }

    /// Asks Transit to sign `message` with version `version` of key `name`: pure Ed25519, not
    /// retried. The caller verifies the signature.
    pub async fn transit_sign(
        &self,
        mount: &str,
        name: &str,
        version: u32,
        message: &[u8],
    ) -> Result<[u8; 64], Error> {
        let path = name_path(mount, name, "sign")?;
        let body = serde_json::to_vec(&serde_json::json!({
            "input": STANDARD.encode(message),
            "prehashed": false,
            "key_version": version,
        }))
        .map_err(|_| Error::Config("the signing request is not usable".to_owned()))?;
        let answer = self
            .client
            .send(
                "sign",
                Method::POST,
                &path,
                Some(&self.token),
                Some(&body),
                false,
            )
            .await?;
        parse_signature(&answer, version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(extra: &str) -> Vec<u8> {
        let public = STANDARD.encode([7u8; 32]);
        format!(
            r#"{{"data":{{"type":"ed25519","exportable":false,"derived":false,"allow_plaintext_backup":false,
            "supports_signing":true,"latest_version":2,"min_available_version":0,
            "keys":{{"1":{{"public_key":"{}"}},"2":{{"public_key":"{public}"}}}}{extra}}}}}"#,
            STANDARD.encode([1u8; 32])
        )
        .into_bytes()
    }

    #[test]
    fn req_cu_1_the_latest_versions_public_key_is_read() {
        let parsed = parse_key(&key("")).unwrap();
        assert_eq!(
            parsed,
            TransitKey {
                version: 2,
                public_key: [7; 32]
            }
        );
    }

    #[test]
    fn req_cu_1_a_key_that_can_be_released_is_refused() {
        for (from, to) in [
            (r#""exportable":false"#, r#""exportable":true"#),
            (r#""derived":false"#, r#""derived":true"#),
            (
                r#""allow_plaintext_backup":false"#,
                r#""allow_plaintext_backup":true"#,
            ),
            (r#""supports_signing":true"#, r#""supports_signing":false"#),
            (r#""type":"ed25519""#, r#""type":"aes256-gcm96""#),
            (r#""exportable":false,"#, ""),
        ] {
            let text = String::from_utf8(key("")).unwrap().replace(from, to);
            assert!(
                matches!(parse_key(text.as_bytes()), Err(Error::Transit(_))),
                "{to}"
            );
        }
    }

    #[test]
    fn req_cu_1_a_malformed_key_description_is_refused() {
        for body in [
            "{}",
            "not json",
            r#"{"data":{"type":"ed25519","exportable":false,"latest_version":1,"keys":{}}}"#,
            r#"{"data":{"type":"ed25519","exportable":false,"latest_version":1,"keys":{"1":{"public_key":"AAAA"}}}}"#,
        ] {
            assert!(parse_key(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn inv_9_a_signature_is_parsed_only_for_the_version_asked() {
        let sig = |s: &str| format!(r#"{{"data":{{"signature":"{s}","key_version":1}}}}"#);
        let good = sig(&format!("vault:v1:{}", STANDARD.encode([5u8; 64])));
        assert_eq!(parse_signature(good.as_bytes(), 1).unwrap(), [5; 64]);
        assert!(matches!(
            parse_signature(good.as_bytes(), 2),
            Err(Error::Transit(_))
        ));
        for bad in [
            sig("vault:v1:AAAA"),
            sig("v1:AAAA"),
            sig(&format!("vault:1:{}", STANDARD.encode([5u8; 64]))),
            sig(&format!("vault:v1:{}", STANDARD.encode([5u8; 65]))),
            "{}".to_owned(),
        ] {
            assert!(parse_signature(bad.as_bytes(), 1).is_err(), "{bad}");
        }
    }
}

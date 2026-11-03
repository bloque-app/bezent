// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! A Vault signing plugin, for a secp256k1 key that never leaves Vault: the
//! `blockchain-signer-vault-plugin` secrets engine, mounted at a path the configuration names.
//! [`Session::plugin_sign_hash`] asks it to sign a 32-byte hash with key `<id>`
//! (`<mount>/keys/<id>/sign/hash`) and returns the DER signature it answers, which carries no
//! recovery id.
//!
//! The plugin hashes nothing and checks no length: it signs whatever bytes it is given. So the
//! caller computes the digest, and this client refuses, before any request, a hash that is not 32
//! bytes. The request carries the hash and nothing else; it is not retried (a signature request is
//! never sent twice). The caller turns the DER into the network's form, finds the recovery id and
//! verifies the signature against the key it knows (`INV-9`); this module only parses what Vault
//! answers.
//!
//! Only `sign/hash` is asked for: the role the caller logs in as needs `update` on
//! `<mount>/keys/<id>/sign/hash` and nothing else, not even a read of the key. No hash, signature
//! or token is logged or put in an error.

use reqwest::Method;
use serde::Deserialize;

use crate::Error;
use crate::client::{Session, plain_path};

/// The longest DER encoding of a secp256k1 ECDSA signature: two 33-byte integers and their
/// headers.
const MAX_DER: usize = 72;

#[derive(Deserialize)]
struct SignAnswer {
    data: SignData,
}

#[derive(Deserialize)]
struct SignData {
    signature: String,
}

/// `<mount>/keys/<key>/sign/hash`, if the mount is a plain path and the key a plain name.
fn sign_path(mount: &str, key: &str) -> Result<String, Error> {
    if !plain_path(mount) || !plain_path(key) || key.contains('/') {
        return Err(Error::Config(
            "the signing plugin's mount or key is not a plain path".to_owned(),
        ));
    }
    Ok(format!("{mount}/keys/{key}/sign/hash"))
}

/// The DER bytes of the plugin's answer: `data.signature`, hex, with or without `0x`.
fn parse_signature(body: &[u8]) -> Result<Vec<u8>, Error> {
    let bad = Error::Malformed("the signature is not a hex DER ECDSA signature");
    let SignAnswer { data } = serde_json::from_slice(body).map_err(|_| bad.clone())?;
    let digits = data.signature.strip_prefix("0x").unwrap_or(&data.signature);
    let der = hex_decode(digits).ok_or_else(|| bad.clone())?;
    // A DER SEQUENCE of two INTEGERs: at least 8 bytes, at most MAX_DER.
    if der.len() < 8 || der.len() > MAX_DER || der[0] != 0x30 {
        return Err(bad);
    }
    Ok(der)
}

fn hex_decode(digits: &str) -> Option<Vec<u8>> {
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(digits.get(i..i + 2)?, 16).ok())
        .collect()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Session {
    /// Asks the signing plugin at `mount` to sign `hash` with key `key`: the DER signature, not
    /// retried. Refuses a `hash` that is not 32 bytes before anything is sent. The caller verifies
    /// the signature.
    pub async fn plugin_sign_hash(
        &self,
        mount: &str,
        key: &str,
        hash: &[u8],
    ) -> Result<Vec<u8>, Error> {
        if hash.len() != 32 {
            return Err(Error::Plugin(
                "the hash to sign is not 32 bytes; the signer computes it",
            ));
        }
        let path = sign_path(mount, key)?;
        let body = serde_json::to_vec(&serde_json::json!({ "hash": hex_encode(hash) }))
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
        parse_signature(&answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_ky_1_the_path_is_the_keys_sign_hash_and_names_are_plain() {
        assert_eq!(
            sign_path("signer", "polygon-delivery").unwrap(),
            "signer/keys/polygon-delivery/sign/hash"
        );
        for (mount, key) in [
            ("signer", "a/b"),
            ("signer", ".."),
            ("signer", ""),
            ("", "k"),
            ("sig ner", "k"),
            ("signer", "k?x=1"),
        ] {
            assert!(
                matches!(sign_path(mount, key), Err(Error::Config(_))),
                "{mount} {key}"
            );
        }
    }

    #[test]
    fn inv_9_the_answer_is_hex_der_with_or_without_0x() {
        let der = "3044022001020304050607080910111213141516171819202122232425262728293031320220"
            .to_owned()
            + &"11".repeat(32);
        let answer = |s: &str| format!(r#"{{"data":{{"signature":"{s}"}}}}"#);
        let parsed = parse_signature(answer(&der).as_bytes()).unwrap();
        assert_eq!(parsed.len(), 70);
        assert_eq!(
            parse_signature(answer(&format!("0x{der}")).as_bytes()).unwrap(),
            parsed
        );
        for bad in [
            answer("zz"),
            answer("30"),
            answer(&"31".repeat(40)),
            answer(&format!("30{}", "00".repeat(80))),
            answer(&der[1..]),
            r#"{"data":{}}"#.to_owned(),
            "{}".to_owned(),
            "not json".to_owned(),
        ] {
            assert!(
                matches!(parse_signature(bad.as_bytes()), Err(Error::Malformed(_))),
                "{bad}"
            );
        }
    }
}

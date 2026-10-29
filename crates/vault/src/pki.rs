// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Certificates from Vault's PKI engine: `POST <mount>/issue/<role>`.
//!
//! The engine makes the key pair and hands the private key back once. Nothing it answers is taken
//! on trust: [`verify`] checks that a private key came back, that it is the key the certificate
//! names, and that the certificate lives as long as it was asked to (a role's `max_ttl` shortens a
//! request without an error, and a certificate shorter than planned must not be mistaken for the
//! one that was). The TLS link that uses these (PUB-1b) is not here.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair, Ed25519KeyPair,
    KeyPair, RsaKeyPair,
};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::Error;

/// How far Vault's idea of the time may differ from the caller's: the certificate's `NotAfter` must
/// be within this of `now + ttl`. Vault backdates `NotBefore` by 30 seconds, so `NotBefore` is
/// allowed to be early but not late.
pub const TOLERANCE: Duration = Duration::from_secs(120);

/// What to ask a role to issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRequest {
    /// The one name the role allows (`common_name`).
    pub common_name: String,
    /// How long the certificate should live.
    pub ttl: Duration,
}

/// The body of `POST <mount>/issue/<role>`.
#[derive(Serialize)]
pub(crate) struct IssueBody<'a> {
    common_name: &'a str,
    ttl: String,
    format: &'static str,
    private_key_format: &'static str,
}

impl<'a> IssueBody<'a> {
    pub(crate) fn new(request: &'a IssueRequest) -> Self {
        Self {
            common_name: &request.common_name,
            ttl: format!("{}s", request.ttl.as_secs()),
            format: "pem",
            private_key_format: "pkcs8",
        }
    }
}

/// The `data` of the answer.
#[derive(Deserialize)]
pub(crate) struct IssueData {
    certificate: String,
    #[serde(default)]
    ca_chain: Vec<String>,
    #[serde(default)]
    issuing_ca: Option<String>,
    #[serde(default)]
    private_key: Option<Zeroizing<String>>,
    #[serde(default)]
    serial_number: String,
    #[serde(default)]
    expiration: Option<u64>,
}

/// The answer's envelope.
#[derive(Deserialize)]
pub(crate) struct IssueAnswer {
    pub(crate) data: IssueData,
}

/// A certificate Vault issued, with the key it belongs to, after [`verify`].
pub struct Issued {
    /// The leaf certificate, PEM.
    pub certificate_pem: String,
    /// The leaf certificate, DER.
    pub certificate_der: Vec<u8>,
    /// The chain above the leaf, PEM, as Vault gave it (the issuing CA when it gave no chain).
    pub ca_chain_pem: Vec<String>,
    /// The private key, PKCS#8 DER.
    pub private_key_pkcs8: Zeroizing<Vec<u8>>,
    /// The certificate's serial number, as Vault writes it.
    pub serial_number: String,
    /// When the certificate expires.
    pub not_after: SystemTime,
}

impl fmt::Debug for Issued {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Issued")
            .field("serial_number", &self.serial_number)
            .field("not_after", &self.not_after)
            .field("private_key_pkcs8", &"<redacted>")
            .finish_non_exhaustive()
    }
}

/// Checks `data` against what was asked at `now`, and returns the certificate with its key.
pub(crate) fn verify(
    data: IssueData,
    request: &IssueRequest,
    now: SystemTime,
) -> Result<Issued, Error> {
    let pem_key = data
        .private_key
        .as_ref()
        .filter(|k| !k.trim().is_empty())
        .ok_or(Error::Pki("Vault returned no private key"))?;
    let key =
        pem_block(pem_key, "PRIVATE KEY").ok_or(Error::Pki("the private key is not PKCS#8 PEM"))?;
    let certificate_der = pem_block(&data.certificate, "CERTIFICATE")
        .ok_or(Error::Pki("the certificate is not PEM"))?;
    let certificate = Certificate::parse(&certificate_der).ok_or(Error::Pki(
        "the certificate is not a valid X.509 certificate",
    ))?;
    let public =
        public_key(&key).ok_or(Error::Pki("the private key is not one that can be used"))?;
    if public.as_slice() != certificate.public_key {
        return Err(Error::Pki("the private key is not the certificate's key"));
    }
    let after = unix(certificate.not_after);
    let before = unix(certificate.not_before);
    let asked = unix(now) + request.ttl.as_secs() as i64;
    if (after - asked).unsigned_abs() > TOLERANCE.as_secs() {
        return Err(Error::Pki(
            "the certificate does not live as long as it was asked to",
        ));
    }
    if before > unix(now) + TOLERANCE.as_secs() as i64 {
        return Err(Error::Pki("the certificate is not valid yet"));
    }
    if data
        .expiration
        .is_some_and(|e| i64::try_from(e).ok() != Some(after))
    {
        return Err(Error::Pki(
            "the expiration Vault states is not the certificate's",
        ));
    }
    let ca_chain_pem = if data.ca_chain.is_empty() {
        data.issuing_ca.into_iter().collect()
    } else {
        data.ca_chain
    };
    Ok(Issued {
        certificate_pem: data.certificate,
        certificate_der: certificate_der.to_vec(),
        ca_chain_pem,
        private_key_pkcs8: key,
        serial_number: data.serial_number,
        not_after: certificate.not_after,
    })
}

fn unix(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// The first PEM block labelled `label`, decoded. Zeroizing, because it may be a key.
fn pem_block(text: &str, label: &str) -> Option<Zeroizing<Vec<u8>>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let start = text.find(&begin)? + begin.len();
    let stop = start + text[start..].find(&end)?;
    let body: String = text[start..stop]
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let body = Zeroizing::new(body);
    STANDARD.decode(body.as_bytes()).ok().map(Zeroizing::new)
}

/// The public key of a PKCS#8 private key, as an X.509 `subjectPublicKey` holds it: the uncompressed
/// point for ECDSA, the raw 32 bytes for Ed25519, an `RSAPublicKey` for RSA.
fn public_key(pkcs8: &[u8]) -> Option<Vec<u8>> {
    let rng = SystemRandom::new();
    for alg in [
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &ECDSA_P384_SHA384_ASN1_SIGNING,
    ] {
        if let Ok(pair) = EcdsaKeyPair::from_pkcs8(alg, pkcs8, &rng) {
            return Some(pair.public_key().as_ref().to_vec());
        }
    }
    if let Ok(pair) = Ed25519KeyPair::from_pkcs8(pkcs8) {
        return Some(pair.public_key().as_ref().to_vec());
    }
    RsaKeyPair::from_pkcs8(pkcs8)
        .ok()
        .map(|pair| pair.public_key().as_ref().to_vec())
}

/// The few fields of an X.509 certificate that are checked.
pub(crate) struct Certificate<'a> {
    not_before: SystemTime,
    not_after: SystemTime,
    /// The `subjectPublicKey` bits, without the unused-bits byte.
    public_key: &'a [u8],
}

/// Reads one DER element of a single-byte tag: `(tag, content, rest)`.
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let length = rest[..n].iter().fold(0usize, |l, &b| (l << 8) | b as usize);
        (length, &rest[n..])
    };
    if rest.len() < length {
        return None;
    }
    Some((tag, &rest[..length], &rest[length..]))
}

fn expect(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (found, content, rest) = element(input)?;
    (found == tag).then_some((content, rest))
}

#[cfg(test)]
impl Certificate<'_> {
    pub(crate) fn parse_for_tests(der: &[u8]) -> Option<Certificate<'_>> {
        Certificate::parse(der)
    }
}

impl<'a> Certificate<'a> {
    fn parse(der: &'a [u8]) -> Option<Self> {
        let (certificate, _) = expect(der, 0x30)?;
        let (tbs, _) = expect(certificate, 0x30)?;
        // [0] version (optional), serial, signature algorithm, issuer.
        let mut rest = tbs;
        if rest.first() == Some(&0xa0) {
            rest = expect(rest, 0xa0)?.1;
        }
        let (_, rest) = expect(rest, 0x02)?;
        let (_, rest) = expect(rest, 0x30)?;
        let (_, rest) = expect(rest, 0x30)?;
        let (validity, rest) = expect(rest, 0x30)?;
        let (_, rest) = expect(rest, 0x30)?;
        let (spki, _) = expect(rest, 0x30)?;
        let (not_before, validity) = time(validity)?;
        let (not_after, _) = time(validity)?;
        let (_, spki) = expect(spki, 0x30)?;
        let (bits, _) = expect(spki, 0x03)?;
        let (&unused, public_key) = bits.split_first()?;
        (unused == 0).then_some(Self {
            not_before,
            not_after,
            public_key,
        })
    }
}

/// A `UTCTime` or `GeneralizedTime`, in Zulu, to a time, and the rest.
fn time(input: &[u8]) -> Option<(SystemTime, &[u8])> {
    let (tag, content, rest) = element(input)?;
    let text = std::str::from_utf8(content).ok()?;
    let digits = text.strip_suffix('Z')?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let number = |range: std::ops::Range<usize>| digits.get(range)?.parse::<i64>().ok();
    let (year, at) = match (tag, digits.len()) {
        (0x17, 12) => {
            let yy = number(0..2)?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, 2)
        }
        (0x18, 14) => (number(0..4)?, 4),
        _ => return None,
    };
    let (month, day) = (number(at..at + 2)?, number(at + 2..at + 4)?);
    let (hour, minute, second) = (
        number(at + 4..at + 6)?,
        number(at + 6..at + 8)?,
        number(at + 8..at + 10)?,
    );
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second;
    let time = if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    };
    Some((time, rest))
}

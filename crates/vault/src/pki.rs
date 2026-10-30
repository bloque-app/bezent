// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Certificates from Vault's PKI engine: `POST <mount>/issue/<role>`.
//!
//! The engine makes the key pair and hands the private key back once. Nothing it answers is taken
//! on trust: [`verify`] checks that a private key came back, that it is the key the certificate
//! names, and that the certificate lives as long as it was asked to (a role's `max_ttl` shortens a
//! request without an error, and a certificate shorter than planned must not be mistaken for the
//! one that was). It also checks what the certificate says: the subject's common name is the name
//! asked for and the only subject alternative name is that DNS name (no other name, no IP address),
//! the extended key usage is the one the role is for (server or client, nothing else) when the
//! certificate carries one, the key usage allows signatures and not signing certificates, and the
//! certificate is not a CA. Last, the leaf's signature is verified against the issuing CA Vault
//! returned, so the pair is consistent; but that CA arrives over the same TLS connection as the
//! leaf, so trust in the chain still rests on TLS to Vault (the link's root is pinned separately,
//! by the caller of PUB-1b). The TLS link that uses these is not here.

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

/// What a role is for: the extended key usage its certificates may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Usage {
    /// `serverAuth`: the signer's role.
    Server,
    /// `clientAuth`: the callers' roles.
    Client,
}

/// What to ask a role to issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRequest {
    /// The one name the role allows (`common_name`).
    pub common_name: String,
    /// How long the certificate should live.
    pub ttl: Duration,
    /// What the role is for.
    pub usage: Usage,
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
    check_name_and_use(&certificate, request)?;
    let issuing = data
        .issuing_ca
        .as_deref()
        .and_then(|pem| pem_block(pem, "CERTIFICATE"))
        .ok_or(Error::Pki("Vault returned no issuing CA"))?;
    let issuer = Certificate::parse(&issuing).ok_or(Error::Pki(
        "the issuing CA is not a valid X.509 certificate",
    ))?;
    certificate.verify_signed_by(&issuer)?;
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

/// A name in the subject alternative names.
#[derive(Debug, PartialEq, Eq)]
enum San {
    /// A DNS name.
    Dns(String),
    /// Anything else: an IP address, an e-mail address, a URI.
    Other,
}

/// The few fields of an X.509 certificate that are checked.
pub(crate) struct Certificate<'a> {
    not_before: SystemTime,
    not_after: SystemTime,
    /// The `subjectPublicKey` bits, without the unused-bits byte.
    public_key: &'a [u8],
    /// The `tbsCertificate`, whole, which the signature covers.
    tbs: &'a [u8],
    /// The OID (content bytes) of the signature algorithm.
    signature_algorithm: &'a [u8],
    /// The signature bits, without the unused-bits byte.
    signature: &'a [u8],
    /// Every common name in the subject.
    common_names: Vec<String>,
    /// The subject alternative names, if the extension is there.
    sans: Option<Vec<San>>,
    /// The extended key usage OIDs (content bytes), if the extension is there.
    extended_key_usage: Option<Vec<&'a [u8]>>,
    /// The first byte of the key usage bits, if the extension is there.
    key_usage: Option<u8>,
    /// Whether basicConstraints says this is a CA.
    ca: bool,
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

/// The elements of a sequence's content, as `(tag, content)`.
fn elements(mut input: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    while !input.is_empty() {
        let (tag, content, rest) = element(input)?;
        out.push((tag, content));
        input = rest;
    }
    Some(out)
}

const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
const OID_EKU: &[u8] = &[0x55, 0x1d, 0x25];
const OID_SERVER_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
const OID_CLIENT_AUTH: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02];

impl<'a> Certificate<'a> {
    pub(crate) fn parse(der: &'a [u8]) -> Option<Self> {
        let (certificate, _) = expect(der, 0x30)?;
        let (tag, tbs_content, after_tbs) = element(certificate)?;
        if tag != 0x30 {
            return None;
        }
        let tbs = &certificate[..certificate.len() - after_tbs.len()];
        let (algorithm, rest) = expect(after_tbs, 0x30)?;
        let (signature_algorithm, _) = expect(algorithm, 0x06)?;
        let (signature_bits, _) = expect(rest, 0x03)?;
        let (&unused, signature) = signature_bits.split_first()?;
        if unused != 0 {
            return None;
        }
        // [0] version (optional), serial, signature algorithm, issuer, validity, subject, spki.
        let mut rest = tbs_content;
        if rest.first() == Some(&0xa0) {
            rest = expect(rest, 0xa0)?.1;
        }
        let (_, rest) = expect(rest, 0x02)?;
        let (_, rest) = expect(rest, 0x30)?;
        let (_, rest) = expect(rest, 0x30)?;
        let (validity, rest) = expect(rest, 0x30)?;
        let (subject, rest) = expect(rest, 0x30)?;
        let (spki, mut rest) = expect(rest, 0x30)?;
        let (not_before, validity) = time(validity)?;
        let (not_after, _) = time(validity)?;
        let (_, spki) = expect(spki, 0x30)?;
        let (bits, _) = expect(spki, 0x03)?;
        let (&unused, public_key) = bits.split_first()?;
        if unused != 0 {
            return None;
        }
        let mut common_names = Vec::new();
        for (set_tag, set) in elements(subject)? {
            if set_tag != 0x31 {
                return None;
            }
            for (seq_tag, attribute) in elements(set)? {
                let (oid, value) = expect(attribute, 0x06).filter(|_| seq_tag == 0x30)?;
                if oid == OID_CN {
                    let (_, text, _) = element(value)?;
                    common_names.push(String::from_utf8(text.to_vec()).ok()?);
                }
            }
        }
        // Optional unique ids ([1], [2]), then the extensions ([3]).
        while matches!(rest.first(), Some(0x81 | 0x82)) {
            rest = element(rest)?.2;
        }
        let mut found = Self {
            not_before,
            not_after,
            public_key,
            tbs,
            signature_algorithm,
            signature,
            common_names,
            sans: None,
            extended_key_usage: None,
            key_usage: None,
            ca: false,
        };
        if rest.first() == Some(&0xa3) {
            let (extensions, _) = expect(rest, 0xa3)?;
            let (extensions, _) = expect(extensions, 0x30)?;
            for (tag, extension) in elements(extensions)? {
                if tag != 0x30 {
                    return None;
                }
                let (oid, mut after) = expect(extension, 0x06)?;
                if after.first() == Some(&0x01) {
                    after = element(after)?.2; // critical
                }
                let (value, _) = expect(after, 0x04)?;
                match oid {
                    OID_SAN => {
                        let (names, _) = expect(value, 0x30)?;
                        found.sans = Some(
                            elements(names)?
                                .into_iter()
                                .map(|(tag, name)| match tag {
                                    0x82 => String::from_utf8(name.to_vec()).ok().map(San::Dns),
                                    _ => Some(San::Other),
                                })
                                .collect::<Option<_>>()?,
                        );
                    }
                    OID_EKU => {
                        let (usages, _) = expect(value, 0x30)?;
                        found.extended_key_usage = Some(
                            elements(usages)?
                                .into_iter()
                                .map(|(tag, oid)| (tag == 0x06).then_some(oid))
                                .collect::<Option<_>>()?,
                        );
                    }
                    OID_KEY_USAGE => {
                        let (bits, _) = expect(value, 0x03)?;
                        found.key_usage = Some(bits.get(1).copied().unwrap_or(0));
                    }
                    OID_BASIC_CONSTRAINTS => {
                        let (constraints, _) = expect(value, 0x30)?;
                        found.ca = matches!(element(constraints), Some((0x01, [0xff], _)));
                    }
                    _ => {}
                }
            }
        }
        Some(found)
    }

    /// Verifies this certificate's signature with `issuer`'s key.
    fn verify_signed_by(&self, issuer: &Certificate<'_>) -> Result<(), Error> {
        use ring::signature as sig;
        let algorithm: &dyn sig::VerificationAlgorithm = match self.signature_algorithm {
            [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02] => &sig::ECDSA_P256_SHA256_ASN1,
            [0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03] => &sig::ECDSA_P384_SHA384_ASN1,
            [0x2b, 0x65, 0x70] => &sig::ED25519,
            [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b] => {
                &sig::RSA_PKCS1_2048_8192_SHA256
            }
            _ => {
                return Err(Error::Pki(
                    "the certificate's signature algorithm is not supported",
                ));
            }
        };
        sig::UnparsedPublicKey::new(algorithm, issuer.public_key)
            .verify(self.tbs, self.signature)
            .map_err(|_| Error::Pki("the certificate is not signed by the issuing CA"))
    }
}

/// The name and the use: the subject, the names, the usages and the CA flag.
fn check_name_and_use(certificate: &Certificate<'_>, request: &IssueRequest) -> Result<(), Error> {
    let name = request.common_name.as_str();
    if certificate.common_names.len() != 1
        || !certificate.common_names[0].eq_ignore_ascii_case(name)
    {
        return Err(Error::Pki("the common name is not the name asked for"));
    }
    match certificate.sans.as_deref() {
        Some([San::Dns(dns)]) if dns.eq_ignore_ascii_case(name) => {}
        _ => {
            return Err(Error::Pki(
                "the subject alternative names are not exactly the name asked for",
            ));
        }
    }
    if let Some(usages) = &certificate.extended_key_usage {
        let wanted = match request.usage {
            Usage::Server => OID_SERVER_AUTH,
            Usage::Client => OID_CLIENT_AUTH,
        };
        if usages.as_slice() != [wanted] {
            return Err(Error::Pki(
                "the extended key usage is not the one the role is for",
            ));
        }
    }
    // digitalSignature (bit 0) set; keyCertSign (bit 5) and cRLSign (bit 6) clear.
    if certificate
        .key_usage
        .is_some_and(|bits| bits & 0x80 == 0 || bits & 0x06 != 0)
    {
        return Err(Error::Pki("the key usage does not fit a leaf that signs"));
    }
    if certificate.ca {
        return Err(Error::Pki("the certificate is a CA"));
    }
    Ok(())
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

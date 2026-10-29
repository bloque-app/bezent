// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The request Vault's AWS auth method (`auth_type = iam`) takes to prove who the caller is: a
//! signed `sts:GetCallerIdentity` call that Vault forwards to STS. The service never sends it to
//! STS itself.
//!
//! The call is signed with AWS Signature Version 4 under the task role's credentials. The server
//! ID header (`X-Vault-AWS-IAM-Server-ID`) is among the signed headers, so a request meant for one
//! Vault cannot be replayed to another. The signature, the session token and the whole request are
//! secrets for as long as they are valid: they are held in zeroizing buffers and never logged.

use std::collections::BTreeMap;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use zeroize::Zeroizing;

use crate::Error;

/// The body of `sts:GetCallerIdentity`.
pub(crate) const BODY: &str = "Action=GetCallerIdentity&Version=2011-06-15";

/// The content type STS expects with it.
const CONTENT_TYPE: &str = "application/x-www-form-urlencoded; charset=utf-8";

/// The header that names the Vault the request is for.
pub(crate) const SERVER_ID_HEADER: &str = "X-Vault-AWS-IAM-Server-ID";

/// The body of `POST auth/<mount>/login`.
#[derive(Serialize)]
pub(crate) struct Login {
    role: String,
    iam_http_request_method: &'static str,
    iam_request_url: String,
    iam_request_body: String,
    iam_request_headers: String,
}

/// What a request is signed for.
pub(crate) struct Scope<'a> {
    pub(crate) region: &'a str,
    pub(crate) service: &'a str,
    pub(crate) time: SystemTime,
}

/// Signs `request` for `service` in `region` under `credentials` at `time`, and returns the headers
/// to add to it: `x-amz-date`, `authorization`, and `x-amz-security-token` for a session. `headers`
/// are signed as they are. Generic over the service so the published test vectors (which use a
/// made-up one) can check it.
pub(crate) fn sigv4(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
    credentials: &Credentials,
    scope: &Scope<'_>,
) -> Result<Vec<(String, Zeroizing<String>)>, Error> {
    fn fail<E>(_: E) -> Error {
        Error::Credentials("the login request could not be signed")
    }
    let identity = Identity::from(credentials.clone());
    let params: aws_sigv4::http_request::SigningParams<'_> = v4::SigningParams::builder()
        .identity(&identity)
        .region(scope.region)
        .name(scope.service)
        .time(scope.time)
        .settings(SigningSettings::default())
        .build()
        .map_err(fail)?
        .into();
    let signable = SignableRequest::new(
        method,
        url,
        headers.iter().copied(),
        SignableBody::Bytes(body),
    )
    .map_err(fail)?;
    let (instructions, _signature) = sign(signable, &params).map_err(fail)?.into_parts();
    Ok(instructions
        .headers()
        .map(|(name, value)| (name.to_owned(), Zeroizing::new(value.to_owned())))
        .collect())
}

/// The login body for `role`: a `GetCallerIdentity` call to the STS endpoint of `region`, signed
/// with `credentials` at `time`, carrying `server_id`.
pub(crate) fn login(
    credentials: &Credentials,
    role: &str,
    region: &str,
    server_id: &str,
    time: SystemTime,
) -> Result<Zeroizing<Vec<u8>>, Error> {
    if region.is_empty()
        || !region
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::Config("vault.sts_region is not a region".to_owned()));
    }
    if server_id.is_empty() || server_id.chars().any(|c| c.is_control()) {
        return Err(Error::Config("vault.server_id is not usable".to_owned()));
    }
    let host = format!("sts.{region}.amazonaws.com");
    let url = format!("https://{host}/");
    let length = BODY.len().to_string();
    let signed_in = [
        ("content-length", length.as_str()),
        ("content-type", CONTENT_TYPE),
        ("host", host.as_str()),
        ("x-vault-aws-iam-server-id", server_id),
    ];
    let added = sigv4(
        "POST",
        &url,
        &signed_in,
        BODY.as_bytes(),
        credentials,
        &Scope {
            region,
            service: "sts",
            time,
        },
    )?;
    // Vault takes a map of header name to a list of values, as Go's http.Header marshals. The
    // map borrows, so the signature is copied nowhere but into the zeroizing JSON.
    let mut map: BTreeMap<&str, [&str; 1]> = BTreeMap::new();
    map.insert("Content-Length", [length.as_str()]);
    map.insert("Content-Type", [CONTENT_TYPE]);
    map.insert("Host", [host.as_str()]);
    map.insert(SERVER_ID_HEADER, [server_id]);
    for (name, value) in &added {
        let canonical = match name.as_str() {
            "x-amz-date" => "X-Amz-Date",
            "authorization" => "Authorization",
            "x-amz-security-token" => "X-Amz-Security-Token",
            _ => return Err(Error::Credentials("the login request could not be signed")),
        };
        map.insert(canonical, [value.as_str()]);
    }
    let headers = Zeroizing::new(
        serde_json::to_string(&map)
            .map_err(|_| Error::Credentials("the login request could not be signed"))?,
    );
    drop(map);
    let body = Login {
        role: role.to_owned(),
        iam_http_request_method: "POST",
        iam_request_url: STANDARD.encode(url),
        iam_request_body: STANDARD.encode(BODY),
        iam_request_headers: STANDARD.encode(headers.as_bytes()),
    };
    let json = serde_json::to_vec(&body)
        .map_err(|_| Error::Credentials("the login request could not be signed"))?;
    Ok(Zeroizing::new(json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    const KEY: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";

    fn header<'a>(headers: &'a [(String, Zeroizing<String>)], name: &str) -> &'a str {
        headers
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no {name}"))
            .1
            .as_str()
    }

    /// The AWS Signature Version 4 test suite's `get-vanilla` case (made-up service, 2015): the
    /// published signature, so the signing is checked against AWS's own answer.
    #[test]
    fn req_ky_1_sigv4_matches_the_published_get_vanilla_vector() {
        let credentials = Credentials::new(KEY, SECRET, None, None, "test");
        let time = UNIX_EPOCH + Duration::from_secs(1_440_938_160); // 20150830T123600Z
        let added = sigv4(
            "GET",
            "https://example.amazonaws.com/",
            &[("host", "example.amazonaws.com")],
            b"",
            &credentials,
            &Scope {
                region: "us-east-1",
                service: "service",
                time,
            },
        )
        .unwrap();
        assert_eq!(header(&added, "x-amz-date"), "20150830T123600Z");
        assert_eq!(
            header(&added, "authorization"),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// The `GetCallerIdentity` login request, signed with a session token: the signature was
    /// computed by an independent implementation (a 20-line stdlib-only Python `hmac`/`hashlib`
    /// SigV4, which reproduces the vector above), not by this crate.
    #[test]
    fn req_ky_1_the_login_request_has_the_known_signature() {
        let credentials =
            Credentials::new(KEY, SECRET, Some("SESSIONTOKEN".to_owned()), None, "test");
        let time = UNIX_EPOCH + Duration::from_secs(1_793_275_200); // 20261029T120000Z
        let body = login(
            &credentials,
            "bezent-core-iam",
            "us-east-2",
            "vault.example",
            time,
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["role"], "bezent-core-iam");
        assert_eq!(v["iam_http_request_method"], "POST");
        let decode =
            |k: &str| String::from_utf8(STANDARD.decode(v[k].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(
            decode("iam_request_url"),
            "https://sts.us-east-2.amazonaws.com/"
        );
        assert_eq!(decode("iam_request_body"), BODY);
        let headers: serde_json::Value =
            serde_json::from_str(&decode("iam_request_headers")).unwrap();
        assert_eq!(
            headers["Authorization"][0],
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261029/us-east-2/sts/aws4_request, \
             SignedHeaders=content-length;content-type;host;x-amz-date;x-amz-security-token;x-vault-aws-iam-server-id, \
             Signature=f8ee107b104bd1c8578c94bdcccf1dd941575dad2bd1b5c11f71838666e4b32a"
        );
        assert_eq!(headers["X-Vault-AWS-IAM-Server-ID"][0], "vault.example");
        assert_eq!(headers["X-Amz-Date"][0], "20261029T120000Z");
        assert_eq!(headers["X-Amz-Security-Token"][0], "SESSIONTOKEN");
        assert_eq!(headers["Host"][0], "sts.us-east-2.amazonaws.com");
        assert_eq!(headers["Content-Length"][0], "43");
    }

    #[test]
    fn a_region_or_server_id_that_could_redirect_the_request_is_refused() {
        let credentials = Credentials::new(KEY, SECRET, None, None, "test");
        let now = UNIX_EPOCH;
        for region in ["", "us-east-2.evil.example", "US", "a/b", "x@y"] {
            assert!(
                login(&credentials, "r", region, "s", now).is_err(),
                "{region}"
            );
        }
        assert!(login(&credentials, "r", "us-east-2", "", now).is_err());
        assert!(login(&credentials, "r", "us-east-2", "a\r\nb", now).is_err());
    }
}

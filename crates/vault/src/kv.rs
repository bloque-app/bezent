// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Reading what a KV mount answers: version 1 puts the secret under `.data`, version 2 under
//! `.data.data` (next to `.data.metadata`). Both are read, version 2 first, as the signer's loader
//! did.
//!
//! The answer is deserialised straight into the type the caller names, or through [`Pick`], which
//! keeps one string field as a [`Zeroizing`] string and skips every other. No `serde_json::Value`
//! and no untagged enum is ever built, because both buffer the secret in memory that is never
//! zeroized.

use std::fmt;
use std::marker::PhantomData;

use serde::Deserializer;
use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use zeroize::Zeroizing;

use crate::Error;

/// A seed that keeps the string field of the given name from a map, and skips the others. `None`
/// when the map has no such field.
#[derive(Clone, Copy)]
pub(crate) struct Pick<'a>(pub &'a str);

impl<'de> DeserializeSeed<'de> for Pick<'_> {
    type Value = Option<Zeroizing<String>>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(PickVisitor(self.0))
    }
}

struct PickVisitor<'a>(&'a str);

impl<'de> Visitor<'de> for PickVisitor<'_> {
    type Value = Option<Zeroizing<String>>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map of fields")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut found = None;
        while let Some(key) = map.next_key::<String>()? {
            if key == self.0 {
                found = Some(map.next_value::<Zeroizing<String>>()?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(found)
    }
}

/// `{ "data": <seed>, ... }`: KV version 1.
struct V1<S>(S);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for V1<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(V1Visitor(self.0))
    }
}

struct V1Visitor<S>(S);

impl<'de, S: DeserializeSeed<'de>> Visitor<'de> for V1Visitor<S> {
    type Value = S::Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a KV version 1 answer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seed = Some(self.0);
        let mut value = None;
        while let Some(key) = map.next_key::<String>()? {
            match (key.as_str(), seed.take()) {
                ("data", Some(s)) => value = Some(map.next_value_seed(s)?),
                (_, s) => {
                    seed = s;
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        value.ok_or_else(|| serde::de::Error::missing_field("data"))
    }
}

/// `{ "data": { "data": <seed>, "metadata": {...} }, ... }`: KV version 2.
struct V2<S>(S);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for V2<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        V1(Inner(self.0)).deserialize(deserializer)
    }
}

struct Inner<S>(S);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for Inner<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(InnerVisitor(self.0))
    }
}

struct InnerVisitor<S>(S);

impl<'de, S: DeserializeSeed<'de>> Visitor<'de> for InnerVisitor<S> {
    type Value = S::Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a KV version 2 answer")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seed = Some(self.0);
        let (mut value, mut metadata) = (None, false);
        while let Some(key) = map.next_key::<String>()? {
            match (key.as_str(), seed.take()) {
                ("data", Some(s)) => value = Some(map.next_value_seed(s)?),
                ("metadata", s) => {
                    seed = s;
                    map.next_value::<IgnoredAny>()?;
                    metadata = true;
                }
                (_, s) => {
                    seed = s;
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if !metadata {
            return Err(serde::de::Error::missing_field("metadata"));
        }
        value.ok_or_else(|| serde::de::Error::missing_field("data"))
    }
}

/// Runs `seed` over the secret in `body`, trying KV version 2 and then version 1.
pub(crate) fn unwrap<S, T>(body: &[u8], seed: S) -> Result<T, Error>
where
    S: for<'de> DeserializeSeed<'de, Value = T> + Clone,
{
    let v2 = (|| -> Result<T, serde_json::Error> {
        let mut de = serde_json::Deserializer::from_slice(body);
        let value = V2(seed.clone()).deserialize(&mut de)?;
        de.end()?;
        Ok(value)
    })();
    match v2 {
        Ok(value) => Ok(value),
        Err(_) => {
            let mut de = serde_json::Deserializer::from_slice(body);
            let value = V1(seed)
                .deserialize(&mut de)
                .map_err(|_| Error::Malformed("the answer is not a KV secret"))?;
            de.end()
                .map_err(|_| Error::Malformed("the answer is not a KV secret"))?;
            Ok(value)
        }
    }
}

/// The secret in `body` as a `T`.
pub(crate) fn secret<T: DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    unwrap(body, PhantomData::<T>)
}

/// The string field `field` of the secret in `body`, or `None` when it has none.
pub(crate) fn field(body: &[u8], field: &str) -> Result<Option<Zeroizing<String>>, Error> {
    unwrap(body, Pick(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    const V1_BODY: &str = r#"{"request_id":"r","lease_id":"","renewable":false,"lease_duration":2764800,"data":{"login":"svc","password":"pw","n":3},"wrap_info":null,"warnings":null,"auth":null}"#;
    const V2_BODY: &str = r#"{"request_id":"r","data":{"data":{"login":"svc","password":"pw","n":3},"metadata":{"version":3,"destroyed":false}}}"#;

    #[test]
    fn req_ky_1_a_field_is_picked_from_kv_v1_and_v2() {
        for body in [V1_BODY, V2_BODY] {
            let f = field(body.as_bytes(), "password").unwrap().unwrap();
            assert_eq!(f.as_str(), "pw");
            assert!(field(body.as_bytes(), "absent").unwrap().is_none());
        }
    }

    #[test]
    fn req_ky_1_a_non_string_field_or_a_malformed_answer_is_refused() {
        for body in [V1_BODY, V2_BODY] {
            assert!(
                !matches!(field(body.as_bytes(), "n"), Ok(Some(_))),
                "a number is no secret"
            );
        }
        for body in [
            "{}",
            r#"{"data":null}"#,
            r#"{"data":[]}"#,
            r#"{"data":{"data":{},"metadata":{}}} trailing"#,
            "not json",
            "",
        ] {
            assert!(field(body.as_bytes(), "x").is_err(), "{body}");
        }
    }

    #[test]
    fn a_v1_secret_with_fields_named_data_and_metadata_is_still_v1() {
        let body = r#"{"data":{"data":"d","metadata":"m"}}"#;
        assert_eq!(
            field(body.as_bytes(), "data").unwrap().unwrap().as_str(),
            "d"
        );
    }

    #[test]
    fn a_typed_secret_is_read_from_both_versions() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Keys {
            environment: String,
        }
        for body in [
            r#"{"data":{"environment":"test"}}"#,
            r#"{"data":{"data":{"environment":"test"},"metadata":{"version":1}}}"#,
        ] {
            let keys: Keys = secret(body.as_bytes()).unwrap();
            assert_eq!(keys.environment, "test");
        }
        assert!(secret::<Keys>(br#"{"data":{"environment":"test","extra":1}}"#).is_err());
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The intent as the core recorded it (`intents`, F-001), read through the signer's role.
//!
//! The signer reads the payload fields below and nothing else. The core writes them; the signer
//! never takes a field from its caller, who sends only the intent id (`INV-9`).
//!
//! | Field | Meaning |
//! |---|---|
//! | `message` | The exact bytes to sign, `0x` hex: the network's signing payload |
//! | `amount` | What the intent moves, in minor units, a decimal string (`INV-11`): its order's. The window cap sums what the decoded bytes move, which must be the order's (audit must-fix 2) |
//! | `from` | For a destroy on the issuing network: the account destroyed from, `0x` hex |
//! | `prepared` | The prepared transaction whose bytes `message` must be, an integer id (PR #39 third review MF-A) |

use serde_json::Value;

use crate::config::parse_minor_units;

/// A recorded intent, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    /// The intent id.
    pub id: i64,
    /// The network id.
    pub network: String,
    /// What the intent does (`mint`, `destroy`, …).
    pub purpose: String,
    /// The one-time on-chain reference, if the intent carries one.
    pub on_chain_reference: Option<Vec<u8>>,
    /// The exact bytes to sign.
    pub message: Vec<u8>,
    /// The amount, in minor units.
    pub amount: u128,
    /// The account a destroy takes from.
    pub from: Option<Vec<u8>>,
    /// The prepared transaction the intent names (MF-A: the signer signs only its exact bytes).
    pub prepared: Option<i64>,
}

/// Why a recorded intent cannot be read: names the field, never quotes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed(pub &'static str);

fn hex_field(payload: &Value, field: &'static str) -> Result<Option<Vec<u8>>, Malformed> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let digits = s.strip_prefix("0x").ok_or(Malformed(field))?;
            hex::decode(digits)
                .ok()
                .filter(|b| !b.is_empty())
                .map(Some)
                .ok_or(Malformed(field))
        }
        Some(_) => Err(Malformed(field)),
    }
}

impl Intent {
    /// Parses a row of `intents`.
    pub fn from_row(
        id: i64,
        network: String,
        purpose: String,
        on_chain_reference: Option<Vec<u8>>,
        payload: &Value,
    ) -> Result<Self, Malformed> {
        let message = hex_field(payload, "message")?.ok_or(Malformed("message"))?;
        let amount = match payload.get("amount") {
            Some(Value::String(s)) => parse_minor_units(s).ok_or(Malformed("amount"))?,
            _ => return Err(Malformed("amount")),
        };
        let from = hex_field(payload, "from")?;
        let prepared = match payload.get("prepared") {
            None | Some(Value::Null) => None,
            Some(v) => Some(v.as_i64().ok_or(Malformed("prepared"))?),
        };
        Ok(Self {
            id,
            network,
            purpose,
            on_chain_reference,
            message,
            amount,
            from,
            prepared,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(payload: Value) -> Result<Intent, Malformed> {
        Intent::from_row(1, "n".into(), "mint".into(), None, &payload)
    }

    #[test]
    fn inv_11_amount_is_an_exact_decimal_string() {
        assert_eq!(
            parse(json!({"message": "0x01", "amount": "340282366920938463463374607431768211455"}))
                .unwrap()
                .amount,
            u128::MAX
        );
        for amount in [
            json!(1),
            json!("1.5"),
            json!("-1"),
            json!("1e3"),
            json!(null),
        ] {
            assert_eq!(
                parse(json!({"message": "0x01", "amount": amount})),
                Err(Malformed("amount"))
            );
        }
    }

    #[test]
    fn inv_9_message_is_hex_bytes() {
        for message in [json!("01"), json!("0x"), json!("0xzz"), json!(1)] {
            assert_eq!(
                parse(json!({"message": message, "amount": "1"})),
                Err(Malformed("message"))
            );
        }
    }
}

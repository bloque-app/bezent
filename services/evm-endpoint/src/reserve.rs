// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The vault's balances at each final head the endpoint records (issue #42; `REQ-BK-8`, `NFR-5`,
//! `INV-1`, `REQ-CV-4`, `REQ-OB-5`; PLAN `DEC-12`, `DEC-16`).
//!
//! The reserve check reads Reserve(A, n) at a final point of network *n*, and the core holds no
//! network connection (`DEC-3`): the endpoint reads it. At the final point both providers stand
//! on, it asks each provider, separately, for the vault's `balanceOf` of every admitted origin token
//! (the reserve) and of every admitted convertible (itemised outside the equation, `REQ-CV-4`), at
//! that block number. Both answers are recorded. When they agree the reading is that balance; when
//! they disagree, or either fails, the reading is **unknown**, with the reason (`DEC-12`): never
//! the higher, never the lower, never one provider's word. The check takes an evaluation with an
//! unknown reading as incomplete, and one that stays so past the network's bound pauses the asset
//! (`REQ-IC-2`).
//!
//! The readings are recorded with the final head, in one transaction ([`crate::observe`]), so a
//! final point the check picks has its readings with it.

use alloy::primitives::{Address, Bytes, U256};
use serde_json::{Value, json};

use crate::observe::Watch;
use crate::pair::Pair;
use crate::rpc::{At, Call, Rpc};

/// `balanceOf(address)`.
const BALANCE_OF: [u8; 4] = [0x70, 0xa0, 0x82, 0x31];

/// One reading of the vault's balance of a token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// `reserve` (an admitted origin token) or `convertible`.
    pub kind: &'static str,
    /// The token, as the store spells it.
    pub subject: String,
    /// The vault, as the store spells it.
    pub holder: String,
    /// The balance, or `None` when unknown.
    pub amount: Option<u128>,
    /// Why it is unknown.
    pub unknown_reason: Option<String>,
    /// What each provider answered.
    pub detail: Value,
}

/// The calldata of `balanceOf(holder)`.
pub fn balance_of(holder: Address) -> Bytes {
    let mut data = BALANCE_OF.to_vec();
    data.extend_from_slice(holder.into_word().as_slice());
    data.into()
}

/// What one provider answered: the balance, or why not.
async fn ask<R: Rpc>(p: &R, token: Address, holder: Address, number: u64) -> Result<U256, String> {
    let call = Call {
        from: Address::ZERO,
        to: token,
        data: balance_of(holder),
    };
    let out = p
        .call(&call, At::Number(number))
        .await
        .map_err(|e| format!("{}: {e}", p.name()))?;
    if out.len() != 32 {
        return Err(format!(
            "{}: balanceOf answered {} bytes, not 32",
            p.name(),
            out.len()
        ));
    }
    Ok(U256::from_be_slice(&out))
}

/// The vault's balance of `token` at block `number`, from both providers (`DEC-12`).
pub async fn read<R: Rpc>(
    pair: &Pair<R>,
    kind: &'static str,
    token: Address,
    subject: String,
    vault: Address,
    holder: String,
    number: u64,
) -> Reading {
    let a = ask(&pair.a, token, vault, number).await;
    let b = ask(&pair.b, token, vault, number).await;
    let shown = |r: &Result<U256, String>| match r {
        Ok(v) => json!({"balance": v.to_string()}),
        Err(e) => json!({"error": e}),
    };
    let detail = json!({
        "block_number": number,
        "providers": [
            {"provider": pair.a.name(), "answer": shown(&a)},
            {"provider": pair.b.name(), "answer": shown(&b)},
        ],
    });
    let (amount, unknown_reason) = match (a, b) {
        (Ok(x), Ok(y)) if x == y => match u128::try_from(x) {
            Ok(v) => (Some(v), None),
            Err(_) => (
                None,
                Some(format!("the balance {x} is above the store's domain")),
            ),
        },
        (Ok(x), Ok(y)) => (
            None,
            Some(format!(
                "DEC-12: the providers disagree on the balance of {subject} at block {number}: \
                 {} answers {x}, {} answers {y}",
                pair.a.name(),
                pair.b.name()
            )),
        ),
        (Err(e), _) | (_, Err(e)) => (None, Some(format!("unavailable: {e}"))),
    };
    Reading {
        kind,
        subject,
        holder,
        amount,
        unknown_reason,
        detail,
    }
}

/// The vault's balance of every admitted origin token and every admitted convertible at block
/// `number`.
pub async fn readings<R: Rpc>(pair: &Pair<R>, watch: &Watch<'_>, number: u64) -> Vec<Reading> {
    let holder = watch.declarations.vault_text.clone();
    let mut out = Vec::new();
    for (address, token) in &watch.declarations.tokens {
        out.push(
            read(
                pair,
                "reserve",
                *address,
                token.text.clone(),
                watch.vault,
                holder.clone(),
                number,
            )
            .await,
        );
    }
    for (convertible, _pool) in watch.convertibles {
        out.push(
            read(
                pair,
                "convertible",
                *convertible,
                convertible.to_checksum(None),
                watch.vault,
                holder.clone(),
                number,
            )
            .await,
        );
    }
    out
}

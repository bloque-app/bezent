// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The delivery account's fee position at each final head the endpoint records (issue #14;
//! `REQ-ST-6`, `AC-H3.1`, `REQ-OB-5`; PLAN `DEC-12`, `DEC-18`).
//!
//! A fee position is the native token an address needs to pay network costs (SPEC glossary): here,
//! the lane's native balance. The core holds no network connection (`DEC-3`), so the endpoint reads
//! it, at the final point both providers stand on, from each provider separately, and records it in
//! the same transaction as the head, as it does a reserve reading ([`crate::reserve`]). When the
//! providers agree the reading is that balance; when they disagree, or either fails, it is
//! **unknown**, with the reason: never the higher, never the lower, never zero (`REQ-NX-3`).
//!
//! An unknown fee position is recorded and logged, and never fails the head's recording or stops
//! the follower: it is not a reserve reading, and the reserve check does not see it.

use std::time::Duration;

use alloy::primitives::{Address, U256};
use serde_json::{Value, json};

use crate::pair::Pair;
use crate::rpc::{At, Rpc};

/// One reading of the delivery account's native balance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The delivery account, as the store spells it (EIP-55).
    pub holder: String,
    /// The balance in wei, or `None` when unknown.
    pub amount: Option<u128>,
    /// Why it is unknown.
    pub unknown_reason: Option<String>,
    /// What each provider answered.
    pub detail: Value,
}

/// The most the whole fee read may take, both providers together. The read runs before the head's
/// transaction opens, so a provider that never answers would otherwise keep the head and the
/// reserve readings already collected from being recorded. Past it the reading is unknown, like any
/// other failed read. alloy's HTTP client sets no timeout of its own (it builds a plain
/// `reqwest::Client`), and nothing else in this endpoint bounds a read; five seconds is a few
/// polls' worth of slack at most, and a healthy provider answers a balance in milliseconds.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

async fn ask<R: Rpc>(p: &R, holder: Address, number: u64) -> Result<U256, String> {
    p.balance(holder, At::Number(number))
        .await
        .map_err(|e| format!("{}: {e}", p.name()))
}

/// `holder`'s native balance at block `number`, from both providers (`DEC-12`).
pub async fn read<R: Rpc>(pair: &Pair<R>, holder: Address, number: u64) -> Reading {
    read_within(pair, holder, number, READ_TIMEOUT).await
}

/// [`read`] with its bound given: the providers are asked together, and an answer that is not
/// complete within `limit` is an unknown reading, not a wait.
pub async fn read_within<R: Rpc>(
    pair: &Pair<R>,
    holder: Address,
    number: u64,
    limit: Duration,
) -> Reading {
    let asked = tokio::time::timeout(limit, async {
        tokio::join!(ask(&pair.a, holder, number), ask(&pair.b, holder, number))
    })
    .await;
    let Ok((a, b)) = asked else {
        let reason = format!(
            "unavailable: the providers did not both answer the native balance within {} s",
            limit.as_secs_f64()
        );
        return Reading {
            holder: holder.to_checksum(None),
            amount: None,
            unknown_reason: Some(reason.clone()),
            detail: json!({"block_number": number, "error": reason}),
        };
    };
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
                "DEC-12: the providers disagree on the native balance at block {number}: \
                 {} answers {x}, {} answers {y}",
                pair.a.name(),
                pair.b.name()
            )),
        ),
        (Err(e), _) | (_, Err(e)) => (None, Some(format!("unavailable: {e}"))),
    };
    Reading {
        holder: holder.to_checksum(None),
        amount,
        unknown_reason,
        detail,
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The delivery account's fee position at each final head the endpoint records (issue #14; `REQ-ST-6`,
//! `AC-H3.1`, `REQ-OB-5`, `REQ-NX-3`; PLAN `DEC-18`; amendment 0025, `d-fee-position-scope`).
//!
//! The fee position is the native token the delivery account can spend on network costs. On Kreivo that is
//! its **free balance less its frozen balance** (ruled 2026-11-06): not the free balance alone, since a frozen
//! part cannot pay, and not the reserved part, which a storage deposit holds. Where frozen exceeds free the
//! position is zero, and the reading keeps all three figures. The reading is made at the same final head as the
//! supply readings ([`crate::supply`]), through the light client, so it is proven like them (`DEC-5`), and it
//! is recorded in the same transaction as the head.
//!
//! The core holds no network connection (`DEC-3`), so the endpoint reads it. A read that fails or exceeds
//! [`READ_TIMEOUT`] is **unknown**, with the reason: never zero, never the last value (`REQ-NX-3`). An unknown
//! fee position is recorded and logged, and never fails the head's recording or stops the endpoint: it is not a
//! reserve reading, and the reserve check does not see it.

use std::time::Duration;

use serde_json::{Value, json};

use crate::chain::{BlockRef, Chain};
use crate::kreivo::AccountId32;

/// One reading of the delivery account's fee position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The delivery account, as the endpoint spells it (`0x` and hex, as its lane declaration does).
    pub holder: String,
    /// Free balance less frozen balance, in the smallest unit (planck), or `None` when unknown.
    pub amount: Option<u128>,
    /// Why it is unknown.
    pub unknown_reason: Option<String>,
    /// What the chain answered.
    pub detail: Value,
}

/// The most one balance read may take. The read runs before the head's transaction opens, so a chain that never
/// answers would otherwise keep the head and the supply readings already collected from being recorded. Past it
/// the reading is unknown, like any other failed read (the EVM endpoint's bound, `fee::READ_TIMEOUT`).
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `holder`'s fee position at `head`, a final block.
pub async fn read<C: Chain>(chain: &C, holder: &AccountId32, head: &BlockRef) -> Reading {
    read_within(chain, holder, head, READ_TIMEOUT).await
}

/// [`read`] with its bound given: an answer that does not come within `limit` is an unknown reading, not a wait.
pub async fn read_within<C: Chain>(
    chain: &C,
    holder: &AccountId32,
    head: &BlockRef,
    limit: Duration,
) -> Reading {
    let holder_text = format!("0x{}", hex::encode(holder));
    let unknown = |reason: String| Reading {
        holder: holder_text.clone(),
        amount: None,
        unknown_reason: Some(reason.clone()),
        detail: json!({"block_number": head.number, "error": reason}),
    };
    match tokio::time::timeout(limit, chain.account_balances(head, holder)).await {
        Err(_) => unknown(format!(
            "unavailable: the native balance was not answered within {} s",
            limit.as_secs_f64()
        )),
        Ok(Err(e)) => unknown(format!("unavailable: {e}")),
        Ok(Ok(b)) => Reading {
            holder: holder_text,
            amount: Some(b.free.saturating_sub(b.frozen)),
            unknown_reason: None,
            detail: json!({
                "block_number": head.number,
                "free": b.free.to_string(),
                "reserved": b.reserved.to_string(),
                "frozen": b.frozen.to_string(),
            }),
        },
    }
}

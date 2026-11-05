// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Where a signed transaction is sent: public nodes, and only they (PLAN `DEC-12`, amendment 0023,
//! form item `d-evm-submit-public`).
//!
//! Only `eth_sendRawTransaction` goes through here. Everything that feeds a decision (the nonce,
//! the fee and gas estimates, the dry run, receipts, logs, headers) stays on the paid
//! [`Pair`](crate::pair::Pair), cross-checked. A public node cannot forge a signed transaction;
//! the worst it can do is drop one, and the submitter sends the same bytes again (`REQ-EO-1`,
//! `AC-D2.1`) until the pair shows the outcome. There is no fallback to the pair: if every
//! broadcast node fails, the send is not taken and the next pass tries again.
//!
//! The same bytes go to every node at once, each bounded by [`BROADCAST_TIMEOUT`]. A node has
//! taken the transaction only if the hash it returns is the `keccak256` of the signed bytes; any
//! other hash is that node's failure. The hash stored is always the one computed here.

use std::time::Duration;

use alloy::primitives::keccak256;
use futures_util::future::join_all;

use crate::rpc::Rpc;

/// How long one broadcast node may take to answer a send. It is well under the provider
/// `REQUEST_TIMEOUT` (30 s) and the health `PASS_ALLOWANCE` (120 s): the nodes are asked at once,
/// so one that hangs delays one send by this and no more. A pass sends several rows one after
/// another, so it is bounded by the re-send budget (`submit::RESEND_BUDGET`, 30 s), not by this.
pub const BROADCAST_TIMEOUT: Duration = Duration::from_secs(10);

/// The public nodes a network's signed transactions are sent to.
pub struct Broadcast<R> {
    /// The nodes.
    pub nodes: Vec<R>,
    /// The bound on each node's answer to a send or a chain id request.
    pub timeout: Duration,
}

impl<R: Rpc> Broadcast<R> {
    /// These nodes, each bounded by [`BROADCAST_TIMEOUT`].
    pub fn new(nodes: Vec<R>) -> Self {
        Self {
            nodes,
            timeout: BROADCAST_TIMEOUT,
        }
    }

    /// Sends `bytes` to every node at once. Returns how many took it: a node took it when it
    /// returned the hash of exactly these bytes. `Err` when none did (`REQ-EO-1`: the caller
    /// sends again on the next pass).
    pub async fn send_raw(&self, bytes: &[u8]) -> Result<usize, String> {
        let expected = keccak256(bytes);
        let answers = join_all(self.nodes.iter().map(|node| async move {
            match tokio::time::timeout(self.timeout, node.send_raw(bytes)).await {
                Ok(Ok(hash)) if hash == expected => Ok(()),
                Ok(Ok(hash)) => {
                    tracing::warn!(
                        node = node.name(),
                        expected = %expected,
                        returned = %hash,
                        "a broadcast node returned another hash than the signed bytes'; it did not take the send"
                    );
                    Err(format!(
                        "{}: returned {hash}, not {expected}",
                        node.name()
                    ))
                }
                Ok(Err(e)) => Err(e.to_string()),
                Err(_) => Err(format!(
                    "{}: no answer within {} s",
                    node.name(),
                    self.timeout.as_secs_f32()
                )),
            }
        }))
        .await;
        let taken = answers.iter().filter(|a| a.is_ok()).count();
        if taken > 0 {
            return Ok(taken);
        }
        let reasons: Vec<String> = answers.into_iter().filter_map(Result::err).collect();
        Err(if reasons.is_empty() {
            "no broadcast node is configured".to_owned()
        } else {
            reasons.join("; ")
        })
    }

    /// Start-up: every node must serve chain `expected`. A node on another chain is an error
    /// (the endpoint does not start); a node that cannot be reached is logged and tolerated.
    pub async fn check_chain(&self, expected: u64) -> Result<(), String> {
        let answers = join_all(self.nodes.iter().map(|node| async move {
            (
                node,
                tokio::time::timeout(self.timeout, node.chain_id()).await,
            )
        }))
        .await;
        for (node, answer) in answers {
            match answer {
                Ok(Ok(id)) if id == expected => {}
                Ok(Ok(id)) => {
                    return Err(format!(
                        "broadcast node {} serves chain {id}, not {expected}",
                        node.name()
                    ));
                }
                Ok(Err(e)) => {
                    tracing::warn!(node = node.name(), error = %e, "a broadcast node could not be reached at start-up; starting anyway");
                }
                Err(_) => {
                    tracing::warn!(
                        node = node.name(),
                        "a broadcast node did not answer at start-up; starting anyway"
                    );
                }
            }
        }
        Ok(())
    }
}

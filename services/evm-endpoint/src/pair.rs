// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Two independent providers, cross-checked (PLAN `DEC-12`).
//!
//! There is no light client for these networks in bezent's stack, so one provider could lie or
//! fail unseen. Every read that decides anything is made through both, at a block both report
//! with the same hash, and must give the same answer from both. A disagreement is
//! [`Read::Disagree`]: unknown, never an arrival, never a proof (`REQ-NX-3`, `AC-C1.2`). A
//! provider that fails is [`Read::Unavailable`], unknown too.
//!
//! Only reads that prepare a transaction (fees, the latest nonce, the dry run) and sending are not
//! decisions; those use both providers where both answer, as each method says.

use std::fmt;

use alloy::primitives::{Address, B256, Bytes, U256};

use crate::rpc::{At, Call, CallError, Header, Log, LogQuery, Receipt, Rpc, RpcError, Tx};

/// Why a cross-checked read has no answer. Either way it is unknown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// A provider failed: its own words.
    Unavailable(String),
    /// The providers answered differently (`DEC-12`).
    Disagree(String),
}

impl fmt::Display for Read {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(e) => write!(f, "unavailable: {e}"),
            Self::Disagree(e) => write!(f, "DEC-12: the providers disagree: {e}"),
        }
    }
}

impl From<RpcError> for Read {
    fn from(e: RpcError) -> Self {
        Self::Unavailable(e.0)
    }
}

/// Two providers of one network.
pub struct Pair<R> {
    /// The first.
    pub a: R,
    /// The second.
    pub b: R,
}

impl<R: Rpc> Pair<R> {
    fn same<T: PartialEq + fmt::Debug>(&self, what: &str, a: T, b: T) -> Result<T, Read> {
        if a == b {
            Ok(a)
        } else {
            Err(Read::Disagree(format!(
                "{what}: {} answers {a:?}, {} answers {b:?}",
                self.a.name(),
                self.b.name()
            )))
        }
    }

    /// The final point both providers stand on: the lower of their finalized heads, with the hash
    /// both report at that height (`REQ-OB-2`, `REQ-OB-5`).
    pub async fn final_point(&self) -> Result<Header, Read> {
        let (fa, fb) = (
            self.a.header(At::Finalized).await?,
            self.b.header(At::Finalized).await?,
        );
        let (Some(fa), Some(fb)) = (fa, fb) else {
            return Err(Read::Unavailable("no finalized block".into()));
        };
        let n = fa.number.min(fb.number);
        self.header(n).await?.ok_or_else(|| {
            Read::Unavailable(format!("block {n}, below both finalized heads, is missing"))
        })
    }

    /// The header at `number`, the same from both.
    pub async fn header(&self, number: u64) -> Result<Option<Header>, Read> {
        let a = self.a.header(At::Number(number)).await?;
        let b = self.b.header(At::Number(number)).await?;
        self.same(&format!("block {number}"), a, b)
    }

    /// The logs answering `q`, the same from both, ordered by coordinate.
    pub async fn logs(&self, q: &LogQuery) -> Result<Vec<Log>, Read> {
        let mut a = self.a.logs(q).await?;
        let mut b = self.b.logs(q).await?;
        a.sort();
        a.dedup();
        b.sort();
        b.dedup();
        if a != b {
            return Err(Read::Disagree(format!(
                "the logs of blocks {} to {}: {} returns {}, {} returns {}",
                q.from,
                q.to,
                self.a.name(),
                a.len(),
                self.b.name(),
                b.len()
            )));
        }
        Ok(a)
    }

    /// `address`'s nonce at block `number`, the same from both.
    pub async fn nonce_at(&self, address: Address, number: u64) -> Result<u64, Read> {
        let a = self.a.nonce(address, At::Number(number)).await?;
        let b = self.b.nonce(address, At::Number(number)).await?;
        self.same(&format!("the nonce of {address} at block {number}"), a, b)
    }

    /// `address`'s nonce at the latest block: the higher of the two (preparation only; a
    /// provider that lags would hand out a nonce already taken).
    pub async fn latest_nonce(&self, address: Address) -> Result<u64, Read> {
        let a = self.a.nonce(address, At::Latest).await?;
        let b = self.b.nonce(address, At::Latest).await?;
        Ok(a.max(b))
    }

    /// `address`'s balance at the latest block: the lower of the two (preparation only).
    pub async fn latest_balance(&self, address: Address) -> Result<U256, Read> {
        let a = self.a.balance(address, At::Latest).await?;
        let b = self.b.balance(address, At::Latest).await?;
        Ok(a.min(b))
    }

    /// Whether `address` holds code at the latest block on either provider (an EIP-7702
    /// delegation is code): any code on either is code (PR #39 review S5).
    pub async fn has_code(&self, address: Address) -> Result<bool, Read> {
        let a = self.a.code(address, At::Latest).await?;
        let b = self.b.code(address, At::Latest).await?;
        Ok(!a.is_empty() || !b.is_empty())
    }

    /// `eth_call` of a view at block `number`, the same answer from both providers.
    pub async fn view_at(&self, call: &Call, number: u64) -> Result<Bytes, Read> {
        let mut out = Vec::with_capacity(2);
        for p in [&self.a, &self.b] {
            out.push(match p.call(call, At::Number(number)).await {
                Ok(b) => b,
                Err(CallError::Reverted { message, .. }) => {
                    return Err(Read::Unavailable(format!("{}: {message}", p.name())));
                }
                Err(CallError::Rpc(e)) => return Err(Read::Unavailable(e.0)),
            });
        }
        let b = out.pop().unwrap_or_default();
        let a = out.pop().unwrap_or_default();
        self.same(&format!("a call to {} at block {number}", call.to), a, b)
    }

    /// The transactions of block `number`, the same from both.
    pub async fn block_transactions(&self, number: u64) -> Result<Option<(Header, Vec<Tx>)>, Read> {
        let a = self.a.block_transactions(number).await?;
        let b = self.b.block_transactions(number).await?;
        self.same(&format!("the transactions of block {number}"), a, b)
    }

    /// A receipt, the same from both.
    pub async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, Read> {
        let a = self.a.receipt(hash).await?;
        let b = self.b.receipt(hash).await?;
        self.same(&format!("the receipt of {hash}"), a, b)
    }

    /// The dry run (PLAN `DEC-3`): `eth_call` at the latest block of each provider. It passes
    /// only if it passes on both; each answer is returned with the block it was made at.
    pub async fn dry_run(&self, call: &Call) -> Result<DryRun, Read> {
        let mut runs = Vec::with_capacity(2);
        let mut violations = Vec::with_capacity(2);
        for p in [&self.a, &self.b] {
            let Some(at) = p.header(At::Latest).await? else {
                return Err(Read::Unavailable(format!("{}: no latest block", p.name())));
            };
            let result = match p.call(call, At::Number(at.number)).await {
                Ok(out) => {
                    violations.push(None);
                    Ok(out)
                }
                Err(CallError::Reverted { message, data }) => {
                    violations.push(crate::evm::condition_violation(data.as_ref()));
                    Err(crate::evm::revert_reason(&message, data.as_ref()))
                }
                Err(CallError::Rpc(e)) => return Err(Read::Unavailable(e.0)),
            };
            runs.push((p.name().to_owned(), at, result));
        }
        Ok(DryRun { runs, violations })
    }

    /// The gas estimate: the higher of the two. A revert is returned as its reason.
    pub async fn estimate_gas(&self, call: &Call) -> Result<Result<u64, String>, Read> {
        let mut best = 0;
        for p in [&self.a, &self.b] {
            match p.estimate_gas(call).await {
                Ok(g) => best = best.max(g),
                Err(CallError::Reverted { message, data }) => {
                    return Ok(Err(crate::evm::revert_reason(&message, data.as_ref())));
                }
                Err(CallError::Rpc(e)) => return Err(Read::Unavailable(e.0)),
            }
        }
        Ok(Ok(best))
    }

    /// The priority fee: the higher of the two.
    pub async fn priority_fee(&self) -> Result<u128, Read> {
        Ok(self
            .a
            .priority_fee()
            .await?
            .max(self.b.priority_fee().await?))
    }

    /// Sends the same bytes to both; it is sent if either accepted it.
    pub async fn send_raw(&self, bytes: &[u8]) -> Result<B256, String> {
        let a = self.a.send_raw(bytes).await;
        let b = self.b.send_raw(bytes).await;
        match (a, b) {
            (Ok(h), _) | (_, Ok(h)) => Ok(h),
            (Err(x), Err(y)) => Err(format!("{x}; {y}")),
        }
    }
}

/// A dry run on both providers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DryRun {
    /// Each provider's name, the block it ran at, and the output or the revert reason.
    pub runs: Vec<(String, Header, Result<Bytes, String>)>,
    /// Each run's revert decoded as the Roles modifier's `ConditionViolation`, if it was one.
    pub violations: Vec<Option<(u8, B256)>>,
}

impl DryRun {
    /// The Roles modifier's refusal, `(status, key)`, when every provider reverted with the same
    /// `ConditionViolation` (`DEC-12`; PR #52 review S-2). A refusal one provider reports alone,
    /// or two that differ, is none: the core then names no `CND-PayoutWindow` from it.
    pub fn violation(&self) -> Option<(u8, B256)> {
        let first = (*self.violations.first()?)?;
        (self.violations.len() == self.runs.len()
            && self.violations.len() >= 2
            && self.violations.iter().all(|v| *v == Some(first)))
        .then_some(first)
    }

    /// Whether it passed on both.
    pub fn passed(&self) -> bool {
        self.runs.iter().all(|(_, _, r)| r.is_ok())
    }

    /// The first revert reason.
    pub fn reason(&self) -> Option<&str> {
        self.runs
            .iter()
            .find_map(|(_, _, r)| r.as_ref().err().map(String::as_str))
    }
}

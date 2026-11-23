// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Two independent providers, cross-checked (PLAN `DEC-12`), drawn from a pool (amendment 0032).
//!
//! There is no light client for these networks in bezent's stack, so one provider could lie or
//! fail unseen. Every read that decides anything is made through both, at a block both report
//! with the same hash, and must give the same answer from both. A disagreement is
//! [`Read::Disagree`]: unknown, never an arrival, never a proof (`REQ-NX-3`, `AC-C1.2`). A
//! provider that fails is [`Read::Unavailable`], unknown too.
//!
//! Only reads that prepare a transaction (fees, the latest nonce, the dry run) are not decisions;
//! those use both providers where both answer, as each method says. The pair never sends: signed
//! transactions go to public nodes only ([`crate::broadcast`], amendment 0023).
//!
//! The endpoint holds a [`Pool`] of at least two providers from at least two operators. A pass of
//! its loop takes one pair from the pool and uses it for everything in that pass; the next pass
//! takes the next pair, round-robin. A pair is two providers of different operators.

use std::fmt;
use std::future::Future;
use std::time::Duration;

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

/// The pauses before a provider's second and third try of a reading at a final block (issue #154).
///
/// A reading of the reserve or of the fee position is made once per final head and recorded with
/// it, so a provider's passing failure made it unknown until the next head, and the reserve check
/// held the asset meanwhile. Each provider is now asked up to three times, at the same block,
/// when its failure is one that fails fast and passes ([`passing_failure`]). Nothing else is asked
/// again: not a timeout (a hung provider would cost three timeouts), not an answer that does not
/// decode or lacks a field, not a revert, not a wrong-length answer, and never a disagreement
/// between the two. The pair does not change (amendment 0032), and a reading is still known only
/// when both providers of the pair give the same answer (`DEC-12`).
pub const READ_RETRY_PAUSES: [Duration; 2] =
    [Duration::from_millis(300), Duration::from_millis(1200)];

/// Whether a provider's error is one worth asking again after a pause: a rate limit (HTTP 429), a
/// gateway or server error (HTTP 500 to 504), or a failed connection (`transport error`: a reset
/// or a refusal). Read from the error's text as [`crate::rpc`] spells it.
pub fn passing_failure(error: &str) -> bool {
    [
        "HTTP 429",
        "HTTP 500",
        "HTTP 502",
        "HTTP 503",
        "HTTP 504",
        "transport error",
    ]
    .iter()
    .any(|k| error.contains(k))
}

/// `read` until it succeeds or fails with an error `again` does not accept, at most once more per
/// pause of [`READ_RETRY_PAUSES`]; the last error otherwise.
pub async fn with_retries<T, E, F, Fut>(mut read: F, again: impl Fn(&E) -> bool) -> Result<T, E>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut pauses = READ_RETRY_PAUSES.iter();
    loop {
        match read().await {
            Err(e) if again(&e) => match pauses.next() {
                Some(pause) => tokio::time::sleep(*pause).await,
                None => return Err(e),
            },
            done => return done,
        }
    }
}

/// Two providers of one network.
pub struct Pair<R> {
    /// The first.
    pub a: R,
    /// The second.
    pub b: R,
}

/// The valid pairs of a list of operators, as indexes `(i, j)` with `i < j`, in list order: two
/// providers form a pair only when their operators differ (amendment 0032, A2). Operators are
/// compared ignoring case and surrounding space.
pub fn valid_pairs<S: AsRef<str>>(operators: &[S]) -> Vec<(usize, usize)> {
    let key = |s: &S| s.as_ref().trim().to_lowercase();
    let mut out = Vec::new();
    for i in 0..operators.len() {
        for j in i + 1..operators.len() {
            if key(&operators[i]) != key(&operators[j]) {
                out.push((i, j));
            }
        }
    }
    out
}

/// The providers of one network and the rotation among their pairs (amendment 0032).
///
/// [`Pool::take`] hands out the next pair and moves the rotation on, whether or not the pass that
/// uses it succeeds: a pass that fails on a provider is followed by a pass on another pair
/// (while another pair exists). Within a pass the pair never changes, so every rule of `DEC-12`
/// holds as it did with a fixed pair.
pub struct Pool<R> {
    members: Vec<(String, R)>,
    pairs: Vec<(usize, usize)>,
    next: usize,
}

impl<R: Rpc> Pool<R> {
    /// A pool of `members`, each `(operator, provider)`. Refused if no valid pair exists.
    pub fn new(members: Vec<(String, R)>) -> Result<Self, String> {
        let operators: Vec<&str> = members.iter().map(|(o, _)| o.as_str()).collect();
        let pairs = valid_pairs(&operators);
        if pairs.is_empty() {
            return Err(format!(
                "DEC-12: no valid pair of providers: {} provider(s) from {} operator(s); a pair needs two operators",
                members.len(),
                {
                    let mut o: Vec<String> =
                        operators.iter().map(|s| s.trim().to_lowercase()).collect();
                    o.sort();
                    o.dedup();
                    o.len()
                }
            ));
        }
        Ok(Self {
            members,
            pairs,
            next: 0,
        })
    }

    /// How many valid pairs the pool has.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// The names of pair `k` (modulo the count), `(a, b)`.
    pub fn names(&self, k: usize) -> (&str, &str) {
        let (i, j) = self.pairs[k % self.pairs.len()];
        (self.members[i].1.name(), self.members[j].1.name())
    }

    /// Pair `k` (modulo the count), without moving the rotation.
    pub fn pair_at(&self, k: usize) -> Pair<&R> {
        let (i, j) = self.pairs[k % self.pairs.len()];
        Pair {
            a: &self.members[i].1,
            b: &self.members[j].1,
        }
    }

    /// The next pair of the rotation and its index, and the rotation moves on. One pass, one pair.
    pub fn take(&mut self) -> (usize, Pair<&R>) {
        let k = self.next % self.pairs.len();
        self.next = (k + 1) % self.pairs.len();
        (k, self.pair_at(k))
    }
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

    /// `address`'s nonce at the latest block, from each provider, `(a, b)`: for an alarm, which
    /// rests on both providers' word and never on one's (amendment 0022, P2, ruled
    /// `d-pause-0022`, 2026-11-06). A provider that fails is [`Read::Unavailable`].
    pub async fn latest_nonces(&self, address: Address) -> Result<(u64, u64), Read> {
        let a = self.a.nonce(address, At::Latest).await?;
        let b = self.b.nonce(address, At::Latest).await?;
        Ok((a, b))
    }

    /// Whether `address` holds code at the latest block, from each provider, `(a, b)`: for an
    /// alarm, as [`Pair::latest_nonces`].
    pub async fn codes(&self, address: Address) -> Result<(bool, bool), Read> {
        let a = self.a.code(address, At::Latest).await?;
        let b = self.b.code(address, At::Latest).await?;
        Ok((!a.is_empty(), !b.is_empty()))
    }

    /// `address`'s balance at the latest block: the lower of the two (preparation only).
    pub async fn latest_balance(&self, address: Address) -> Result<U256, Read> {
        let a = self.a.balance(address, At::Latest).await?;
        let b = self.b.balance(address, At::Latest).await?;
        Ok(a.min(b))
    }

    /// Whether `address` holds code at the latest block on either provider (an EIP-7702
    /// delegation is code): any code on either is code (PR #39 review S5). For the start-up check,
    /// which fails closed and raises nothing; the alarm reads [`Pair::codes`].
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::{MockChain, MockRpc};

    fn pool(ops: &[&str], chain: &MockChain) -> Pool<MockRpc> {
        Pool::new(
            ops.iter()
                .enumerate()
                .map(|(i, o)| ((*o).to_owned(), MockRpc::new(&format!("p{i}"), chain)))
                .collect(),
        )
        .unwrap()
    }

    #[test]
    fn a_pair_is_two_providers_of_different_operators_in_config_order() {
        assert_eq!(valid_pairs(&["a", "b", "c"]), [(0, 1), (0, 2), (1, 2)]);
        assert_eq!(valid_pairs(&["a", "b", "a"]), [(0, 1), (1, 2)]);
        assert_eq!(valid_pairs(&["a", "A "]), []);
        assert_eq!(valid_pairs(&["a"]), []);
    }

    #[test]
    fn a_pool_without_a_valid_pair_is_refused() {
        let chain = MockChain::new(137, 3);
        let e = Pool::new(vec![
            ("x".to_owned(), MockRpc::new("a", &chain)),
            ("x".to_owned(), MockRpc::new("b", &chain)),
        ])
        .err()
        .unwrap();
        assert!(e.contains("no valid pair"), "{e}");
    }

    /// Amendment 0032, A2: each pass takes the next pair, round-robin, and wraps.
    #[test]
    fn each_pass_takes_the_next_pair_round_robin() {
        let chain = MockChain::new(137, 3);
        let mut pool = pool(&["a", "b", "a"], &chain);
        let mut seen = Vec::new();
        for _ in 0..5 {
            let (k, pair) = pool.take();
            seen.push((k, pair.a.name().to_owned(), pair.b.name().to_owned()));
        }
        let want = |k: usize, a: &str, b: &str| (k, a.to_owned(), b.to_owned());
        assert_eq!(
            seen,
            [
                want(0, "p0", "p1"),
                want(1, "p1", "p2"),
                want(0, "p0", "p1"),
                want(1, "p1", "p2"),
                want(0, "p0", "p1"),
            ]
        );
    }

    /// A pass that fails on a provider is followed by a pass on another pair, which reads.
    #[tokio::test]
    async fn a_pass_that_fails_moves_the_next_pass_to_another_pair() {
        let chain = MockChain::new(137, 10);
        let down = chain.fork();
        down.fail_all(Some("HTTP 429"));
        let mut pool = Pool::new(vec![
            ("one".to_owned(), MockRpc::new("p0", &chain)),
            ("two".to_owned(), MockRpc::new("p1", &down)),
            ("three".to_owned(), MockRpc::new("p2", &chain)),
        ])
        .unwrap();
        // Pairs: (p0, p1), (p0, p2), (p1, p2). The first holds the failing provider.
        let (k, pair) = pool.take();
        let first = pair.final_point().await;
        assert_eq!(k, 0);
        assert!(matches!(first, Err(Read::Unavailable(_))), "{first:?}");
        let (k, pair) = pool.take();
        assert_eq!(k, 1);
        assert_eq!(pair.final_point().await.unwrap().number, 9);
    }
}

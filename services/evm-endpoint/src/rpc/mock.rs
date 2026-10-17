// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! An in-memory provider for tests: blocks, logs, nonces, transactions and receipts that a test
//! writes, and the faults it injects. Nothing executes: a test states what the chain holds. Two
//! providers that disagree are two [`MockChain`]s, one [`MockChain::fork`]ed from the other.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use alloy::primitives::{Address, B256, Bytes, U256, keccak256};

use super::{At, Call, CallError, Header, Log, LogQuery, Receipt, Rpc, RpcError, Tx};

#[derive(Clone, Debug, Default)]
struct State {
    chain_id: u64,
    headers: BTreeMap<u64, Header>,
    finalized: u64,
    logs: Vec<Log>,
    /// Per address: (from block, nonce from that block on).
    nonces: HashMap<Address, Vec<(u64, u64)>>,
    balances: HashMap<Address, U256>,
    txs: BTreeMap<u64, Vec<Tx>>,
    receipts: HashMap<B256, Receipt>,
    call: Option<Result<Bytes, (String, Option<Bytes>)>>,
    /// What `eth_call` answers for one target and calldata, before `call`.
    views: HashMap<(Address, Bytes), Bytes>,
    codes: HashMap<Address, Bytes>,
    gas: u64,
    priority_fee: u128,
    sent: Vec<Vec<u8>>,
    fail_logs: Option<String>,
    fail_all: Option<String>,
    salt: u8,
}

/// The chain a mock provider answers from. Clones share it.
#[derive(Clone, Debug, Default)]
pub struct MockChain(Arc<Mutex<State>>);

impl MockChain {
    /// A chain `chain_id` with blocks 0 to `blocks - 1`, every one final.
    pub fn new(chain_id: u64, blocks: u64) -> Self {
        let chain = Self::default();
        {
            let mut s = chain.state();
            s.chain_id = chain_id;
            s.gas = 200_000;
            s.priority_fee = 30_000_000_000;
            s.call = Some(Ok(Bytes::new()));
        }
        for _ in 0..blocks {
            chain.mine();
        }
        let last = blocks.saturating_sub(1);
        chain.finalize(last);
        chain
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether `other` is this same chain.
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// An independent copy: what changes in one no longer shows in the other.
    pub fn fork(&self) -> Self {
        let mut s = self.state().clone();
        s.salt = s.salt.wrapping_add(1);
        Self(Arc::new(Mutex::new(s)))
    }

    /// Appends a block one second after the last, and returns it.
    pub fn mine(&self) -> Header {
        let mut s = self.state();
        let (number, parent, time) = match s.headers.last_key_value() {
            Some((n, h)) => (n + 1, h.hash, h.timestamp + 1),
            None => (0, B256::ZERO, 1_700_000_000),
        };
        let mut seed = number.to_be_bytes().to_vec();
        seed.push(s.salt);
        let h = Header {
            number,
            hash: keccak256(&seed),
            parent_hash: parent,
            timestamp: time,
            base_fee: Some(30_000_000_000),
        };
        s.headers.insert(number, h);
        h
    }

    /// Replaces block `number` (and nothing else) with another of the same height: a
    /// reorganisation as a provider would show it.
    pub fn replace(&self, number: u64) -> Header {
        let mut s = self.state();
        let old = s.headers[&number];
        let h = Header {
            hash: keccak256([old.hash.as_slice(), b"reorg"].concat()),
            ..old
        };
        s.headers.insert(number, h);
        h
    }

    /// Puts `h` at its height, as it is: two providers showing the same block.
    pub fn replace_with(&self, h: Header) {
        self.state().headers.insert(h.number, h);
    }

    /// Sets the finalized head.
    pub fn finalize(&self, number: u64) {
        self.state().finalized = number;
    }

    /// The header at `number`.
    pub fn header_at(&self, number: u64) -> Header {
        self.state().headers[&number]
    }

    /// The latest block's number.
    pub fn latest(&self) -> u64 {
        self.state().headers.last_key_value().map_or(0, |(n, _)| *n)
    }

    /// Adds a log in block `number` at `log_index` of transaction `tx`, with the block's hash.
    #[allow(clippy::too_many_arguments)]
    pub fn log(
        &self,
        number: u64,
        log_index: u64,
        tx: B256,
        tx_index: u64,
        address: Address,
        topics: Vec<B256>,
        data: Vec<u8>,
    ) -> Log {
        let mut s = self.state();
        let log = Log {
            block_number: number,
            log_index,
            block_hash: s.headers[&number].hash,
            tx_hash: tx,
            tx_index,
            address,
            topics,
            data: data.into(),
            removed: false,
        };
        s.logs.push(log.clone());
        log
    }

    /// Changes the block hash every log of block `number` reports.
    pub fn set_log_hash(&self, number: u64, hash: B256) {
        for l in self
            .state()
            .logs
            .iter_mut()
            .filter(|l| l.block_number == number)
        {
            l.block_hash = hash;
        }
    }

    /// Removes every log of block `number`.
    pub fn drop_logs(&self, number: u64) {
        self.state().logs.retain(|l| l.block_number != number);
    }

    /// Every `eth_getLogs` fails with `reason` until cleared.
    pub fn fail_logs(&self, reason: Option<&str>) {
        self.state().fail_logs = reason.map(str::to_owned);
    }

    /// Every read fails with `reason` until cleared.
    pub fn fail_all(&self, reason: Option<&str>) {
        self.state().fail_all = reason.map(str::to_owned);
    }

    /// `address`'s nonce is `nonce` from block `from` on.
    pub fn set_nonce(&self, address: Address, from: u64, nonce: u64) {
        let mut s = self.state();
        let v = s.nonces.entry(address).or_default();
        v.push((from, nonce));
        v.sort();
    }

    /// `address`'s balance.
    pub fn set_balance(&self, address: Address, balance: U256) {
        self.state().balances.insert(address, balance);
    }

    /// Adds a transaction to block `number`.
    pub fn add_tx(&self, number: u64, tx: Tx) {
        self.state().txs.entry(number).or_default().push(tx);
    }

    /// Records a receipt.
    pub fn add_receipt(&self, r: Receipt) {
        self.state().receipts.insert(r.tx_hash, r);
    }

    /// What `eth_call` and `eth_estimateGas` answer: `Ok(output)` or `Err((message, data))`.
    pub fn set_call(&self, result: Result<Bytes, (String, Option<Bytes>)>) {
        self.state().call = Some(result);
    }

    /// What `eth_call` to `to` with `data` answers, whatever [`Self::set_call`] says.
    pub fn set_view(&self, to: Address, data: impl Into<Bytes>, output: impl Into<Bytes>) {
        self.state().views.insert((to, data.into()), output.into());
    }

    /// `address`'s code (none by default).
    pub fn set_code(&self, address: Address, code: impl Into<Bytes>) {
        self.state().codes.insert(address, code.into());
    }

    /// The gas estimate.
    pub fn set_gas(&self, gas: u64) {
        self.state().gas = gas;
    }

    /// Every raw transaction sent so far.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.state().sent.clone()
    }

    fn check(&self) -> Result<(), RpcError> {
        match &self.state().fail_all {
            Some(r) => Err(RpcError(r.clone())),
            None => Ok(()),
        }
    }

    fn number(&self, at: At) -> Option<u64> {
        let s = self.state();
        match at {
            At::Finalized => Some(s.finalized),
            At::Latest => s.headers.last_key_value().map(|(n, _)| *n),
            At::Number(n) => s.headers.contains_key(&n).then_some(n),
        }
    }
}

/// A provider answering from a [`MockChain`].
#[derive(Clone, Debug)]
pub struct MockRpc {
    /// Its name.
    pub name: String,
    /// Its chain.
    pub chain: MockChain,
}

impl MockRpc {
    /// A provider named `name` over `chain`.
    pub fn new(name: &str, chain: &MockChain) -> Self {
        Self {
            name: name.into(),
            chain: chain.clone(),
        }
    }
}

impl Rpc for MockRpc {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chain_id(&self) -> Result<u64, RpcError> {
        self.chain.check()?;
        Ok(self.chain.state().chain_id)
    }

    async fn header(&self, at: At) -> Result<Option<Header>, RpcError> {
        self.chain.check()?;
        Ok(self
            .chain
            .number(at)
            .and_then(|n| self.chain.state().headers.get(&n).copied()))
    }

    async fn logs(&self, q: &LogQuery) -> Result<Vec<Log>, RpcError> {
        self.chain.check()?;
        let s = self.chain.state();
        if let Some(r) = &s.fail_logs {
            return Err(RpcError(r.clone()));
        }
        let mut out: Vec<Log> = s.logs.iter().filter(|l| q.matches(l)).cloned().collect();
        out.sort();
        Ok(out)
    }

    async fn nonce(&self, address: Address, at: At) -> Result<u64, RpcError> {
        self.chain.check()?;
        let n = self
            .chain
            .number(at)
            .ok_or_else(|| RpcError("no such block".into()))?;
        let s = self.chain.state();
        Ok(s.nonces
            .get(&address)
            .and_then(|v| v.iter().rev().find(|(from, _)| *from <= n))
            .map_or(0, |(_, nonce)| *nonce))
    }

    async fn balance(&self, address: Address, _at: At) -> Result<U256, RpcError> {
        self.chain.check()?;
        Ok(self
            .chain
            .state()
            .balances
            .get(&address)
            .copied()
            .unwrap_or(U256::from(10u128.pow(21))))
    }

    async fn block_transactions(&self, number: u64) -> Result<Option<(Header, Vec<Tx>)>, RpcError> {
        self.chain.check()?;
        let s = self.chain.state();
        Ok(s.headers
            .get(&number)
            .map(|h| (*h, s.txs.get(&number).cloned().unwrap_or_default())))
    }

    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, RpcError> {
        self.chain.check()?;
        Ok(self.chain.state().receipts.get(&hash).copied())
    }

    async fn code(&self, address: Address, _at: At) -> Result<Bytes, RpcError> {
        self.chain.check()?;
        Ok(self
            .chain
            .state()
            .codes
            .get(&address)
            .cloned()
            .unwrap_or_default())
    }

    async fn call(&self, call: &Call, _at: At) -> Result<Bytes, CallError> {
        self.chain.check().map_err(CallError::Rpc)?;
        let s = self.chain.state();
        if let Some(out) = s.views.get(&(call.to, call.data.clone())) {
            return Ok(out.clone());
        }
        match s.call.clone() {
            Some(Ok(b)) => Ok(b),
            Some(Err((message, data))) => Err(CallError::Reverted { message, data }),
            None => Err(CallError::Rpc(RpcError("no call result".into()))),
        }
    }

    async fn estimate_gas(&self, call: &Call) -> Result<u64, CallError> {
        self.call(call, At::Latest).await?;
        Ok(self.chain.state().gas)
    }

    async fn priority_fee(&self) -> Result<u128, RpcError> {
        self.chain.check()?;
        Ok(self.chain.state().priority_fee)
    }

    async fn send_raw(&self, bytes: &[u8]) -> Result<B256, RpcError> {
        self.chain.check()?;
        self.chain.state().sent.push(bytes.to_vec());
        Ok(keccak256(bytes))
    }
}

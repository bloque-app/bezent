// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Kreivo in memory, for tests: blocks, finality, the contract's events, mortal eras, nonces and
//! signatures, checked the way Kreivo checks them, with the endpoint's own encoding
//! ([`crate::kreivo`]).
//!
//! What it models:
//! - a chain of blocks from a genesis, a finalized head that only moves forward, and block times;
//! - the transaction pool: an extrinsic is included in the next block only if it decodes, its
//!   era is live and was born at a block of this chain (whose hash it binds), its nonce is the
//!   sender's next, and its signature verifies over the payload rebuilt from this chain's genesis
//!   and runtime version; an extrinsic whose era has passed is dropped, one with a later nonce
//!   waits;
//! - the contract: an included `mint` or `refund_mint` emits `Minted` or `RefundMinted` with the
//!   next sequence number, or, when told to, is refused on chain (`ExtrinsicFailed`);
//! - events emitted from outside (a holder's redemption), dry runs that pass or are refused,
//!   another transaction that consumes a sender's nonce, and reads that fail and heal.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::{
    BlockEvents, BlockRef, Chain, ChainError, ChainResult, Emitted, ExtrinsicResult, Header,
    RuntimeVersion,
};
use parity_scale_codec::{Decode, Encode};

use crate::kreivo::{
    self, AccountId32, ContractEvent, Message, Mortality, Scheme, TxParams, Weight, blake2_256,
};

#[derive(Clone)]
struct Block {
    header: Header,
    time_ms: u64,
    extrinsics: Vec<Vec<u8>>,
    emitted: Vec<Emitted>,
    results: Vec<ExtrinsicResult>,
    nonces: HashMap<AccountId32, u64>,
    /// The contract's state at this block: the mint references used, and the redemptions.
    used: HashSet<[u8; 32]>,
    redeemed: HashMap<(AccountId32, [u8; 32]), kreivo::Redemption>,
    /// Per local asset: the contract's own supply count, and the asset's issuance (issue #42).
    supply: HashMap<u32, (u128, u128)>,
}

/// What a dry run answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DryRunMode {
    /// The message returns `Ok(())`.
    Pass,
    /// The contract refuses it with the error of this index.
    Refuse(u8),
    /// The call fails to dispatch.
    Trap,
}

struct State {
    genesis: [u8; 32],
    version: RuntimeVersion,
    contract: AccountId32,
    blocks: Vec<Block>,
    finalized: u64,
    pool: Vec<Vec<u8>>,
    keys: HashMap<AccountId32, (Scheme, Vec<u8>)>,
    outside: Vec<ContractEvent>,
    other_nonces: Vec<AccountId32>,
    failing: HashSet<u64>,
    failing_headers: HashSet<u64>,
    /// The number of every header asked for, in order.
    header_log: Vec<u64>,
    dry_run: DryRunMode,
    refuse_on_chain: bool,
    seq: u64,
    redemptions: HashMap<(AccountId32, [u8; 32]), (kreivo::AssetId, u128)>,
    submitted: Vec<Vec<u8>>,
    raw: Vec<Vec<u8>>,
    outside_uses: Vec<[u8; 32]>,
    outside_refunds: Vec<(AccountId32, [u8; 32])>,
    refuse_submissions: bool,
    unavailable: bool,
    /// The code hash of the contract instantiated at `contract`, if any (issue #24).
    code_hash: Option<[u8; 32]>,
    /// What blocks produced from now on hold of each local asset: (contract count, issuance).
    supply: HashMap<u32, (u128, u128)>,
}

/// Kreivo in memory. See the module documentation.
pub struct FakeChain {
    state: Mutex<State>,
}

fn hash_of(number: u64, parent: &[u8; 32]) -> [u8; 32] {
    let mut b = number.to_le_bytes().to_vec();
    b.extend_from_slice(parent);
    blake2_256(&b)
}

/// The block time of block `n`: six-second blocks from a fixed start.
pub fn time_of(n: u64) -> u64 {
    1_760_000_000_000 + n * 6_000
}

impl FakeChain {
    /// A chain with genesis `genesis`, Kreivo `0.17.1`'s runtime version, and the contract at
    /// `contract`. Block 0 is the genesis, and final.
    pub fn new(genesis: [u8; 32], contract: AccountId32) -> FakeChain {
        let header = Header {
            block: BlockRef {
                number: 0,
                hash: genesis,
            },
            parent: [0; 32],
        };
        FakeChain {
            state: Mutex::new(State {
                genesis,
                version: RuntimeVersion {
                    spec_version: 134,
                    transaction_version: 13,
                },
                contract,
                blocks: vec![Block {
                    header,
                    time_ms: time_of(0),
                    extrinsics: Vec::new(),
                    emitted: Vec::new(),
                    results: Vec::new(),
                    nonces: HashMap::new(),
                    used: HashSet::new(),
                    redeemed: HashMap::new(),
                    supply: HashMap::new(),
                }],
                finalized: 0,
                pool: Vec::new(),
                keys: HashMap::new(),
                outside: Vec::new(),
                other_nonces: Vec::new(),
                failing: HashSet::new(),
                failing_headers: HashSet::new(),
                header_log: Vec::new(),
                dry_run: DryRunMode::Pass,
                refuse_on_chain: false,
                seq: 0,
                redemptions: HashMap::new(),
                submitted: Vec::new(),
                raw: Vec::new(),
                outside_uses: Vec::new(),
                outside_refunds: Vec::new(),
                refuse_submissions: false,
                unavailable: false,
                code_hash: None,
                supply: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers the public key of an account whose extrinsics the chain checks.
    pub fn register_key(&self, scheme: Scheme, public_key: &[u8]) -> AccountId32 {
        let account = scheme
            .account(public_key)
            .expect("a key of the scheme's length");
        self.lock()
            .keys
            .insert(account, (scheme, public_key.to_vec()));
        account
    }

    /// Sets the runtime version.
    pub fn set_version(&self, version: RuntimeVersion) {
        self.lock().version = version;
    }

    /// Sets what dry runs answer.
    pub fn set_dry_run(&self, mode: DryRunMode) {
        self.lock().dry_run = mode;
    }

    /// From now on, every included contract call is refused on chain (`ExtrinsicFailed`).
    pub fn refuse_on_chain(&self, refuse: bool) {
        self.lock().refuse_on_chain = refuse;
    }

    /// Reads of block `n`'s events fail until [`FakeChain::heal`].
    pub fn fail_reads_at(&self, n: u64) {
        self.lock().failing.insert(n);
    }

    /// Reads of block `n`'s header fail until [`FakeChain::heal_header`].
    pub fn fail_header_at(&self, n: u64) {
        self.lock().failing_headers.insert(n);
    }

    /// Reads of block `n`'s header work again.
    pub fn heal_header(&self, n: u64) {
        self.lock().failing_headers.remove(&n);
    }

    /// The number of every header asked for so far, in order (failed asks included).
    pub fn header_reads(&self) -> Vec<u64> {
        self.lock().header_log.clone()
    }

    /// Reads of block `n`'s events work again.
    pub fn heal(&self, n: u64) {
        self.lock().failing.remove(&n);
    }

    /// Every call fails, as if the chain could not be reached.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.lock().unavailable = unavailable;
    }

    /// Instantiates the contract with code hash `code_hash` (none: no contract is there).
    pub fn set_code_hash(&self, code_hash: Option<[u8; 32]>) {
        self.lock().code_hash = code_hash;
    }

    /// The contract emits `event` in the next block, as if a holder's call made it; its sequence
    /// number is replaced by the contract's next.
    pub fn emit(&self, event: ContractEvent) {
        self.lock().outside.push(event);
    }

    /// Takes the contract's next sequence number, as an event the endpoint never sees would:
    /// the next emitted event skips one.
    pub fn skip_seq(&self) {
        self.lock().seq += 1;
    }

    /// Another transaction of `account`, not one the endpoint built, consumes its next nonce in
    /// the next block.
    pub fn consume_nonce(&self, account: AccountId32) {
        self.lock().other_nonces.push(account);
    }

    /// Includes `extrinsic` in the next block as it is, unchecked, bumping its sender's nonce:
    /// a transaction made outside bezent.
    pub fn include_raw(&self, extrinsic: Vec<u8>) {
        self.lock().raw.push(extrinsic);
    }

    /// The contract uses mint reference `reference` in the next block, as a call the endpoint
    /// cannot attribute to the lane's nonce would (a proxy, a multisig, a `dispatch_as`): its
    /// `mint_reference_used` answers `true` from then on (review M1-bis).
    pub fn use_reference_outside(&self, reference: [u8; 32]) {
        self.lock().outside_uses.push(reference);
    }

    /// The contract refunds the redemption (`redeemer`, `reference`) in the next block, the same
    /// way: its `refunded` flag is set from then on.
    pub fn refund_outside(&self, redeemer: AccountId32, reference: [u8; 32]) {
        self.lock().outside_refunds.push((redeemer, reference));
    }

    /// Submissions are refused (a node that does not accept them) until told otherwise.
    pub fn refuse_submissions(&self, refuse: bool) {
        self.lock().refuse_submissions = refuse;
    }

    /// Everything ever submitted.
    pub fn submitted(&self) -> Vec<Vec<u8>> {
        self.lock().submitted.clone()
    }

    /// The best block's number.
    pub fn best(&self) -> u64 {
        self.lock().blocks.len() as u64 - 1
    }

    /// The finalized head.
    pub fn finalized_ref(&self) -> BlockRef {
        let s = self.lock();
        s.blocks[s.finalized as usize].header.block
    }

    /// Block `n` of the chain.
    pub fn block(&self, n: u64) -> BlockRef {
        self.lock().blocks[n as usize].header.block
    }

    /// `account`'s nonce at the best block.
    pub fn nonce(&self, account: &AccountId32) -> u64 {
        let s = self.lock();
        s.blocks
            .last()
            .and_then(|b| b.nonces.get(account).copied())
            .unwrap_or(0)
    }

    /// From the next block on, local asset `Here(asset)` has `contract` as the contract's supply
    /// count and `issuance` as its issuance (issue #42).
    pub fn set_supply(&self, asset: u32, contract: u128, issuance: u128) {
        self.lock().supply.insert(asset, (contract, issuance));
    }

    /// Produces `count` blocks and finalizes them all.
    pub fn advance(&self, count: u64) {
        for _ in 0..count {
            self.produce();
        }
        let best = self.best();
        self.finalize(best);
    }

    /// Moves the finalized head to block `n`.
    pub fn finalize(&self, n: u64) {
        let mut s = self.lock();
        assert!(n < s.blocks.len() as u64 && n >= s.finalized);
        s.finalized = n;
    }

    /// Produces one block, with what the pool and the outside hold.
    pub fn produce(&self) {
        let mut s = self.lock();
        let s = &mut *s;
        let parent = s.blocks.last().expect("genesis").clone();
        let number = parent.header.block.number + 1;
        let mut block = Block {
            header: Header {
                block: BlockRef {
                    number,
                    hash: hash_of(number, &parent.header.block.hash),
                },
                parent: parent.header.block.hash,
            },
            time_ms: time_of(number),
            extrinsics: Vec::new(),
            emitted: Vec::new(),
            results: Vec::new(),
            nonces: parent.nonces.clone(),
            used: parent.used.clone(),
            redeemed: parent.redeemed.clone(),
            supply: s.supply.clone(),
        };
        for reference in std::mem::take(&mut s.outside_uses) {
            block.used.insert(reference);
        }
        for key in std::mem::take(&mut s.outside_refunds) {
            s.redemptions.remove(&key);
            if let Some(r) = block.redeemed.get_mut(&key) {
                r.refunded = true;
            }
        }
        let mut index_of_event = 0u32;
        let mut next_event = || {
            let i = index_of_event;
            index_of_event += 1;
            i
        };

        for account in std::mem::take(&mut s.other_nonces) {
            let nonce = block.nonces.entry(account).or_insert(0);
            let mut other = b"another transaction".to_vec();
            other.extend_from_slice(&account);
            other.extend_from_slice(&nonce.to_le_bytes());
            *nonce += 1;
            let index = block.extrinsics.len() as u32;
            block.extrinsics.push(other);
            block.results.push(ExtrinsicResult {
                index,
                success: true,
                error: None,
            });
            next_event();
        }

        for x in std::mem::take(&mut s.raw) {
            if let Some(d) = kreivo::decode_extrinsic(&x) {
                *block.nonces.entry(d.account).or_insert(0) += 1;
            }
            let index = block.extrinsics.len() as u32;
            block.extrinsics.push(x);
            block.results.push(ExtrinsicResult {
                index,
                success: true,
                error: None,
            });
            next_event();
        }

        // Like a block builder: whatever became valid by an inclusion is looked at again.
        let mut pool = std::mem::take(&mut s.pool);
        loop {
            let mut keep = Vec::new();
            let mut included_any = false;
            for x in pool {
                match Self::check(s, &block, &x) {
                    Check::Include(account, call) => {
                        included_any = true;
                        *block.nonces.entry(account).or_insert(0) += 1;
                        let index = block.extrinsics.len() as u32;
                        block.extrinsics.push(x);
                        let message = kreivo::decode_contracts_call(&call)
                            .filter(|(dest, ..)| *dest == s.contract)
                            .and_then(|(.., data)| Message::decode(&data));
                        let refund_of = |m: &Message| match m {
                            Message::RefundMint {
                                redeemer,
                                reference,
                            } => Some(s.redemptions.get(&(*redeemer, *reference)).copied()),
                            Message::Mint { .. } => None,
                        };
                        let unknown_redemption =
                            matches!(message.as_ref().map(refund_of), Some(Some(None)));
                        if s.refuse_on_chain || message.is_none() || unknown_redemption {
                            block.results.push(ExtrinsicResult {
                                index,
                                success: false,
                                error: Some(
                                    "Module { index: 80, error: ContractReverted }".to_owned(),
                                ),
                            });
                            next_event();
                            continue;
                        }
                        let seq = s.seq;
                        s.seq += 1;
                        let event = match message.expect("checked") {
                            Message::Mint {
                                asset,
                                to,
                                amount,
                                reference,
                            } => {
                                block.used.insert(reference);
                                ContractEvent::Minted {
                                    seq,
                                    reference,
                                    asset,
                                    to,
                                    amount,
                                }
                            }
                            Message::RefundMint {
                                redeemer,
                                reference,
                            } => {
                                let (asset, amount) = s
                                    .redemptions
                                    .remove(&(redeemer, reference))
                                    .expect("checked");
                                if let Some(r) = block.redeemed.get_mut(&(redeemer, reference)) {
                                    r.refunded = true;
                                }
                                ContractEvent::RefundMinted {
                                    seq,
                                    reference,
                                    to: redeemer,
                                    asset,
                                    amount,
                                }
                            }
                        };
                        block.emitted.push(Emitted {
                            index: next_event(),
                            topics: event.topics(),
                            data: event.encode_data(),
                        });
                        block.results.push(ExtrinsicResult {
                            index,
                            success: true,
                            error: None,
                        });
                        next_event();
                    }
                    Check::Wait => keep.push(x),
                    Check::Drop => {}
                }
            }
            pool = keep;
            if !included_any {
                break;
            }
        }
        let keep = pool;
        s.pool = keep;

        for event in std::mem::take(&mut s.outside) {
            let event = with_seq(event, s.seq);
            s.seq += 1;
            if let ContractEvent::Redeemed {
                reference,
                redeemer,
                asset,
                amount,
                backend,
                payout,
                ..
            } = &event
            {
                s.redemptions
                    .insert((*redeemer, *reference), (*asset, *amount));
                block.redeemed.insert(
                    (*redeemer, *reference),
                    kreivo::Redemption {
                        redeemer: *redeemer,
                        asset: *asset,
                        amount: *amount,
                        backend: *backend,
                        payout: *payout,
                        refunded: false,
                    },
                );
            }
            block.emitted.push(Emitted {
                index: next_event(),
                topics: event.topics(),
                data: event.encode_data(),
            });
        }
        s.blocks.push(block);
    }

    fn check(s: &State, block: &Block, x: &[u8]) -> Check {
        let Some(d) = kreivo::decode_extrinsic(x) else {
            return Check::Drop;
        };
        let number = block.header.block.number;
        let birth = Mortality::birth_at(d.era, number);
        let (period, _) = Mortality::period_phase(d.era);
        if number >= birth + period {
            return Check::Drop;
        }
        let Some(birth_block) = s.blocks.get(birth as usize) else {
            return Check::Drop;
        };
        let p = TxParams {
            genesis: s.genesis,
            spec_version: s.version.spec_version,
            transaction_version: s.version.transaction_version,
            era: Mortality { birth, period },
            birth_hash: birth_block.header.block.hash,
            nonce: d.nonce,
            tip: d.tip,
        };
        let message = kreivo::message_to_sign(&kreivo::signing_payload(&d.call, &p));
        let Some((scheme, public)) = s.keys.get(&d.account) else {
            return Check::Drop;
        };
        if *scheme != d.scheme
            || !bezent_signer::verify::verifies(scheme.name(), public, &message, &d.signature)
        {
            return Check::Drop;
        }
        let current = block.nonces.get(&d.account).copied().unwrap_or(0);
        match u64::from(d.nonce).cmp(&current) {
            std::cmp::Ordering::Less => Check::Drop,
            std::cmp::Ordering::Greater => Check::Wait,
            std::cmp::Ordering::Equal => Check::Include(d.account, d.call),
        }
    }

    fn answer<T>(&self, f: impl FnOnce(&State) -> ChainResult<T>) -> ChainResult<T> {
        let s = self.lock();
        if s.unavailable {
            return Err(ChainError::Unavailable("the fake chain is offline".into()));
        }
        f(&s)
    }

    fn block_at<'a>(s: &'a State, at: &BlockRef) -> ChainResult<&'a Block> {
        s.blocks
            .get(at.number as usize)
            .filter(|b| b.header.block.hash == at.hash)
            .ok_or_else(|| ChainError::Unavailable(format!("no block {}", at.number)))
    }
}

/// The `input_data` of `ContractsApi_call` arguments ([`kreivo::contracts_api_call_args`]).
fn input_of(args: &[u8]) -> Option<Vec<u8>> {
    let mut b = args;
    <(
        AccountId32,
        AccountId32,
        u128,
        Option<Weight>,
        Option<u128>,
        Vec<u8>,
    )>::decode(&mut b)
    .ok()
    .filter(|_| b.is_empty())
    .map(|(.., input)| input)
}

enum Check {
    Include(AccountId32, Vec<u8>),
    Wait,
    Drop,
}

fn with_seq(event: ContractEvent, seq: u64) -> ContractEvent {
    match event {
        ContractEvent::Minted {
            reference,
            asset,
            to,
            amount,
            ..
        } => ContractEvent::Minted {
            seq,
            reference,
            asset,
            to,
            amount,
        },
        ContractEvent::Redeemed {
            reference,
            redeemer,
            asset,
            amount,
            backend,
            payout,
            ..
        } => ContractEvent::Redeemed {
            seq,
            reference,
            redeemer,
            asset,
            amount,
            backend,
            payout,
        },
        ContractEvent::RefundMinted {
            reference,
            to,
            asset,
            amount,
            ..
        } => ContractEvent::RefundMinted {
            seq,
            reference,
            to,
            asset,
            amount,
        },
        ContractEvent::Paused { scope, by, .. } => ContractEvent::Paused { seq, scope, by },
        ContractEvent::Unpaused { scope, .. } => ContractEvent::Unpaused { seq, scope },
        ContractEvent::BoundsSet { asset, bounds, .. } => {
            ContractEvent::BoundsSet { seq, asset, bounds }
        }
        ContractEvent::Other { name, mut data, .. } => {
            data.splice(..8.min(data.len()), seq.to_le_bytes());
            ContractEvent::Other { seq, name, data }
        }
    }
}

impl Chain for FakeChain {
    async fn genesis(&self) -> ChainResult<[u8; 32]> {
        self.answer(|s| Ok(s.genesis))
    }

    async fn runtime_version(&self, _at: &BlockRef) -> ChainResult<RuntimeVersion> {
        self.answer(|s| Ok(s.version))
    }

    async fn finalized(&self) -> ChainResult<BlockRef> {
        self.answer(|s| Ok(s.blocks[s.finalized as usize].header.block))
    }

    async fn header(&self, hash: &[u8; 32]) -> ChainResult<Header> {
        let mut s = self.lock();
        if s.unavailable {
            return Err(ChainError::Unavailable("the fake chain is offline".into()));
        }
        let found = s
            .blocks
            .iter()
            .find(|b| b.header.block.hash == *hash)
            .map(|b| b.header);
        if let Some(h) = found {
            s.header_log.push(h.block.number);
            if s.failing_headers.contains(&h.block.number) {
                return Err(ChainError::Unavailable(format!(
                    "the header of block {} cannot be read",
                    h.block.number
                )));
            }
        }
        found.ok_or_else(|| ChainError::Unavailable("no such block".into()))
    }

    async fn events(&self, at: &BlockRef, contract: &AccountId32) -> ChainResult<BlockEvents> {
        self.answer(|s| {
            if s.failing.contains(&at.number) {
                return Err(ChainError::Unavailable(format!(
                    "the events of block {} cannot be read",
                    at.number
                )));
            }
            let b = Self::block_at(s, at)?;
            Ok(BlockEvents {
                time_ms: b.time_ms,
                emitted: if *contract == s.contract {
                    b.emitted.clone()
                } else {
                    Vec::new()
                },
                results: b.results.clone(),
            })
        })
    }

    async fn extrinsics(&self, at: &BlockRef) -> ChainResult<Vec<Vec<u8>>> {
        self.answer(|s| Ok(Self::block_at(s, at)?.extrinsics.clone()))
    }

    async fn account_nonce(&self, at: &BlockRef, account: &AccountId32) -> ChainResult<u64> {
        self.answer(|s| {
            Ok(Self::block_at(s, at)?
                .nonces
                .get(account)
                .copied()
                .unwrap_or(0))
        })
    }

    async fn asset_issuance(&self, at: &BlockRef, asset: u32) -> ChainResult<Option<u128>> {
        self.answer(|s| {
            Ok(Self::block_at(s, at)?
                .supply
                .get(&asset)
                .map(|(_, issuance)| *issuance))
        })
    }

    async fn contract_code_hash(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> ChainResult<Option<[u8; 32]>> {
        self.answer(|s| {
            Self::block_at(s, at)?;
            Ok(if *contract == s.contract {
                s.code_hash
            } else {
                None
            })
        })
    }

    async fn contracts_api_call(&self, at: &BlockRef, args: &[u8]) -> ChainResult<Vec<u8>> {
        self.answer(|s| {
            let block = Self::block_at(s, at)?;
            let gas = Weight {
                ref_time: 2_000_000_000,
                proof_size: 60_000,
            };
            // The contract's getters answer from the block's state (review M1-bis).
            let getter = |value: Vec<u8>| {
                let mut data = vec![0u8];
                data.extend(value);
                kreivo::encode_dry_run(gas, 0, Ok((false, data)))
            };
            let input = input_of(args)
                .ok_or_else(|| ChainError::Invalid("ContractsApi_call arguments".into()))?;
            let (sel, mut rest) = (
                input.get(..4).unwrap_or_default(),
                input.get(4..).unwrap_or_default(),
            );
            if sel == kreivo::selector("mint_reference_used") {
                let reference = <[u8; 32]>::decode(&mut rest)
                    .map_err(|_| ChainError::Invalid("mint_reference_used input".into()))?;
                return Ok(getter(block.used.contains(&reference).encode()));
            }
            if sel == kreivo::selector("supply") {
                let kreivo::AssetId::Here(id) = kreivo::AssetId::decode(&mut rest)
                    .map_err(|_| ChainError::Invalid("supply input".into()))?;
                let count = block.supply.get(&id).map_or(0, |(c, _)| *c);
                return Ok(getter(count.encode()));
            }
            if sel == kreivo::selector("redemption") {
                let key = <(AccountId32, [u8; 32])>::decode(&mut rest)
                    .map_err(|_| ChainError::Invalid("redemption input".into()))?;
                return Ok(getter(block.redeemed.get(&key).copied().encode()));
            }
            Ok(match s.dry_run {
                DryRunMode::Pass => kreivo::encode_dry_run(gas, 1_000, Ok((false, vec![0, 0]))),
                DryRunMode::Refuse(e) => kreivo::encode_dry_run(gas, 0, Ok((true, vec![0, 1, e]))),
                DryRunMode::Trap => kreivo::encode_dry_run(gas, 0, Err(())),
            })
        })
    }

    async fn submit(&self, extrinsic: &[u8]) -> ChainResult<()> {
        let mut s = self.lock();
        if s.unavailable || s.refuse_submissions {
            return Err(ChainError::Unavailable(
                "the node refused the submission".into(),
            ));
        }
        s.submitted.push(extrinsic.to_vec());
        if !s.pool.iter().any(|x| x == extrinsic)
            && !s
                .blocks
                .iter()
                .any(|b| b.extrinsics.iter().any(|x| x == extrinsic))
        {
            s.pool.push(extrinsic.to_vec());
        }
        Ok(())
    }
}

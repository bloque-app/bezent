// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared by the endpoint's tests. Every test runs in a fresh database that `#[sqlx::test]` creates
//! on the server `DATABASE_URL` names, with bezent's migrations applied, against two in-memory
//! providers (`MockRpc`): no test here reaches a network. The signer runs in process, over its real
//! link, with Foundry's test account 4 as the delivery key (a test key with no power anywhere,
//! `REQ-KY-6`). Each pool acts as one role for its whole life: one pool, one role (#27).

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::primitives::{Address, B256, Bytes, U256, keccak256};
use bezent_evm_endpoint::evm::{self, Contracts, Delivery, Gas};
use bezent_evm_endpoint::network::{Declarations, Token};
use bezent_evm_endpoint::observe::{Follower, Watch};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::proof::Prover;
use bezent_evm_endpoint::rpc::mock::{MockChain, MockRpc};
use bezent_evm_endpoint::rpc::{Receipt, Tx};
use bezent_evm_endpoint::submit::{
    GasBounds, Lane, Prepared, SignerLink, Submitter, decode_signed, signing_intent_payload,
};
use bezent_signer::config::{Caller, Config};
use bezent_signer::keys::{FileBackend, KeyBackend};
use bezent_signer::link::{self, Callers};
use bezent_signer::log::Log;
use bezent_signer::signer::Signer;
use k256::ecdsa::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The origin network.
pub const NETWORK: &str = "polygon";
/// Its chain id.
pub const CHAIN_ID: u64 = 137;
/// The most the lane pays per gas: the endpoint's `max_fee_per_gas`, and the signer's cap on a
/// cancel's (PR #39 second review SF2).
pub const MAX_FEE: u128 = 3_000_000_000_000;
/// The issuing network.
pub const ISSUING: &str = "kreivo";
/// The asset.
pub const ASSET: &str = "bUSDC";
/// The token the endpoint presents to the signer.
pub const TOKEN: &str = "evm-endpoint-token-for-tests";
/// Foundry's test account 4: the delivery key.
pub const LANE_SECRET: &str = "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a";
/// The key's id.
pub const KEY: &str = "polygon-payout";

/// Native USDC on Polygon (F-008 NOTES).
pub fn usdc() -> Address {
    "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359"
        .parse()
        .unwrap()
}
/// USDT0 on Polygon, an admitted convertible.
pub fn usdt0() -> Address {
    "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
        .parse()
        .unwrap()
}
/// USDT0's declared pool.
pub fn pool_address() -> Address {
    "0x31083a78E11B18e450fd139F9ABEa98CD53181B7"
        .parse()
        .unwrap()
}
/// The vault.
pub fn vault() -> Address {
    Address::repeat_byte(0xa1)
}
/// The vault's contracts.
pub fn contracts() -> Contracts {
    Contracts {
        roles: Address::repeat_byte(0xa2),
        multisend: "0x9641d764fc13c8B624c04430C7356C1C7C8102e2"
            .parse()
            .unwrap(),
        emitter: Address::repeat_byte(0xa3),
        delivery_role: evm::role_key("bezent.delivery").unwrap(),
    }
}
/// A beneficiary.
pub fn beneficiary() -> Address {
    Address::repeat_byte(0xbe)
}
/// A depositor.
pub fn depositor() -> Address {
    Address::repeat_byte(0xde)
}

/// The vault's spelling in the store: lower case, as an operator might write it.
pub fn vault_text() -> String {
    format!("{:#x}", vault())
}

/// The lane's signing key.
pub fn lane_key() -> SigningKey {
    SigningKey::from_slice(&hex::decode(&LANE_SECRET[2..]).unwrap()).unwrap()
}

/// A fresh directory under the system's temporary directory.
pub fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bze-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The current process's user id.
pub fn my_uid() -> u32 {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    a.set_nonblocking(true).unwrap();
    tokio::net::UnixStream::from_std(a)
        .unwrap()
        .peer_cred()
        .unwrap()
        .uid()
}

/// The test-only password of the logins these tests make.
const LOGIN_PASSWORD: &str = "test-only-evm-endpoint-tests-login";

/// A pool on `connect`'s database logged in as a login of its own that is a member of `role` and
/// of nothing else, as each service's login is: one pool, one login, never a switched role (#27,
/// #46). Logins are created once per server.
pub async fn role_pool(connect: &PgConnectOptions, role: &'static str) -> PgPool {
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(connect.clone())
        .await
        .unwrap();
    let pool = bezent_store::login::connect_login(
        &owner,
        &format!("evm_endpoint_tests_{role}"),
        role,
        LOGIN_PASSWORD,
        PgPoolOptions::new().max_connections(3),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    owner.close().await;
    pool
}

/// Declares the networks, the asset, its token on the origin network and the vault as its
/// reserve, as the operator would.
pub async fn declare(admin: &PgPool) {
    declare_vault(admin, &vault_text()).await;
}

/// [`declare`], with the vault at `vault`.
pub async fn declare_vault(admin: &PgPool, vault: &str) {
    sqlx::raw_sql(&format!(
        "INSERT INTO networks (network, role, address_format, backend) VALUES \
           ('{ISSUING}', 'issuing', 'ss58', NULL), ('{NETWORK}', 'origin', 'evm', 'evm:{CHAIN_ID}'); \
         INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
           ('{ASSET}', 6, 'public', 'Here(101)'); \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('{NETWORK}', '{}', '{ASSET}', 6); \
         INSERT INTO reserves (asset, network, address) VALUES ('{ASSET}', '{NETWORK}', '{}');",
        usdc(),
        vault
    ))
    .execute(admin)
    .await
    .unwrap();
}

/// The declarations as the endpoint reads them.
pub fn declarations() -> Declarations {
    Declarations {
        vault_text: vault_text(),
        tokens: [(
            usdc(),
            Token {
                text: usdc().to_string(),
                asset: ASSET.into(),
            },
        )]
        .into_iter()
        .collect(),
    }
}

/// Starts the signer in process, over its real link, with the delivery key; returns the link and
/// the lane.
pub async fn start_signer(connect: &PgConnectOptions) -> (SignerLink, Lane) {
    start_signer_for(connect, contracts()).await
}

/// As [`start_signer`], with the vault's contracts `vault` declared to the signer: what a payout's
/// bytes must name (audit must-fix 2).
pub async fn start_signer_for(connect: &PgConnectOptions, vault: Contracts) -> (SignerLink, Lane) {
    let dir = temp_dir();
    let keys_path = dir.join("keys.json");
    std::fs::write(
        &keys_path,
        serde_json::to_vec(&json!({
            "comment": "TEST KEYS: Foundry's test account 4",
            "environment": "test",
            "keys": [{"id": KEY, "scheme": "secp256k1-evm", "seed": LANE_SECRET}],
        }))
        .unwrap(),
    )
    .unwrap();
    let uid = my_uid();
    let caller = Caller {
        name: "evm-endpoint".into(),
        uid,
        token_sha256: vec![hex::encode(Sha256::digest(TOKEN.as_bytes()))],
    };
    let config_path = dir.join("signer.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "socket": dir.join("s.sock"),
            "callers": [{"name": caller.name, "uid": uid, "token_sha256": caller.token_sha256}],
            "networks": [{"id": NETWORK, "environment": "test", "chain_id": CHAIN_ID,
                          "cancel_max_fee_per_gas": MAX_FEE.to_string(),
                          // What a payout's bytes must name (audit must-fix 2).
                          "delivery": {"roles": vault.roles.to_string(),
                                       "multisend": vault.multisend.to_string(),
                                       "emitter": vault.emitter.to_string(),
                                       "role": "bezent.delivery"}}],
            "keys": {"backend": "file", "path": keys_path},
            "bindings": [{"key": KEY, "network": NETWORK, "purposes": ["payout", "cancel"],
                          "window_cap": "1000000000000", "window_seconds": 3600}],
        }))
        .unwrap(),
    )
    .unwrap();
    let config = Config::load(&config_path).unwrap();
    let backend = FileBackend::load(&keys_path).unwrap();
    let policy = config.policy(&backend).unwrap();
    let signer_pool = role_pool(connect, bezent_store::SIGNER_ROLE).await;
    let (log, _captured) = Log::capture();
    let signer = Arc::new(Signer::new(
        signer_pool,
        policy,
        FileBackend::load(&keys_path).unwrap(),
        log,
    ));
    let socket = dir.join("s.sock");
    let listener = link::bind(&socket).unwrap();
    tokio::spawn(link::serve(
        listener,
        Arc::new(Callers::sharing_the_signer_uid_in_tests(&[caller])),
        signer,
    ));
    let public_key = backend.public_key(KEY).unwrap();
    let address = evm::address_of(&public_key).unwrap();
    (
        SignerLink {
            socket,
            uid,
            token: TOKEN.into(),
        },
        Lane {
            key: KEY.into(),
            public_key,
            address,
        },
    )
}

/// The endpoint over a test database, two providers and an in-process signer.
pub struct Fixture {
    /// The migration owner's pool, for reading anything.
    pub admin: PgPool,
    /// The endpoint's pool: `bezent_evm_endpoint`.
    pub endpoint: PgPool,
    /// The core's pool: it records intents.
    pub core: PgPool,
    /// The chain provider `a` reads.
    pub chain: MockChain,
    /// The chain provider `b` reads (the same one unless a test forks it).
    pub chain_b: MockChain,
    /// The two providers.
    pub pair: Pair<MockRpc>,
    /// The lane.
    pub lane: Lane,
    /// The signer link.
    pub link: SignerLink,
    /// The store's declarations.
    pub declarations: Declarations,
    /// The convertibles.
    pub convertibles: Vec<(Address, Address)>,
}

impl Fixture {
    /// Builds the fixture over a chain of `blocks` final blocks.
    pub async fn new(pool: PgPoolOptions, connect: PgConnectOptions, blocks: u64) -> Fixture {
        let chain = MockChain::new(CHAIN_ID, blocks);
        Self::with_chains(pool, connect, chain.clone(), chain).await
    }

    /// Builds the fixture over two chains, one per provider.
    pub async fn with_chains(
        pool: PgPoolOptions,
        connect: PgConnectOptions,
        chain: MockChain,
        chain_b: MockChain,
    ) -> Fixture {
        let admin = pool.connect_with(connect.clone()).await.unwrap();
        declare(&admin).await;
        let (link, lane) = start_signer(&connect).await;
        let address = lane.address;
        chain.set_balance(address, U256::from(10u128.pow(21)));
        Fixture {
            endpoint: role_pool(&connect, "bezent_evm_endpoint").await,
            core: role_pool(&connect, "bezent_core").await,
            admin,
            pair: Pair {
                a: MockRpc::new("a", &chain),
                b: MockRpc::new("b", &chain_b),
            },
            chain,
            chain_b,
            lane,
            link,
            declarations: declarations(),
            convertibles: vec![(usdt0(), pool_address())],
        }
    }

    /// The follower.
    pub fn follower(&self) -> Follower<'_, MockRpc> {
        Follower {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            watch: Watch {
                vault: vault(),
                emitter: contracts().emitter,
                declarations: &self.declarations,
                convertibles: &self.convertibles,
            },
            start_block: 1,
            max_range: 4,
        }
    }

    /// The submitter.
    pub fn submitter(&self) -> Submitter<'_, MockRpc> {
        Submitter {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            chain_id: CHAIN_ID,
            contracts: contracts(),
            vault: vault(),
            declarations: &self.declarations,
            lane: &self.lane,
            signer: &self.link,
            gas: GasBounds {
                max_gas: 500_000,
                max_fee: MAX_FEE,
                margin_percent: 130,
            },
        }
    }

    /// The prover.
    pub fn prover(&self) -> Prover<'_, MockRpc> {
        Prover {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            lane: &self.lane,
            contracts: contracts(),
        }
    }

    /// Records an intent as the core, and returns its id.
    pub async fn intent(&self, purpose: &str, reference: Option<&[u8]>, payload: Value) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
             VALUES ($1, $2, $3, $4::text::jsonb) RETURNING id",
        )
        .bind(NETWORK)
        .bind(purpose)
        .bind(reference)
        .bind(payload.to_string())
        // A delivery order rests on a transfer and a final observation the store checks (0049, the
        // audit of PR #56): these tests are the endpoint's, with no transfer behind the order, so
        // an order is recorded as the migration owner, as a drill would. Every other intent is the
        // core's login's.
        .fetch_one(
            if payload.get("order").is_some() && payload.get("prepared").is_none() {
                &self.admin
            } else {
                &self.core
            },
        )
        .await
        .expect("the core records the intent")
    }

    /// Records an intent that the store's signing-intent check (0037, PR #39 third review MF-A)
    /// refuses: first proves the refusal, then records it with the check off, as the migration
    /// owner, the way a core that wrote past the store would, so a test reaches the endpoint's own
    /// check behind the store's.
    pub async fn intent_past_the_store(
        &self,
        purpose: &str,
        reference: Option<&[u8]>,
        payload: Value,
    ) -> i64 {
        let insert = "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
                      VALUES ($1, $2, $3, $4::text::jsonb) RETURNING id";
        let refused = sqlx::query_scalar::<_, i64>(insert)
            .bind(NETWORK)
            .bind(purpose)
            .bind(reference)
            .bind(payload.to_string())
            .fetch_one(&self.core)
            .await
            .expect_err("the store recorded a signing intent its check refuses");
        assert_eq!(
            refused
                .as_database_error()
                .and_then(|d| d.code())
                .as_deref(),
            Some(bezent_store::REFUSAL_SQLSTATE),
            "{refused}"
        );
        let mut tx = self.admin.begin().await.unwrap();
        sqlx::raw_sql("ALTER TABLE intents DISABLE TRIGGER intents_guard_prepared")
            .execute(&mut *tx)
            .await
            .unwrap();
        let id: i64 = sqlx::query_scalar(insert)
            .bind(NETWORK)
            .bind(purpose)
            .bind(reference)
            .bind(payload.to_string())
            .fetch_one(&mut *tx)
            .await
            .expect("record the intent past the store's check");
        sqlx::raw_sql("ALTER TABLE intents ENABLE TRIGGER intents_guard_prepared")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// Records a payout order as the core would (F-003: `{"order": {to, asset, token}, amount}`).
    pub async fn order_payout(&self, reference: B256, to: Address, amount: u128) -> i64 {
        self.intent(
            "payout",
            Some(reference.as_slice()),
            json!({"order": {"to": to.to_string(), "asset": ASSET, "token": usdc().to_string()},
                   "amount": amount.to_string()}),
        )
        .await
    }

    /// Records a cancel order for signed transaction `signed`, as the core would (`REQ-NX-4`).
    pub async fn order_cancel(&self, signed: i64) -> i64 {
        self.intent(
            "cancel",
            None,
            json!({"order": {"cancel": signed}, "amount": "0"}),
        )
        .await
    }

    /// Records the signing intent for a prepared transaction, as the core would.
    pub async fn signing_intent(
        &self,
        purpose: &str,
        reference: Option<&[u8]>,
        prepared: &Prepared,
        amount: u128,
    ) -> i64 {
        let Prepared::Ready { id, message, .. } = prepared else {
            panic!("not ready: {prepared:?}");
        };
        self.intent(
            purpose,
            reference,
            signing_intent_payload(*id, message, amount),
        )
        .await
    }

    /// Order, preparation, signing intent, signature and submission of one payout. Returns the
    /// signed transaction's id and its bytes.
    pub async fn signed_payout(&self, reference: B256, amount: u128) -> (i64, Vec<u8>) {
        let order = self.order_payout(reference, beneficiary(), amount).await;
        let prepared = self.submitter().prepare(order).await.unwrap();
        let intent = self
            .signing_intent("payout", Some(reference.as_slice()), &prepared, amount)
            .await;
        let submitted = self.submitter().sign_and_submit(intent).await.unwrap();
        let bezent_evm_endpoint::submit::Submitted::Submitted { id, .. } = submitted else {
            panic!("not submitted: {submitted:?}");
        };
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT bytes FROM signed_transactions WHERE id = $1")
                .bind(id)
                .fetch_one(&self.admin)
                .await
                .unwrap();
        (id, bytes)
    }

    /// A count from the admin pool.
    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    /// Mines `bytes` (a signed EIP-1559 transaction) into a new block of both chains, consuming
    /// the sender's nonce there; `executed` decides its receipt, and an executed payout emits its
    /// transfer and its reference. Returns the block.
    pub fn mine(&self, bytes: &[u8], executed: bool) -> u64 {
        let (tx, signature) = decode_signed(bytes).unwrap();
        let t = block_tx(&tx, signature, keccak256(bytes), 0);
        self.mine_tx(t, executed)
    }

    /// Mines `t` into a new block of both chains (the same block if they are one).
    pub fn mine_tx(&self, mut t: Tx, executed: bool) -> u64 {
        let mut number = 0;
        for c in self.chains() {
            let h = c.mine();
            number = h.number;
            t.index = 0;
            c.add_tx(number, t.clone());
            c.set_nonce(t.from, number, t.nonce + 1);
            c.add_receipt(Receipt {
                tx_hash: t.hash,
                block_number: number,
                block_hash: h.hash,
                tx_index: 0,
                status: executed,
            });
            if executed
                && t.to == Some(contracts().roles)
                && let Some(d) = evm::decode_delivery_call(&contracts(), &t.input)
            {
                payout_logs(&c, number, t.hash, &d);
            }
        }
        number
    }

    /// Every distinct chain the providers read.
    pub fn chains(&self) -> Vec<MockChain> {
        let mut v = vec![self.chain.clone()];
        if !self.chain.same(&self.chain_b) {
            v.push(self.chain_b.clone());
        }
        v
    }

    /// Mines `n` empty blocks on every chain and finalizes them all.
    pub fn advance(&self, n: u64) {
        for c in self.chains() {
            for _ in 0..n {
                c.mine();
            }
            c.finalize(c.latest());
        }
    }
}

/// The logs of an executed delivery: the token's `Transfer` from the vault, then the emitter's
/// `Reference`, consecutive.
pub fn payout_logs(c: &MockChain, number: u64, tx: B256, d: &Delivery) {
    c.log(
        number,
        0,
        tx,
        0,
        d.token,
        vec![evm::TRANSFER_TOPIC, vault().into_word(), d.to.into_word()],
        d.amount.to_be_bytes::<32>().to_vec(),
    );
    c.log(
        number,
        1,
        tx,
        0,
        contracts().emitter,
        vec![evm::REFERENCE_TOPIC, d.reference],
        vec![],
    );
}

/// A `Transfer` log of `token` in block `number`, in the transaction at index `tx[0]`.
#[allow(clippy::too_many_arguments)]
pub fn transfer_log(
    c: &MockChain,
    number: u64,
    log_index: u64,
    tx: B256,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) {
    c.log(
        number,
        log_index,
        tx,
        u64::from(tx[0]),
        token,
        vec![evm::TRANSFER_TOPIC, from.into_word(), to.into_word()],
        amount.to_be_bytes::<32>().to_vec(),
    );
}

/// A block's listing of an EIP-1559 transaction with `signature` (`r ‖ s ‖ parity`).
pub fn block_tx(tx: &TxEip1559, signature: Vec<u8>, hash: B256, index: u64) -> Tx {
    let (&v, rs) = signature.split_last().unwrap();
    let from = alloy::primitives::Signature::from_bytes_and_parity(rs, v == 1)
        .recover_address_from_prehash(&tx.signature_hash())
        .unwrap();
    Tx {
        hash,
        index,
        tx_type: evm::EIP1559,
        from,
        nonce: tx.nonce,
        to: tx.to.to().copied(),
        value: tx.value,
        input: tx.input.clone(),
        chain_id: Some(tx.chain_id),
        gas: tx.gas_limit,
        max_fee_per_gas: Some(tx.max_fee_per_gas),
        max_priority_fee_per_gas: Some(tx.max_priority_fee_per_gas),
        access_list_empty: true,
        signature,
    }
}

/// Signs `tx` with the lane's key outside the signer (what a stolen key would do).
pub fn sign_outside(tx: &TxEip1559) -> Tx {
    let (sig, recovery) = lane_key()
        .sign_prehash_recoverable(tx.signature_hash().as_slice())
        .unwrap();
    let mut signature = sig.to_bytes().to_vec();
    signature.push(recovery.to_byte());
    let signed = evm::signed(tx, &signature).unwrap();
    block_tx(tx, signature, signed.hash, 0)
}

/// An EIP-1559 transaction from the lane at `nonce` to `to` with `input`.
pub fn lane_tx(nonce: u64, to: Address, input: Vec<u8>) -> TxEip1559 {
    evm::transaction(
        CHAIN_ID,
        nonce,
        to,
        input,
        Gas {
            limit: 100_000,
            max_fee: 100_000_000_000,
            priority_fee: 30_000_000_000,
        },
    )
}

/// The payout of `amount` to the beneficiary with `reference`.
pub fn delivery(reference: B256, amount: u128) -> Delivery {
    Delivery {
        token: usdc(),
        to: beneficiary(),
        amount: U256::from(amount),
        reference,
    }
}

/// An empty output for `eth_call`.
pub fn ok_call() -> Result<Bytes, (String, Option<Bytes>)> {
    Ok(Bytes::from(vec![0u8; 32]))
}

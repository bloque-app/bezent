// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The world the core's tests run in. Every test runs in a fresh database that `#[sqlx::test]`
//! creates on the server `DATABASE_URL` names, with the store's migrations applied by the owner.
//!
//! - The core runs on a pool of its own, logged in as a login that is a member of `bezent_core`
//!   and of nothing else, as the core's login is; one pool, one login, no role switched (issues
//!   #27, #46). The signer and the operators have logins of their own too.
//! - Declarations, observations, final heads and proofs are written as the owner, as the operator
//!   and the endpoints would, into F-007's tables (`prepared_transactions`,
//!   `non_execution_proofs`). The Kreivo endpoint's own tests run these steps through the endpoint
//!   itself (`services/kreivo-endpoint/tests/delivery.rs`).
//! - Signatures come from the real signer (F-005), in process, with test keys, over the signing
//!   intents the core recorded; the endpoint's persisted bytes are written as the endpoint would.

#![allow(dead_code)]

pub mod reserve;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bezent_core_service::{Core, Report, Settings};
use bezent_signer::config::Config as SignerConfig;
use bezent_signer::keys::FileBackend;
use bezent_signer::log::Log;
use bezent_signer::signer::Signer;
use bezent_signer::{Outcome, Signed};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

pub const ISSUING: &str = "kreivo-test";
pub const ORIGIN: &str = "polygon-test";
pub const ASSET: &str = "bUSDC";
pub const ISSUING_ASSET: &str = "Here(101)";
/// The origin token: an EVM address, as the EVM endpoint requires of every admitted token there
/// (its delivery allowance key is derived from it; PR #52 review S-2).
pub const TOKEN: &str = "0x41e94eb019c0762f9bfcf9fb1e58725bfb0e7582";
pub const RESERVE: &str = "0x00000000000000000000000000000000000000aa";
/// Alice, prefix 42, and her account.
pub const DESTINATION: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
pub const DESTINATION_ACCOUNT: &str =
    "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d";
pub const REDEEMER: &str = "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48";
pub const BENEFICIARY: &str = "0x52908400098527886e0f7030069857d2e4169ee7";
/// The route's completion budget, the deadline of every leg (F-003 NOTES).
pub const BUDGET: i64 = 3600;

/// The start of every test's clock: a Tuesday.
pub fn t0() -> DateTime<Utc> {
    "2026-10-13T00:00:00Z".parse().unwrap()
}

/// The core's login in these tests: a member of `bezent_core` and of nothing else, like the core's
/// login (#46). Created once per server; tests run in parallel.
pub const CORE_LOGIN: &str = "core_tests_core_login";
/// The signer's login in these tests: a member of `bezent_signer` only.
pub const SIGNER_LOGIN: &str = "core_tests_signer_login";
const LOGIN_PASSWORD: &str = "test-only-core-tests-login";

/// Two operators, each on a login of their own that is a member of `bezent_operator`, as every
/// operator logs in (REQ-GV-5; review M3). Created once per server, with a test-only password.
pub const OPERATORS: [&str; 2] = ["ops_alice_for_tests", "ops_bob_for_tests"];
const OPERATOR_PASSWORD: &str = "test-only-operator-login";

/// A pool on `admin`'s database logged in as `name`, made a member of `role` and of nothing else:
/// one pool, one login, never a switched role (issues #27, #46).
pub async fn login_pool(admin: &PgPool, name: &str, role: &str, max: u32) -> PgPool {
    let password = if role == bezent_store::OPERATOR_ROLE {
        OPERATOR_PASSWORD
    } else {
        LOGIN_PASSWORD
    };
    bezent_store::login::connect_login(
        admin,
        name,
        role,
        password,
        PgPoolOptions::new().max_connections(max),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"))
}

/// A pool that logs in as operator `name`: one pool, one login (issue #27).
pub async fn operator_login(admin: &PgPool, _connect: &PgConnectOptions, name: &str) -> PgPool {
    login_pool(admin, name, bezent_store::OPERATOR_ROLE, 2).await
}

pub struct World {
    /// The migration owner: the endpoints write through it.
    pub admin: PgPool,
    /// The two operators, on their own logins (`OPERATORS`).
    pub ops: [PgPool; 2],
    /// The core, on its own pool.
    pub core: Core,
    /// The signer, in process.
    pub signer: Arc<Signer<FileBackend>>,
    pub connect: PgConnectOptions,
}

fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bzc-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The most a cancel may pay per gas on the origin network, in wei (the signer's bound, SF2).
pub const CANCEL_MAX_FEE: u128 = 1_000_000_000;

/// The on-chain component's account on the issuing test network (the signer's policy names it).
pub const CONTRACT: [u8; 32] = [0xc0; 32];
/// The issuing test network's genesis hash.
pub const GENESIS: [u8; 32] = [0x6b; 32];

/// The vault's contracts on the origin network, as the signer's policy declares them: what a
/// payout's bytes must name (audit must-fix 2).
pub fn contracts() -> bezent_signer::delivery::EvmContracts {
    bezent_signer::delivery::EvmContracts {
        roles: [0xa2; 20],
        multisend: [0xa4; 20],
        emitter: [0xa3; 20],
        role_key: bezent_signer::delivery::role_key("bezent.delivery").unwrap(),
    }
}

/// The seed of test key `id`.
fn test_seed(id: &str) -> [u8; 32] {
    Sha256::digest(format!("bezent core test key {id}")).into()
}

/// The EVM address of the test payout key: the lane a cancel is a transfer of nothing to.
pub fn payout_lane() -> [u8; 20] {
    use sha3::Digest as _;
    let key = k256::ecdsa::SigningKey::from_slice(&test_seed("polygon-payout")).unwrap();
    let point = key.verifying_key().to_encoded_point(false);
    let hash = sha3::Keccak256::digest(&point.as_bytes()[1..]);
    hash[12..].try_into().unwrap()
}

/// The test signer: test keys from fixed labels, tagged `test`, and bindings for the two networks.
fn signer_files() -> (FileBackend, SignerConfig) {
    let dir = temp_dir();
    let keys: Vec<Value> = [
        ("kreivo-mint", "sr25519"),
        ("polygon-payout", "secp256k1-evm"),
    ]
    .iter()
    .map(|(id, scheme)| {
        let seed = test_seed(id);
        json!({"id": id, "scheme": scheme, "seed": format!("0x{}", hex::encode(seed))})
    })
    .collect();
    let key_path = dir.join("test-keys.json");
    std::fs::write(
        &key_path,
        serde_json::to_vec(&json!({"comment": "TEST KEYS", "environment": "test", "keys": keys}))
            .unwrap(),
    )
    .unwrap();
    let config = json!({
        "socket": dir.join("s.sock"),
        "callers": [{"name": "core", "uid": 1, "token_sha256": [hex::encode(Sha256::digest(b"t"))]}],
        "networks": [
            {"id": ISSUING, "environment": "test", "issuing": true,
             "contract_account": format!("0x{}", hex::encode(CONTRACT)),
             "genesis": format!("0x{}", hex::encode(GENESIS))},
            {"id": ORIGIN, "environment": "test", "chain_id": 80002,
             "cancel_max_fee_per_gas": CANCEL_MAX_FEE.to_string(),
             "delivery": {"roles": format!("0x{}", hex::encode(contracts().roles)),
                          "multisend": format!("0x{}", hex::encode(contracts().multisend)),
                          "emitter": format!("0x{}", hex::encode(contracts().emitter)),
                          "role": "bezent.delivery"}},
        ],
        "keys": {"backend": "file", "path": key_path},
        "bindings": [
            {"key": "kreivo-mint", "network": ISSUING, "purposes": ["mint", "refund-mint"],
             "window_cap": "1000000000000", "window_seconds": 3600},
            {"key": "polygon-payout", "network": ORIGIN, "purposes": ["payout", "cancel"],
             "window_cap": "1000000000000", "window_seconds": 3600},
        ],
    });
    let config = SignerConfig::parse(&serde_json::to_vec(&config).unwrap()).unwrap();
    (FileBackend::load(&key_path).unwrap(), config)
}

/// Runs `sql` as the owner.
pub async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(sql)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
}

/// Reads one value as the owner.
pub async fn scalar<T>(pool: &PgPool, sql: &str) -> T
where
    T: for<'r> sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres> + Send + Unpin,
{
    sqlx::query_scalar(sql)
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

impl World {
    pub async fn new(options: PgPoolOptions, connect: PgConnectOptions) -> Self {
        let admin = bezent_store::login::guard(options)
            .connect_with(connect.clone())
            .await
            .unwrap();
        seed(&admin).await;
        let core_pool = login_pool(&admin, CORE_LOGIN, bezent_store::CORE_ROLE, 4).await;
        let (backend, config) = signer_files();
        let policy = config.policy(&backend).unwrap();
        let signer_pool = login_pool(&admin, SIGNER_LOGIN, bezent_store::SIGNER_ROLE, 4).await;
        let (log, _captured) = Log::capture();
        let signer = Arc::new(Signer::new(signer_pool, policy, backend, log));
        let ops = [
            operator_login(&admin, &connect, OPERATORS[0]).await,
            operator_login(&admin, &connect, OPERATORS[1]).await,
        ];
        Self {
            admin,
            ops,
            core: Core::new(
                core_pool,
                Settings {
                    retry_preparation_after: Duration::zero(),
                    default_deadline: Duration::seconds(BUDGET),
                    ..Settings::default()
                },
            ),
            signer,
            connect,
        }
    }

    /// A core restarted from nothing but the store.
    pub async fn restarted(&self) -> Core {
        let pool = login_pool(&self.admin, CORE_LOGIN, bezent_store::CORE_ROLE, 2).await;
        Core::new(
            pool,
            Settings {
                retry_preparation_after: Duration::zero(),
                default_deadline: Duration::seconds(BUDGET),
                ..Settings::default()
            },
        )
    }

    /// One cycle at `now`.
    pub async fn cycle(&self, now: DateTime<Utc>) -> Report {
        let report = self.core.cycle(now).await.expect("cycle");
        assert_eq!(report.failed, 0, "a step failed: {report:?}");
        report
    }

    /// Cycles at `now` until nothing moves (at most ten).
    pub async fn settle(&self, now: DateTime<Utc>) {
        for _ in 0..10 {
            let r = self.cycle(now).await;
            if r.moved == 0 && r.intents == 0 && r.withdrawals == 0 {
                return;
            }
        }
        panic!("the core never settled");
    }

    pub async fn state(&self, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT state::text, condition::text FROM transfers WHERE id = $1")
            .bind(id)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    pub async fn count(&self, sql: &str) -> i64 {
        scalar(&self.admin, sql).await
    }
}

/// The declarations: the issuing network, one origin network named by its backend, the asset with
/// its issuing-network id, its token, reserve and both routes.
pub async fn seed(pool: &PgPool) {
    exec(
        pool,
        &format!(
            "INSERT INTO networks (network, role, address_format, backend) VALUES \
               ('{ISSUING}', 'issuing', 'ss58', NULL), ('{ORIGIN}', 'origin', 'evm', 'evm:80002'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset, surplus_floor, \
                 surplus_alert) VALUES ('{ASSET}', 6, 'public', '{ISSUING_ASSET}', 0, 1); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('{ORIGIN}', '{TOKEN}', '{ASSET}', 6); \
             INSERT INTO reserves (asset, network, address) VALUES ('{ASSET}', '{ORIGIN}', '{RESERVE}'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('{ASSET}', '{ORIGIN}', 'deposit', 1, 1000000000, 10000000000, 86400, 100, '[]', {BUDGET}), \
               ('{ASSET}', '{ORIGIN}', 'withdrawal', 1, 1000000000, 10000000000, 86400, 100, '[]', {BUDGET});"
        ),
    )
    .await;
}

pub fn hash(network: &str, block: i64) -> Vec<u8> {
    let mut h = vec![if network == ISSUING { 0xcc } else { 0xab }];
    h.extend_from_slice(&block.to_be_bytes());
    h
}

/// An observation the endpoint recorded at (`network`, `block`, log `index`); returns its id.
#[allow(clippy::too_many_arguments)]
pub async fn observe(
    pool: &PgPool,
    network: &str,
    block: i64,
    index: i64,
    token: &str,
    amount: u128,
    sender: Option<&str>,
    recipient: Option<&str>,
    reference: Option<&[u8]>,
    executed: bool,
    detail: Value,
) -> i64 {
    // The kind the endpoint writes (migration 0034): the one its event names, and, for these
    // fixtures, an arrival when an origin network's observation names no event.
    sqlx::query_scalar(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
             position_index, token, amount, sender, recipient, on_chain_reference, executed, detail, \
             kind) \
         VALUES ($1, $2, $3, $4, 'log', $5, $6, $7::numeric, $8, $9, $10, $11, $12, \
             CASE WHEN $12 ? 'event' THEN observation_kind_for_event($12 ->> 'event') \
                  WHEN $1 = $13 THEN 'other' ELSE 'arrival' END) RETURNING id",
    )
    .bind(network)
    .bind(block)
    .bind(hash(network, block))
    .bind(t0() + Duration::seconds(block))
    .bind(index)
    .bind(token)
    .bind(amount.to_string())
    .bind(sender)
    .bind(recipient)
    .bind(reference)
    .bind(executed)
    .bind(detail)
    .bind(ISSUING)
    .fetch_one(pool)
    .await
    .expect("observe")
}

/// The final head (`network`, `block`) at block time t0 + `block` seconds.
pub async fn head(pool: &PgPool, network: &str, block: i64) {
    sqlx::query(
        "INSERT INTO final_heads (network, block_number, block_hash, block_time) \
         VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
    )
    .bind(network)
    .bind(block)
    .bind(hash(network, block))
    .bind(t0() + Duration::seconds(block))
    .execute(pool)
    .await
    .unwrap();
}

/// Decides observation `id` `outcome` (`final` or `withdrawn`) at the final head
/// (its network, `block`), recording the head.
pub async fn decide(pool: &PgPool, id: i64, outcome: &str, block: i64) {
    let network: String = scalar(
        pool,
        &format!("SELECT network FROM observations WHERE id = {id}"),
    )
    .await;
    head(pool, &network, block).await;
    sqlx::query(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
             final_block_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(outcome)
    .bind(block)
    .bind(hash(&network, block))
    .execute(pool)
    .await
    .unwrap();
}

/// An arrival of `amount` of the origin token at the reserve, at (`ORIGIN`, `block`, log 0).
pub async fn arrival(pool: &PgPool, block: i64, amount: u128) -> i64 {
    observe(
        pool,
        ORIGIN,
        block,
        0,
        TOKEN,
        amount,
        Some("0xfunder"),
        Some(RESERVE),
        None,
        true,
        json!({}),
    )
    .await
}

/// A deposit request binding the arrival at (`ORIGIN`, `block`, log 0), taken in as the ingress
/// does: `Received`, then `Accepted` (F-002 NOTES, choice 1). Returns the transfer.
pub async fn deposit(pool: &PgPool, block: i64, amount: u128) -> i64 {
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO transfers (bridge_reference, request_reference, kind, asset, amount, \
             destination_network, destination, refund_destination_network, refund_destination, \
             source_network, source_block_number, source_block_hash, source_position_kind, \
             source_position_index, state) \
         VALUES ('bz-d-' || $1, 'req-' || $1, 'deposit', $2, $3::numeric, $4, $5, $6, $7, $6, $1, \
             $8, 'log', 0, 'Received') RETURNING id",
    )
    .bind(block)
    .bind(ASSET)
    .bind(amount.to_string())
    .bind(ISSUING)
    .bind(DESTINATION)
    .bind(ORIGIN)
    .bind(BENEFICIARY)
    .bind(hash(ORIGIN, block))
    .fetch_one(pool)
    .await
    .unwrap();
    exec(
        pool,
        &format!(
            "INSERT INTO transitions (transfer_id, from_state, to_state) VALUES ({id}, 'Received', 'Accepted')"
        ),
    )
    .await;
    id
}

/// A redemption of `amount` on the issuing network at (`ISSUING`, `block`, log 0), paid out to
/// `BENEFICIARY` on the origin network, as the Kreivo endpoint records it (F-007).
pub async fn redemption(pool: &PgPool, block: i64, reference: &[u8], amount: u128) -> i64 {
    redemption_by(pool, block, REDEEMER, reference, amount, BENEFICIARY).await
}

/// A redemption by `redeemer` with `reference`, paid out to `beneficiary`, at (`ISSUING`, `block`,
/// log 0).
pub async fn redemption_by(
    pool: &PgPool,
    block: i64,
    redeemer: &str,
    reference: &[u8],
    amount: u128,
    beneficiary: &str,
) -> i64 {
    observe(
        pool,
        ISSUING,
        block,
        0,
        ISSUING_ASSET,
        amount,
        Some(redeemer),
        None,
        Some(reference),
        true,
        json!({"event": "Redeemed", "backend": {"kind": "evm", "chain_id": 80002},
               "payout": {"address": beneficiary}}),
    )
    .await
}

/// The reserve the book holds for the asset on the origin network (F-004's book, posted here).
pub async fn reserve(pool: &PgPool, amount: u128) {
    exec(
        pool,
        &format!(
            "WITH e AS (INSERT INTO book_entries (reference) VALUES ('funding') RETURNING id) \
             INSERT INTO book_postings (entry_id, asset, account, network, side, amount) \
             SELECT id, '{ASSET}', 'reserve', '{ORIGIN}', 'debit', {amount} FROM e \
             UNION ALL SELECT id, '{ASSET}', 'surplus', NULL, 'credit', {amount} FROM e"
        ),
    )
    .await;
}

/// The latest order the core recorded for transfer `id`'s attempt `attempt`, with its payload.
pub async fn order(pool: &PgPool, id: i64, attempt: i32) -> Option<(i64, Value)> {
    sqlx::query_as(
        "SELECT id, payload FROM intents WHERE transfer_id = $1 AND attempt = $2 \
           AND payload ? 'order' ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .bind(attempt)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The endpoint prepares order `order` (F-007): its exact bytes, and a dry run that passed or was
/// refused with `reason`.
pub async fn prepare(pool: &PgPool, order: i64, refused: Option<&str>) -> i64 {
    let dry_run = match refused {
        None => json!({"outcome": "passed", "reason": null}),
        Some(r) => json!({"outcome": "refused", "reason": r}),
    };
    prepare_dry_run(pool, order, refused.is_none(), dry_run).await
}

/// The endpoint prepares order `order` with the dry run `dry_run`, which passed or not.
pub async fn prepare_dry_run(pool: &PgPool, order: i64, passed: bool, dry_run: Value) -> i64 {
    // The exact bytes the order's endpoint builds (the signer decodes them and compares them with
    // the order: audit must-fix 2): a payout through the vault's delivery role on the origin
    // network, a call of the on-chain component on the issuing network. An order the endpoint
    // could not build gets bytes that name it, which no signer signs.
    let (payload, message) = endpoint_bytes(pool, order).await;
    sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, nonce, \
             valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
             dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         SELECT $1, COALESCE((SELECT max(attempt) FROM prepared_transactions WHERE order_intent_id = $1), 0) + 1, \
             network, '\\x01', 'kreivo-mint', 0, 0, 0, '\\x02', '\\x03', $5, \
             $4, 1, '\\x05', $2, $3 \
         FROM intents WHERE id = $1 RETURNING id",
    )
    .bind(order)
    .bind(passed)
    .bind(dry_run)
    .bind(message)
    .bind(payload)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The payload and the message the endpoint of order `order` builds for it, at nonce `order`.
async fn endpoint_bytes(pool: &PgPool, order: i64) -> (Vec<u8>, Vec<u8>) {
    use bezent_signer::delivery::{self, EvmPayout, EvmTx, KreivoMessage, KreivoTx};
    let (network, purpose, reference, payload): (String, String, Option<Vec<u8>>, Value) =
        sqlx::query_as(
            "SELECT network, purpose, on_chain_reference, payload FROM intents WHERE id = $1",
        )
        .bind(order)
        .fetch_one(pool)
        .await
        .unwrap();
    let o = &payload["order"];
    let amount = payload["amount"]
        .as_str()
        .and_then(|a| a.parse::<u128>().ok());
    let reference: Option<[u8; 32]> = reference.and_then(|r| r.try_into().ok());
    let hex_n = |field: &str| -> Option<Vec<u8>> {
        o[field]
            .as_str()
            .and_then(|t| t.strip_prefix("0x"))
            .and_then(|t| hex::decode(t).ok())
    };
    let nonce = u64::try_from(order).unwrap();
    let built = match (network.as_str(), purpose.as_str(), amount, reference) {
        (ORIGIN, "payout", Some(amount), Some(reference)) => hex_n("to")
            .and_then(|to| <[u8; 20]>::try_from(to).ok())
            .map(|to| {
                let p = delivery::evm_signing_payload(&EvmTx {
                    chain_id: 80_002,
                    nonce,
                    priority_fee: 1,
                    max_fee: 2,
                    gas: 200_000,
                    to: contracts().roles,
                    value: 0,
                    data: delivery::evm_payout_call(
                        &contracts(),
                        &EvmPayout {
                            token: hex::decode(TOKEN.strip_prefix("0x").unwrap())
                                .unwrap()
                                .try_into()
                                .unwrap(),
                            to,
                            amount,
                            reference,
                        },
                    ),
                });
                (p.clone(), p)
            }),
        (ISSUING, "mint" | "refund-mint", Some(amount), Some(reference)) => {
            let message = if purpose == "mint" {
                let asset = o["asset"]
                    .as_str()
                    .and_then(|a| a.strip_prefix("Here("))
                    .and_then(|a| a.strip_suffix(')'))
                    .and_then(|a| a.parse::<u32>().ok());
                match (
                    asset,
                    hex_n("to").and_then(|t| <[u8; 32]>::try_from(t).ok()),
                ) {
                    (Some(asset), Some(to)) => Some(KreivoMessage::Mint {
                        asset,
                        to,
                        amount,
                        reference,
                    }),
                    _ => None,
                }
            } else {
                hex_n("redeemer")
                    .and_then(|r| <[u8; 32]>::try_from(r).ok())
                    .map(|redeemer| KreivoMessage::RefundMint {
                        redeemer,
                        reference,
                    })
            };
            message.map(|m| {
                let p = delivery::kreivo_signing_payload(&KreivoTx {
                    contract: CONTRACT,
                    value: 0,
                    ref_time: 1_000_000_000,
                    proof_size: 100_000,
                    storage_deposit_limit: Some(10_000_000_000),
                    data: m.data(),
                    era: [0x05, 0x00],
                    nonce: u32::try_from(nonce).unwrap(),
                    tip: 0,
                    spec_version: 134,
                    transaction_version: 13,
                    genesis: GENESIS,
                    birth_hash: [8; 32],
                });
                (p.clone(), delivery::kreivo_message_to_sign(&p))
            })
        }
        _ => None,
    };
    built.unwrap_or_else(|| {
        let named = format!("bytes of order {order}").into_bytes();
        let message = if network == ORIGIN {
            evm_payload(80_002, &named)
        } else {
            named
        };
        (message.clone(), message)
    })
}

/// `0x02 ‖ rlp([chain_id, 0, 1, 1, 21000, 0x11…11, 0, data, []])`, `data` under 56 bytes.
fn evm_payload(chain_id: u32, data: &[u8]) -> Vec<u8> {
    let chain = chain_id.to_be_bytes();
    let chain = &chain[chain.iter().position(|&b| b != 0).unwrap_or(3)..];
    let s = |b: &[u8]| [vec![0x80 + u8::try_from(b.len()).unwrap()], b.to_vec()].concat();
    let body = [
        s(chain),
        vec![0x80, 0x01, 0x01, 0x82, 0x52, 0x08],
        s(&[0x11; 20]),
        vec![0x80],
        s(data),
        vec![0xc0],
    ]
    .concat();
    let len = u8::try_from(body.len()).unwrap();
    let head = if len < 56 {
        vec![0xc0 + len]
    } else {
        vec![0xf8, len]
    };
    [vec![0x02], head, body].concat()
}

/// The delivery allowance key of [`TOKEN`], as F-008's `VaultRoles` names it:
/// `keccak256("bezent.delivery.allowance" ‖ token)`.
pub fn allowance_key() -> String {
    use sha3::Digest as _;
    let token = hex::decode(TOKEN.strip_prefix("0x").unwrap()).unwrap();
    let key = sha3::Keccak256::digest([b"bezent.delivery.allowance".as_slice(), &token].concat());
    format!("0x{}", hex::encode(key))
}

/// The EVM endpoint prepares payout order `order`, and its dry run is refused by the vault's
/// Roles modifier over `key`'s allowance: the structured refusal it records when both providers
/// returned the same `ConditionViolation(17, key)` (PR #52 review S-2), with the reason's words.
pub async fn prepare_allowance_spent(pool: &PgPool, order: i64, key: &str) -> i64 {
    let reason = format!("Roles: ConditionViolation(status 17, {key}) (execution reverted)");
    prepare_dry_run(
        pool,
        order,
        false,
        json!({"passed": false, "reason": reason, "violation": {"status": 17, "key": key}}),
    )
    .await
}

/// The signing intent the core recorded for transfer `id`'s attempt.
pub async fn signing_intent(pool: &PgPool, id: i64, attempt: i32) -> Option<(i64, Value)> {
    sqlx::query_as(
        "SELECT id, payload FROM intents WHERE transfer_id = $1 AND attempt = $2 \
           AND payload ? 'prepared' ORDER BY id DESC LIMIT 1",
    )
    .bind(id)
    .bind(attempt)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The endpoint asks the signer for the signing intent's signature and persists the signed bytes
/// (F-007), as the service role would.
pub async fn sign(world: &World, intent: i64) -> Signed {
    let signed = match world.signer.sign(intent).await {
        Outcome::Signed(s) => s,
        other => panic!("not signed: {other:?}"),
    };
    exec(
        &world.admin,
        &format!(
            "INSERT INTO signed_transactions (intent_id, attempt, bytes) VALUES ({intent}, 1, '\\xbeef')"
        ),
    )
    .await;
    signed
}

/// Runs the endpoint's half of attempt `attempt` of transfer `id` around the core's cycles at
/// `now`: the order is prepared, the signing intent signed, the bytes persisted.
pub async fn deliver(world: &World, id: i64, attempt: i32, now: DateTime<Utc>) {
    world.cycle(now).await;
    let (o, _) = order(&world.admin, id, attempt).await.expect("an order");
    prepare(&world.admin, o, None).await;
    world.cycle(now).await;
    let (s, _) = signing_intent(&world.admin, id, attempt)
        .await
        .expect("a signing intent");
    sign(world, s).await;
    world.cycle(now).await;
}

/// Proves the signed transaction of transfer `id`'s attempt `attempt` not executed at the final
/// head (`network`, `block`), as the endpoint would.
pub async fn prove(pool: &PgPool, id: i64, attempt: i32, network: &str, block: i64) {
    head(pool, network, block).await;
    sqlx::query(
        "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
             valid_from_block, valid_until_block, final_block_number, final_block_hash, \
             nonce_at_final, kind) \
         SELECT st.id, i.network, '\\x01', 0, 0, 0, $3, $4, 0, 'nonce-unconsumed' \
         FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         WHERE i.transfer_id = $1 AND i.attempt = $2",
    )
    .bind(id)
    .bind(attempt)
    .bind(block)
    .bind(hash(network, block))
    .execute(pool)
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------------------------
// Cancels (REQ-NX-4, issue #41): the EVM endpoint's half, as F-009 does it.

/// A cancel's EIP-1559 signing payload, as the EVM endpoint builds it: a transfer of nothing from
/// the payout key to itself at `nonce`, gas 21000, fees within the cap, on the origin network's
/// chain. The signer reads these bytes before it signs a cancel.
pub fn cancel_payload(nonce: u64) -> Vec<u8> {
    bezent_signer::delivery::evm_signing_payload(&bezent_signer::delivery::EvmTx {
        chain_id: 80_002,
        nonce,
        priority_fee: 1,
        max_fee: 1,
        gas: 21_000,
        to: payout_lane(),
        value: 0,
        data: Vec::new(),
    })
}

/// The nonce of the payout cancel order `order` names: the one a cancel must take (the signer
/// compares it with the order, audit must-fix 2).
pub async fn cancelled_nonce(pool: &PgPool, order: i64) -> u64 {
    let message: String = sqlx::query_scalar(
        "SELECT si.payload ->> 'message' FROM intents o \
         JOIN signed_transactions st ON st.id::text = o.payload -> 'order' ->> 'cancel' \
         JOIN intents si ON si.id = st.intent_id WHERE o.id = $1",
    )
    .bind(order)
    .fetch_one(pool)
    .await
    .unwrap();
    let bytes = hex::decode(message.strip_prefix("0x").unwrap()).unwrap();
    bezent_signer::delivery::decode_evm(&bytes)
        .expect("the cancelled payout's bytes")
        .nonce
}

/// The latest cancel order the core recorded for signed transaction `signed`, with its payload.
pub async fn cancel_order(pool: &PgPool, signed: i64) -> Option<(i64, Value)> {
    sqlx::query_as(
        "SELECT id, payload FROM intents WHERE purpose = 'cancel' AND payload ? 'order' \
           AND payload -> 'order' ->> 'cancel' = $1::text ORDER BY id DESC LIMIT 1",
    )
    .bind(signed)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The EVM endpoint prepares cancel order `order` at the nonce of the payout it names: its exact
/// bytes, and a dry run that passed or failed with `refused`.
pub async fn prepare_cancel(pool: &PgPool, order: i64, refused: Option<&str>) -> i64 {
    let nonce = cancelled_nonce(pool, order).await;
    let dry_run = json!({"passed": refused.is_none(), "reason": refused});
    sqlx::query_scalar(
        "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, nonce, \
             valid_from_block, valid_until_block, checkpoint_hash, call, signing_payload, message, \
             dry_run_block_number, dry_run_block_hash, dry_run_passed, dry_run) \
         SELECT $1, 1, network, $2, 'polygon-payout', $3, 1, 9223372036854775807, '\\x02', $2, $4, \
             $4, 1, '\\x05', $5, $6 \
         FROM intents WHERE id = $1 RETURNING id",
    )
    .bind(order)
    .bind(payout_lane().to_vec())
    .bind(i64::try_from(nonce).unwrap())
    .bind(cancel_payload(nonce))
    .bind(refused.is_none())
    .bind(dry_run)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The signing intent the core recorded for the cancel of signed transaction `signed`.
pub async fn cancel_signing_intent(pool: &PgPool, signed: i64) -> Option<(i64, Value)> {
    sqlx::query_as(
        "SELECT si.id, si.payload FROM intents si \
         JOIN prepared_transactions p ON p.id::text = si.payload ->> 'prepared' \
         JOIN intents o ON o.id = p.order_intent_id \
         WHERE si.purpose = 'cancel' AND o.payload -> 'order' ->> 'cancel' = $1::text \
         ORDER BY si.id DESC LIMIT 1",
    )
    .bind(signed)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The signed transaction of transfer `id`'s attempt `attempt`, as the endpoint persisted it.
pub async fn signed_of(pool: &PgPool, id: i64, attempt: i32) -> i64 {
    scalar(
        pool,
        &format!(
            "SELECT st.id FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
             WHERE i.transfer_id = {id} AND i.attempt = {attempt}"
        ),
    )
    .await
}

/// The EVM endpoint's proof that the signed transaction of transfer `id`'s attempt `attempt` was
/// replaced: its nonce consumed at `at` by another transaction (the cancel), final at `block`.
pub async fn prove_replaced(pool: &PgPool, id: i64, attempt: i32, at: i64, block: i64) {
    head(pool, ORIGIN, block).await;
    sqlx::query(
        "INSERT INTO non_execution_proofs (signed_transaction_id, network, signer, nonce, \
             valid_from_block, valid_until_block, final_block_number, final_block_hash, \
             nonce_at_final, kind, at_block_number, at_block_hash) \
         SELECT st.id, i.network, p.lane, p.nonce, p.valid_from_block, p.valid_until_block, $3, \
             $4, p.nonce + 1, 'nonce-consumed-by-other', $5, $6 \
         FROM signed_transactions st JOIN intents i ON i.id = st.intent_id \
         JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared' \
         WHERE i.transfer_id = $1 AND i.attempt = $2",
    )
    .bind(id)
    .bind(attempt)
    .bind(block)
    .bind(hash(ORIGIN, block))
    .bind(at)
    .bind(hash(ORIGIN, at))
    .execute(pool)
    .await
    .unwrap();
}

/// The reference a withdrawal's payout writes on its origin network (review M1).
pub async fn payout_reference_of(pool: &PgPool, id: i64) -> Vec<u8> {
    scalar(
        pool,
        &format!("SELECT destination_reference FROM transfers WHERE id = {id}"),
    )
    .await
}

/// Operator `by` (an index into `World::ops`) approves manual action `action`, as their own login.
pub async fn approve_as(w: &World, by: usize, action: i64) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO manual_approvals (action_id, justification) VALUES ($1, 'checked')")
        .bind(action)
        .execute(&w.ops[by])
        .await
        .map(|_| ())
}

/// The transfer's on-chain reference.
pub async fn reference_of(pool: &PgPool, id: i64) -> Vec<u8> {
    scalar(
        pool,
        &format!("SELECT on_chain_reference FROM transfers WHERE id = {id}"),
    )
    .await
}

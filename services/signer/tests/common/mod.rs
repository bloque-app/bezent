// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared by the signer's tests. Every test runs in a fresh database that `#[sqlx::test]` creates
//! on the server `DATABASE_URL` names, with bezent's migrations applied. Test keys are derived
//! here from fixed labels and tagged `"environment": "test"`; no key is read from anywhere else.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bezent_signer::config::{Config, Policy};
use bezent_signer::keys::FileBackend;
use bezent_signer::log::{Captured, Log};
use bezent_signer::signer::Signer;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The on-chain component's account on the issuing test network.
pub const CONTRACT: &str = "0xc0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";
/// The issuing test network's genesis hash.
pub const GENESIS: [u8; 32] = [0x9e; 32];
/// The vault's contracts on the EVM test network.
pub const ROLES: [u8; 20] = [0x22; 20];
/// MultiSendCallOnly.
pub const MULTISEND: [u8; 20] = [0x23; 20];
/// The reference emitter.
pub const EMITTER: [u8; 20] = [0x24; 20];
/// The asset the fixture's orders move, and its admitted token on the EVM test network.
pub const ASSET: &str = "usd";
/// The asset's issuing identity.
pub const ISSUING_ASSET: &str = "Here(101)";
/// The admitted token.
pub const TOKEN_ADDRESS: [u8; 20] = [0x44; 20];
/// A payout's beneficiary.
pub const BENEFICIARY: [u8; 20] = [0x55; 20];
/// A mint's recipient.
pub const RECIPIENT: [u8; 32] = [0x02; 32];
/// A refund mint's redeemer.
pub const REDEEMER: [u8; 32] = [0x03; 32];

/// The vault's contracts, as the fixture's policy declares them.
pub fn contracts() -> bezent_signer::delivery::EvmContracts {
    bezent_signer::delivery::EvmContracts {
        roles: ROLES,
        multisend: MULTISEND,
        emitter: EMITTER,
        role_key: bezent_signer::delivery::role_key("bezent.delivery").unwrap(),
    }
}

/// A delivery as the core orders it and an endpoint prepares it: the order's body, the full
/// payload the endpoint builds, and the message the key signs.
pub struct Delivery {
    /// The order's `order` object.
    pub order: serde_json::Value,
    /// The prepared payload.
    pub payload: Vec<u8>,
    /// The message the key signs (the payload, or its hash on the issuing network past 256
    /// bytes).
    pub message: Vec<u8>,
}

/// A fresh nonce for each delivery the fixture builds, so no two share a lane position.
pub fn next_nonce() -> u64 {
    static N: AtomicU32 = AtomicU32::new(1);
    u64::from(N.fetch_add(1, Ordering::Relaxed))
}

/// A 32-byte on-chain reference: `reference` padded with zeros, or a fresh one.
pub fn reference32(reference: Option<&[u8]>) -> [u8; 32] {
    let mut r = [0u8; 32];
    match reference {
        Some(given) => r[..given.len().min(32)].copy_from_slice(&given[..given.len().min(32)]),
        None => {
            r[..8].copy_from_slice(&next_nonce().to_be_bytes());
            r[31] = 0xfe;
        }
    }
    r
}

/// The EVM payout of `amount` of the fixture's token to [`BENEFICIARY`] under `reference`, at
/// `nonce`, on chain 137, through the fixture's vault.
pub fn evm_payout(amount: u128, reference: [u8; 32], nonce: u64) -> Vec<u8> {
    use bezent_signer::delivery::{self, EvmPayout, EvmTx};
    delivery::evm_signing_payload(&EvmTx {
        chain_id: 137,
        nonce,
        priority_fee: 1,
        max_fee: 2,
        gas: 200_000,
        to: ROLES,
        value: 0,
        data: delivery::evm_payout_call(
            &contracts(),
            &EvmPayout {
                token: TOKEN_ADDRESS,
                to: BENEFICIARY,
                amount,
                reference,
            },
        ),
    })
}

/// A Kreivo payload calling the fixture's contract with `data`, at `nonce`.
pub fn kreivo_payload(data: Vec<u8>, nonce: u32) -> Vec<u8> {
    use bezent_signer::delivery::{self, KreivoTx};
    delivery::kreivo_signing_payload(&KreivoTx {
        contract: hex::decode(CONTRACT.strip_prefix("0x").unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
        value: 0,
        ref_time: 1_000_000_000,
        proof_size: 100_000,
        storage_deposit_limit: Some(10_000_000_000),
        data,
        era: [0x05, 0x00],
        nonce,
        tip: 0,
        spec_version: 134,
        transaction_version: 13,
        genesis: GENESIS,
        birth_hash: [8; 32],
    })
}

/// The delivery the core orders and an endpoint prepares for `purpose` on `network`, moving
/// `amount` under `reference` at `nonce`: a payout on `polygon-test`, a mint or refund mint on
/// `kreivo-test`, each the exact bytes its endpoint builds. Any other pair gets an empty order and
/// bytes no signer decodes (an EIP-1559 transaction of nothing on `polygon-test`).
pub fn delivery(
    network: &str,
    purpose: &str,
    reference: [u8; 32],
    amount: &str,
    nonce: u64,
) -> Delivery {
    use bezent_signer::delivery::{self, KreivoMessage};
    let units = amount.parse::<u128>().unwrap_or(0);
    let hex0x = |b: &[u8]| format!("0x{}", hex::encode(b));
    let kreivo = |data: Vec<u8>, order: serde_json::Value| {
        let payload = kreivo_payload(data, u32::try_from(nonce).unwrap());
        Delivery {
            order,
            message: delivery::kreivo_message_to_sign(&payload),
            payload,
        }
    };
    match (network, purpose) {
        ("polygon-test", "payout") => {
            let payload = evm_payout(units, reference, nonce);
            Delivery {
                order: serde_json::json!({"to": hex0x(&BENEFICIARY), "asset": ASSET}),
                message: payload.clone(),
                payload,
            }
        }
        ("kreivo-test", "mint") => kreivo(
            KreivoMessage::Mint {
                asset: 101,
                to: RECIPIENT,
                amount: units,
                reference,
            }
            .data(),
            serde_json::json!({"asset": ISSUING_ASSET, "to": hex0x(&RECIPIENT)}),
        ),
        ("kreivo-test", "refund-mint") => kreivo(
            KreivoMessage::RefundMint {
                redeemer: REDEEMER,
                reference,
            }
            .data(),
            serde_json::json!({"redeemer": hex0x(&REDEEMER), "asset": ISSUING_ASSET}),
        ),
        _ => {
            let payload = if network == "polygon-test" {
                evm_payload(137)
            } else {
                vec![1, 2, 3, 4, 5]
            };
            Delivery {
                order: serde_json::json!({}),
                message: payload.clone(),
                payload,
            }
        }
    }
}

/// The keys every fixture holds: id, scheme.
pub const KEYS: &[(&str, &str)] = &[
    ("kreivo-mint", "sr25519"),
    ("kreivo-destroy", "ecdsa"),
    ("polygon-payout", "secp256k1-evm"),
];

/// The 32-byte seed of test key `id`.
pub fn seed(id: &str) -> [u8; 32] {
    Sha256::digest(format!("bezent signer test key {id}")).into()
}

/// A fresh directory under the system's temporary directory.
pub fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bzs-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes the test key file into `dir` and returns its path.
pub fn write_keys(dir: &Path) -> PathBuf {
    let keys: Vec<_> = KEYS
        .iter()
        .map(|(id, scheme)| {
            serde_json::json!({"id": id, "scheme": scheme, "seed": format!("0x{}", hex::encode(seed(id)))})
        })
        .collect();
    let path = dir.join("test-keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "comment": "TEST KEYS, generated by the signer's tests",
            "environment": "test",
            "keys": keys,
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

/// The configuration every fixture uses: caps of 1000 minor units per hour.
pub fn config_json(dir: &Path, uid: u32, token: &str) -> serde_json::Value {
    serde_json::json!({
        "socket": dir.join("s.sock"),
        "callers": [{
            "name": "core",
            "uid": uid,
            "token_sha256": [hex::encode(Sha256::digest(token.as_bytes()))],
        }],
        "networks": [
            {"id": "kreivo-test", "environment": "test", "issuing": true, "contract_account": CONTRACT,
             "genesis": format!("0x{}", hex::encode(GENESIS))},
            {"id": "polygon-test", "environment": "test", "chain_id": 137,
             "cancel_max_fee_per_gas": "1000",
             "delivery": {"roles": format!("0x{}", hex::encode(ROLES)),
                          "multisend": format!("0x{}", hex::encode(MULTISEND)),
                          "emitter": format!("0x{}", hex::encode(EMITTER)),
                          "role": "bezent.delivery"}},
            {"id": "kreivo", "environment": "production", "issuing": true, "contract_account": "0xc1c1"},
        ],
        "keys": {"backend": "file", "path": dir.join("test-keys.json")},
        "bindings": [
            {"key": "kreivo-mint", "network": "kreivo-test", "purposes": ["mint", "refund-mint"],
             "window_cap": "1000", "window_seconds": 3600},
            {"key": "kreivo-destroy", "network": "kreivo-test", "purposes": ["destroy"],
             "window_cap": "1000", "window_seconds": 3600},
            {"key": "polygon-payout", "network": "polygon-test", "purposes": ["payout", "cancel"],
             "window_cap": "1000", "window_seconds": 3600},
        ],
    })
}

/// An EIP-1559 signing payload on chain `chain_id` (at most 2 bytes of it): `0x02 ‖
/// rlp([chain_id, 0, 1, 1, 21000, 0x11…11, 0, "", []])`.
pub fn evm_payload(chain_id: u16) -> Vec<u8> {
    let chain = match chain_id {
        0 => vec![0x80],
        1..=0x7f => vec![u8::try_from(chain_id).unwrap()],
        0x80..=0xff => vec![0x81, u8::try_from(chain_id).unwrap()],
        _ => [vec![0x82], chain_id.to_be_bytes().to_vec()].concat(),
    };
    let body = [
        chain,
        vec![0x80, 0x01, 0x01, 0x82, 0x52, 0x08, 0x94],
        vec![0x11; 20],
        vec![0x80, 0x80, 0xc0],
    ]
    .concat();
    [vec![0x02, 0xc0 + u8::try_from(body.len()).unwrap()], body].concat()
}

/// The signer's login in these tests: a member of `bezent_signer` only (#46).
pub const SIGNER_LOGIN: &str = "signer_tests_signer_login";
/// The core's login in these tests: a member of `bezent_core` only (#46).
pub const CORE_LOGIN: &str = "signer_tests_core_login";
/// The test-only password of the logins above.
const LOGIN_PASSWORD: &str = "test-only-signer-tests-login";

/// A pool on `connect`'s database logged in as `name`, made a member of `role` and of nothing
/// else: one pool, one login, never a switched role (#27, #46).
pub async fn login_pool(connect: &PgConnectOptions, name: &str, role: &str, max: u32) -> PgPool {
    let owner = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(connect.clone())
        .await
        .unwrap();
    let pool = bezent_store::login::connect_login(
        &owner,
        name,
        role,
        LOGIN_PASSWORD,
        PgPoolOptions::new().max_connections(max),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    owner.close().await;
    pool
}

/// The token the fixtures' core presents.
pub const TOKEN: &str = "core-token-for-tests";

/// The current process's user id (no `unsafe`: the kernel reports it for a socket pair).
pub fn my_uid() -> u32 {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    tokio::net::UnixStream::from_std({
        a.set_nonblocking(true).unwrap();
        a
    })
    .unwrap()
    .peer_cred()
    .unwrap()
    .uid()
}

/// A signer over a test database, with the fixture's keys and configuration.
pub struct Fixture {
    /// The migration owner's pool: the tests record intents through it, as the core would.
    pub admin: PgPool,
    /// How to connect again (a restart).
    pub connect: PgConnectOptions,
    /// The signer.
    pub signer: Arc<Signer<FileBackend>>,
    /// The signer's log.
    pub log: Captured,
    /// The fixture's directory.
    pub dir: PathBuf,
    /// The configuration.
    pub config: Config,
    /// The resolved policy.
    pub policy: Policy,
}

impl Fixture {
    /// Builds the fixture.
    pub async fn new(pool: PgPoolOptions, connect: PgConnectOptions) -> Self {
        let admin = pool.connect_with(connect.clone()).await.unwrap();
        // The declarations the signer reads: which token the fixture's asset is paid in.
        sqlx::raw_sql(&format!(
            "INSERT INTO networks (network, role, address_format) VALUES \
               ('polygon-test', 'origin', 'evm'), ('kreivo-test', 'issuing', 'ss58'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
               ('{ASSET}', 6, 'public', '{ISSUING_ASSET}'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('polygon-test', '0x{}', '{ASSET}', 6)",
            hex::encode(TOKEN_ADDRESS)
        ))
        .execute(&admin)
        .await
        .expect("declare the fixture's networks and asset");
        let dir = temp_dir();
        write_keys(&dir);
        let config_path = dir.join("signer.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&config_json(&dir, my_uid(), TOKEN)).unwrap(),
        )
        .unwrap();
        let config = Config::load(&config_path).unwrap();
        let (signer, log, policy) = Self::start(&config, &connect).await;
        Self {
            admin,
            connect,
            signer,
            log,
            dir,
            config,
            policy,
        }
    }

    /// Starts a signer process's state from nothing but the configuration and the store.
    pub async fn start(
        config: &Config,
        connect: &PgConnectOptions,
    ) -> (Arc<Signer<FileBackend>>, Captured, Policy) {
        let bezent_signer::config::KeySource::File { path } = &config.keys else {
            panic!("the tests use the file backend");
        };
        let backend = FileBackend::load(path).unwrap();
        let policy = config.policy(&backend).unwrap();
        let pool = login_pool(connect, SIGNER_LOGIN, bezent_store::SIGNER_ROLE, 4).await;
        let (log, captured) = Log::capture();
        (
            Arc::new(Signer::new(pool, policy.clone(), backend, log)),
            captured,
            policy,
        )
    }

    /// The test key backend, loaded again.
    pub fn backend(&self) -> FileBackend {
        FileBackend::load(&self.dir.join("test-keys.json")).unwrap()
    }

    /// Records an intent as the core would, and returns its id: the order and the exact bytes its
    /// endpoint builds ([`delivery`]: a payout on `polygon-test`, a mint or refund mint on
    /// `kreivo-test`), under `reference` padded to 32 bytes, or a fresh one.
    pub async fn intent(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        amount: &str,
        from: Option<&str>,
    ) -> i64 {
        let reference = reference32(reference);
        let d = delivery(network, purpose, reference, amount, next_nonce());
        self.deliver(network, purpose, Some(&reference), 1, &d, amount, from)
            .await
    }

    /// Records, as the core and an endpoint would (PLAN DEC-3), the delivery order `d.order` on
    /// `network` (`purpose`, `reference`, attempt `attempt`, `amount`), its prepared transaction
    /// over `d.payload` (whose dry run passed), and the signing intent naming it with the exact
    /// bytes `d.message` and that amount (0037, PR #39 third review MF-A). Returns the signing
    /// intent's id.
    #[allow(clippy::too_many_arguments)]
    pub async fn deliver(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        attempt: i32,
        d: &Delivery,
        amount: &str,
        from: Option<&str>,
    ) -> i64 {
        let prepared = self
            .prepare_with(network, purpose, reference, attempt, d, amount)
            .await;
        let mut payload = serde_json::json!({
            "message": format!("0x{}", hex::encode(&d.message)),
            "amount": amount,
            "prepared": prepared,
        });
        if let Some(from) = from {
            payload["from"] = from.into();
        }
        sqlx::query_scalar(
            "INSERT INTO intents (network, purpose, on_chain_reference, attempt, payload) \
             VALUES ($1, $2, $3, $4, $5::text::jsonb) RETURNING id",
        )
        .bind(network)
        .bind(purpose)
        .bind(reference)
        .bind(attempt)
        .bind(payload.to_string())
        .fetch_one(&self.admin)
        .await
        .expect("record intent")
    }

    /// As [`Fixture::deliver`], over raw bytes `message` and an empty order: bytes that answer no
    /// order, for the checks that come before the signer decodes anything.
    #[allow(clippy::too_many_arguments)]
    pub async fn signing_intent(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        attempt: i32,
        message: &[u8],
        amount: &str,
        from: Option<&str>,
    ) -> i64 {
        let d = Delivery {
            order: serde_json::json!({}),
            payload: message.to_vec(),
            message: message.to_vec(),
        };
        self.deliver(network, purpose, reference, attempt, &d, amount, from)
            .await
    }

    /// Records an empty delivery order and its prepared transaction over `message`, as the core
    /// and an endpoint would; returns the prepared transaction's id.
    pub async fn prepare(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        attempt: i32,
        message: &[u8],
        amount: &str,
    ) -> i64 {
        let d = Delivery {
            order: serde_json::json!({}),
            payload: message.to_vec(),
            message: message.to_vec(),
        };
        self.prepare_with(network, purpose, reference, attempt, &d, amount)
            .await
    }

    /// Records the delivery order `d.order` and its prepared transaction over `d.payload` and
    /// `d.message`; returns the prepared transaction's id.
    pub async fn prepare_with(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        attempt: i32,
        d: &Delivery,
        amount: &str,
    ) -> i64 {
        let order: i64 = sqlx::query_scalar(
            "INSERT INTO intents (network, purpose, on_chain_reference, attempt, payload) \
             VALUES ($1, $2, $3, $4, $5::text::jsonb) RETURNING id",
        )
        .bind(network)
        .bind(purpose)
        .bind(reference)
        .bind(attempt)
        .bind(serde_json::json!({"order": d.order, "amount": amount}).to_string())
        .fetch_one(&self.admin)
        .await
        .expect("record the order");
        sqlx::query_scalar(
            "INSERT INTO prepared_transactions (order_intent_id, attempt, network, lane, key_id, \
                 nonce, valid_from_block, valid_until_block, checkpoint_hash, call, \
                 signing_payload, message, dry_run_block_number, dry_run_block_hash, \
                 dry_run_passed, dry_run) \
             VALUES ($1, 1, $2, '\\x01', 'k', 0, 0, 5, '\\x02', '\\x03', $3, $4, 1, '\\x05', true, \
                 '{\"outcome\": \"passed\"}') \
             RETURNING id",
        )
        .bind(order)
        .bind(network)
        .bind(&d.payload)
        .bind(&d.message)
        .fetch_one(&self.admin)
        .await
        .expect("prepare the order")
    }

    /// Records `payload` as an intent with 0037's store check off, as a compromised core would
    /// write past it: how a test reaches the signer's own check behind the store's.
    pub async fn intent_past_the_store(
        &self,
        network: &str,
        purpose: &str,
        reference: Option<&[u8]>,
        payload: &serde_json::Value,
    ) -> i64 {
        let mut tx = self.admin.begin().await.unwrap();
        sqlx::raw_sql("ALTER TABLE intents DISABLE TRIGGER intents_guard_prepared")
            .execute(&mut *tx)
            .await
            .unwrap();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO intents (network, purpose, on_chain_reference, payload) \
             VALUES ($1, $2, $3, $4::text::jsonb) RETURNING id",
        )
        .bind(network)
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

    /// How many signatures are recorded.
    pub async fn signatures(&self) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM signatures")
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    /// A pool logged in as the core's login: a member of `bezent_core` and of nothing else. Only the
    /// core and the endpoints may append compromise signals (migration 0020, F-007 review S-a).
    pub async fn core_pool(&self) -> PgPool {
        login_pool(&self.connect, CORE_LOGIN, bezent_store::CORE_ROLE, 2).await
    }
}

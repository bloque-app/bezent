// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! F-009's fork tests: the endpoint against an anvil fork of Polygon at the block `e2e/evm/forks.env`
//! pins, with the vault of `F-008` deployed by its own script (`contracts/evm/script/DeployVault.s.sol`)
//! and Foundry's test keys only. Nothing is sent to a real network: anvil reads the pinned state
//! from the upstream archive RPC and keeps every transaction local.
//!
//! They run only when `BEZENT_EVM_FORK_URL` names that upstream (an archive RPC of Polygon PoS);
//! otherwise each test says so and passes. They need `anvil` and `forge` on the `PATH` and
//! `DATABASE_URL`, like every store test. Each test starts its own anvil and stops it when it ends,
//! pass or fail.
//!
//! anvil's `finalized` tag is 64 blocks below its latest, so "final" is reached by mining 64 blocks.

mod common;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::rpc::client::{ClientBuilder, RpcClient};
use alloy::sol_types::SolCall;
use bezent_evm_endpoint::evm::{self, Contracts};
use bezent_evm_endpoint::network::{self, Declarations, Identity, Token};
use bezent_evm_endpoint::observe::{Follower, Step, Watch};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::proof::{Outcome, Prover};
use bezent_evm_endpoint::rpc::{Http, Rpc};
use bezent_evm_endpoint::submit::{GasBounds, Lane, Prepared, SignerLink, Submitted, Submitter};
use common::{ASSET, NETWORK, role_pool, start_signer, start_signer_for, usdc};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// The pinned fork (`e2e/evm/forks.env`).
const FORK_BLOCK: u64 = 95_400_000;
/// Its hash, read from two public providers and from the fork (the endpoint's fixture pins it).
const FORK_HASH: &str = "0xcc89572742615188f853cc9e74e7f5279a369fbb4a019afc8027ec974b7fedd3";

/// Foundry's default accounts and keys (anvil's), all test keys.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const OWNERS: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8,0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC,0x90F79bf6EB2c4f870365E785982E1f101E93b906";
const CONVERSION_MEMBER: &str = "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc";
const DEPOSITOR: &str = "0x976EA74026E726554dB657fA54763abd0C3a0aa9";
/// Native USDC's balances live at slot 9 (FiatTokenV2_2 `balanceAndBlacklistStates`).
const USDC_BALANCE_SLOT: u64 = 9;

fn upstream() -> Option<String> {
    let url = std::env::var("BEZENT_EVM_FORK_URL")
        .ok()
        .filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!("BEZENT_EVM_FORK_URL is not set: the fork test is skipped");
    }
    url
}

/// An anvil fork, stopped on drop.
struct Anvil {
    child: std::sync::Mutex<Child>,
    port: u16,
    rpc: RpcClient,
    log: PathBuf,
    upstream: String,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Anvil {
    async fn start(upstream: &str) -> Anvil {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // anvil's own output, for the panic message of a call that fails. anvil prints the
        // upstream URL in its banner, and the URL may carry a key: it is redacted before printing.
        let log = common::temp_dir().join("anvil.log");
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new("anvil")
            .args([
                "--fork-url",
                upstream,
                "--fork-block-number",
                &FORK_BLOCK.to_string(),
                "--port",
                &port.to_string(),
            ])
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .expect("anvil is on the PATH");
        let rpc =
            ClientBuilder::default().http(format!("http://127.0.0.1:{port}").parse().unwrap());
        let anvil = Anvil {
            child: std::sync::Mutex::new(child),
            port,
            rpc,
            log,
            upstream: upstream.to_owned(),
        };
        for _ in 0..120 {
            if anvil.call("eth_blockNumber", json!([])).await.is_ok() {
                return anvil;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("anvil did not answer");
    }

    fn url(&self, host: &str) -> String {
        format!("http://{host}:{}", self.port)
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, String> {
        self.rpc
            .request::<Value, Value>(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    /// A call that must succeed. A transport error is tried again twice (a pooled connection
    /// the node closed); then the panic says whether anvil is still running, and its last lines.
    async fn rpc(&self, method: &'static str, params: Value) -> Value {
        let mut last = String::new();
        for _ in 0..3 {
            match self.call(method, params.clone()).await {
                Ok(v) => return v,
                Err(e) => last = e,
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        let status = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_wait()
            .ok()
            .flatten();
        let log = std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .replace(&self.upstream, "<upstream>");
        let tail: Vec<&str> = log
            .lines()
            .filter(|l| !l.contains("Endpoint"))
            .rev()
            .take(30)
            .collect();
        panic!(
            "{method}: {last}; anvil exited: {status:?}; its last lines:\n{}",
            tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        )
    }

    /// Two providers over this anvil, by two names: one node, so independence is not what these
    /// tests show (the mock tests do); the endpoint's reads are.
    fn pair(&self) -> Pair<Http> {
        Pair {
            a: Http::new("a", &self.url("127.0.0.1")).unwrap(),
            b: Http::new("b", &self.url("localhost")).unwrap(),
        }
    }

    /// Mines `n` blocks.
    async fn mine(&self, n: u64) {
        self.rpc("anvil_mine", json!([format!("0x{n:x}")])).await;
    }

    /// Mines past finality: 64 blocks and one.
    async fn finalize(&self) {
        self.mine(65).await;
    }

    /// Sets `holder`'s USDC balance.
    async fn fund_usdc(&self, holder: Address, amount: u64) {
        let slot = keccak256(
            [
                holder.into_word().as_slice(),
                U256::from(USDC_BALANCE_SLOT).to_be_bytes::<32>().as_slice(),
            ]
            .concat(),
        );
        self.rpc(
            "anvil_setStorageAt",
            json!([usdc(), slot, B256::from(U256::from(amount))]),
        )
        .await;
        assert_eq!(self.usdc_balance(holder).await, U256::from(amount));
    }

    async fn usdc_balance(&self, holder: Address) -> U256 {
        let data = evm::balanceOfCall { owner: holder }.abi_encode();
        let out = self
            .rpc(
                "eth_call",
                json!([{"to": usdc(), "data": format!("0x{}", hex::encode(data))}, "latest"]),
            )
            .await;
        U256::from_str_radix(out.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }

    /// Sends a transaction from one of anvil's unlocked accounts.
    async fn send(&self, tx: Value) -> B256 {
        let h = self.rpc("eth_sendTransaction", json!([tx])).await;
        h.as_str().unwrap().parse().unwrap()
    }
}

/// The vault `F-008`'s script deploys: Safe, Roles, emitter.
struct Vault {
    safe: Address,
    roles: Address,
    emitter: Address,
}

fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm")
}

/// Deploys the vault with `contracts/evm/script/DeployVault.s.sol`, the delivery member being the
/// lane's address, two of three owners, the deployer none of them (F-008 NOTES, "Deployment").
fn deploy(anvil: &Anvil, lane: Address) -> Vault {
    let out = Command::new("forge")
        .current_dir(contracts_dir())
        .args([
            "script",
            "script/DeployVault.s.sol",
            "--rpc-url",
            &anvil.url("127.0.0.1"),
            "--broadcast",
            "--private-key",
            DEPLOYER_KEY,
            "--json",
        ])
        .env("NETWORK", "polygon")
        .env("VAULT_OWNERS", OWNERS)
        .env("VAULT_THRESHOLD", "2")
        .env("DELIVERY_MEMBER", lane.to_string())
        .env("CONVERSION_MEMBER", CONVERSION_MEMBER)
        .output()
        .expect("forge is on the PATH");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "DeployVault failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value = stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v["returns"]["d"]["value"].as_str().map(str::to_owned))
        .expect("the deployment's addresses");
    let addresses: Vec<Address> = value
        .trim_matches(|c| c == '(' || c == ')')
        .split(", ")
        .map(|a| a.parse().unwrap())
        .collect();
    Vault {
        safe: addresses[0],
        roles: addresses[1],
        emitter: addresses[2],
    }
}

/// The endpoint over the fork, the store and the in-process signer.
struct World {
    anvil: Anvil,
    pair: Pair<Http>,
    admin: PgPool,
    endpoint: PgPool,
    core: PgPool,
    lane: Lane,
    link: SignerLink,
    vault: Vault,
    declarations: Declarations,
    convertibles: Vec<(Address, Address)>,
}

impl World {
    async fn new(upstream: &str, pool: PgPoolOptions, connect: PgConnectOptions) -> World {
        let anvil = Anvil::start(upstream).await;
        let (_, lane) = start_signer(&connect).await;
        // Foundry's account 4 carries an EIP-7702 delegation on Polygon at the pinned block (anyone
        // can sign one with a published key). bezent's delivery key has none: it is cleared here,
        // so a cancel, a transfer of nothing to the key itself, runs no code.
        anvil
            .rpc("anvil_setCode", json!([lane.address, "0x"]))
            .await;
        let vault = deploy(&anvil, lane.address);
        // The signer that signs here declares this vault's contracts, deployed only now: a payout's
        // bytes must name them (audit must-fix 2). The first signer only told the lane.
        let (link, lane) = start_signer_for(
            &connect,
            Contracts {
                roles: vault.roles,
                multisend: common::contracts().multisend,
                emitter: vault.emitter,
                delivery_role: evm::role_key("bezent.delivery").unwrap(),
            },
        )
        .await;
        let admin = pool.connect_with(connect.clone()).await.unwrap();
        let vault_text = format!("{:#x}", vault.safe);
        common::declare_vault(&admin, &vault_text).await;
        let declarations = network::declarations(&admin, NETWORK, vault.safe)
            .await
            .unwrap();
        assert_eq!(
            declarations.tokens.get(&usdc()),
            Some(&Token {
                text: usdc().to_string(),
                asset: ASSET.into()
            })
        );
        World {
            pair: anvil.pair(),
            anvil,
            endpoint: role_pool(&connect, "bezent_evm_endpoint").await,
            core: role_pool(&connect, "bezent_core").await,
            admin,
            lane,
            link,
            vault,
            declarations,
            convertibles: vec![(common::usdt0(), common::pool_address())],
        }
    }

    fn contracts(&self) -> Contracts {
        Contracts {
            roles: self.vault.roles,
            multisend: common::contracts().multisend,
            emitter: self.vault.emitter,
            delivery_role: evm::role_key("bezent.delivery").unwrap(),
        }
    }

    fn follower(&self) -> Follower<'_, Http> {
        Follower {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            watch: Watch {
                vault: self.vault.safe,
                emitter: self.vault.emitter,
                declarations: &self.declarations,
                convertibles: &self.convertibles,
            },
            start_block: FORK_BLOCK + 1,
            max_range: 500,
        }
    }

    fn submitter(&self) -> Submitter<'_, Http> {
        Submitter {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            chain_id: common::CHAIN_ID,
            contracts: self.contracts(),
            vault: self.vault.safe,
            declarations: &self.declarations,
            lane: &self.lane,
            signer: &self.link,
            gas: GasBounds {
                max_gas: 500_000,
                max_fee: 3_000_000_000_000,
                margin_percent: 130,
            },
        }
    }

    fn prover(&self) -> Prover<'_, Http> {
        Prover {
            pair: &self.pair,
            pool: &self.endpoint,
            network: NETWORK,
            lane: &self.lane,
            contracts: self.contracts(),
        }
    }

    async fn follow(&self) -> Step {
        loop {
            match self.follower().step().await.unwrap() {
                Step::Read { .. } => continue,
                other => return other,
            }
        }
    }

    async fn intent(&self, purpose: &str, reference: Option<&[u8]>, payload: Value) -> i64 {
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
        // an order is recorded as the migration owner, as a drill would.
        .fetch_one(
            if payload.get("order").is_some() && payload.get("prepared").is_none() {
                &self.admin
            } else {
                &self.core
            },
        )
        .await
        .unwrap()
    }

    /// The core's order, the endpoint's preparation, the core's signing intent, the signature and
    /// the submission. Returns the signed transaction's id.
    async fn pay(&self, purpose: &str, reference: Option<B256>, order: Value, amount: u128) -> i64 {
        let r = reference.map(|r| r.to_vec());
        let order_id = self
            .intent(
                purpose,
                r.as_deref(),
                json!({"order": order, "amount": amount.to_string()}),
            )
            .await;
        let prepared = self.submitter().prepare(order_id).await.unwrap();
        let Prepared::Ready { id, message, .. } = &prepared else {
            panic!("not ready: {prepared:?}");
        };
        let intent = self
            .intent(
                purpose,
                r.as_deref(),
                bezent_evm_endpoint::submit::signing_intent_payload(*id, message, amount),
            )
            .await;
        match self.submitter().sign_and_submit(intent).await.unwrap() {
            Submitted::Submitted { id, .. } => id,
            other => panic!("not submitted: {other:?}"),
        }
    }

    async fn payout(&self, reference: B256, to: Address, amount: u128) -> i64 {
        self.pay(
            "payout",
            Some(reference),
            json!({"to": to.to_string(), "asset": ASSET, "token": usdc().to_string()}),
            amount,
        )
        .await
    }

    /// Follows to the final point, then proves what it can: every live signed transaction's
    /// outcome.
    async fn outcomes(&self) -> Vec<(i64, Outcome)> {
        assert!(matches!(self.follow().await, Step::CaughtUp { .. }));
        self.prover().step().await.unwrap()
    }

    async fn outcome(&self, signed: i64) -> Outcome {
        self.outcomes()
            .await
            .into_iter()
            .find(|(id, _)| *id == signed)
            .map(|(_, o)| o)
            .expect("the prover looked at it")
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_ac_i4_1_the_pinned_polygon_block_starts_and_another_identity_is_refused(
    _pool: PgPoolOptions,
    _connect: PgConnectOptions,
) {
    let Some(upstream) = upstream() else { return };
    let anvil = Anvil::start(&upstream).await;
    let pair = anvil.pair();
    let polygon = Identity {
        chain_id: 137,
        pinned_number: FORK_BLOCK,
        pinned_hash: FORK_HASH.parse().unwrap(),
    };
    // The finalized head is 64 blocks behind: below the pinned block until mined past it.
    let e = network::check(&pair, &polygon).await.unwrap_err();
    assert!(e.contains("below the pinned block"), "{e}");
    anvil.finalize().await;
    let head = network::check(&pair, &polygon).await.unwrap();
    assert!(head.number > FORK_BLOCK);
    let base = Identity {
        chain_id: 8453,
        ..polygon
    };
    assert!(
        network::check(&pair, &base)
            .await
            .unwrap_err()
            .contains("AC-I4.1")
    );
    let other_hash = Identity {
        pinned_hash: B256::repeat_byte(1),
        ..polygon
    };
    assert!(
        network::check(&pair, &other_hash)
            .await
            .unwrap_err()
            .contains("not the pinned")
    );
}

/// PR #39 review S4 (`AC-I4.1`) and S5: F-008's deployed Roles modifier is the vault's (avatar,
/// target, enabled module) at the final point; another vault, or a delivery key carrying an
/// EIP-7702 delegation, fails closed.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_s4_ac_i4_1_the_deployed_roles_modifier_is_the_vaults(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let Some(upstream) = upstream() else { return };
    let w = World::new(&upstream, pool, connect).await;
    w.anvil.finalize().await;
    let head = w.pair.final_point().await.unwrap();
    let (roles, safe, lane) = (w.vault.roles, w.vault.safe, w.lane.address);
    network::check_vault(&w.pair, &head, roles, safe, lane)
        .await
        .unwrap();
    let e = network::check_vault(&w.pair, &head, roles, Address::repeat_byte(1), lane)
        .await
        .unwrap_err();
    assert!(e.contains("avatar"), "{e}");
    let e = network::check_vault(&w.pair, &head, safe, safe, lane)
        .await
        .unwrap_err();
    assert!(e.contains("AC-I4.1"), "{e}");
    let delegation = format!("0xef0100{}", hex::encode(Address::repeat_byte(0x77)));
    w.anvil
        .rpc("anvil_setCode", json!([lane, delegation]))
        .await;
    let e = network::check_vault(&w.pair, &head, roles, safe, lane)
        .await
        .unwrap_err();
    assert!(e.contains("holds code"), "{e}");
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_ac_a2_1_inv_10_ac_c1_3_inv_14_an_arrival_and_a_payout_at_the_vault(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let Some(upstream) = upstream() else { return };
    let w = World::new(&upstream, pool, connect).await;
    let depositor: Address = DEPOSITOR.parse().unwrap();
    w.anvil.fund_usdc(w.vault.safe, 5_000_000_000).await;
    w.anvil.fund_usdc(depositor, 1_000_000_000).await;

    // An arrival: not recorded before it is final (AC-A2.1), then once (INV-10).
    let transfer = evm::transferCall {
        to: w.vault.safe,
        amount: U256::from(123_450_000u64),
    }
    .abi_encode();
    let arrival = w
        .anvil
        .send(json!({"from": depositor, "to": usdc(), "data": format!("0x{}", hex::encode(transfer))}))
        .await;
    w.anvil.mine(1).await;
    w.follow().await;
    let arrivals = "SELECT count(*) FROM observations WHERE detail ->> 'event' = 'Arrival'";
    assert_eq!(w.count(arrivals).await, 0);
    w.anvil.finalize().await;
    w.follow().await;
    w.follow().await;
    assert_eq!(w.count(arrivals).await, 1);
    let (amount, sender, recipient, tx): (String, String, String, String) = sqlx::query_as(
        "SELECT amount::text, sender, recipient, detail ->> 'tx_hash' FROM observations \
         WHERE detail ->> 'event' = 'Arrival'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(amount, "123450000");
    assert_eq!(sender, depositor.to_string());
    assert_eq!(recipient, format!("{:#x}", w.vault.safe));
    assert_eq!(tx.parse::<B256>().unwrap(), arrival);

    // A payout through the delivery role: dry run, exact bytes, signature, submission.
    let beneficiary = Address::repeat_byte(0xbe);
    let reference = B256::repeat_byte(0x5c);
    let signed = w.payout(reference, beneficiary, 300_000_000).await;
    assert_eq!(
        w.anvil.usdc_balance(beneficiary).await,
        U256::from(300_000_000u64)
    );
    w.anvil.finalize().await;
    let Outcome::Executed { block } = w.outcome(signed).await else {
        panic!("the payout is not recorded executed");
    };
    // The payout leg: reference, token, amount, sender the vault, recipient (AC-C1.3, INV-14).
    type Leg = (i64, String, String, String, String, Vec<u8>);
    let (at, token, amount, sender, recipient, on_chain): Leg = sqlx::query_as(
        "SELECT block_number, token, amount::text, sender, recipient, on_chain_reference \
         FROM observations WHERE detail ->> 'event' = 'Payout'",
    )
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(u64::try_from(at).unwrap(), block);
    assert_eq!(token, usdc().to_string());
    assert_eq!(amount, "300000000");
    assert_eq!(sender, format!("{:#x}", w.vault.safe));
    assert_eq!(recipient, beneficiary.to_string());
    assert_eq!(on_chain, reference.to_vec());
    assert_eq!(
        w.count("SELECT count(*) FROM non_execution_proofs").await,
        0
    );
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_ac_c2_1_req_nx_4_a_forced_non_execution_proves_and_a_wrapped_copy_never_does(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    let Some(upstream) = upstream() else { return };
    let w = World::new(&upstream, pool, connect).await;
    w.anvil.fund_usdc(w.vault.safe, 5_000_000_000).await;
    w.anvil.finalize().await;
    w.follow().await;
    // Transactions wait in the pool until a block is mined.
    w.anvil.rpc("evm_setAutomine", json!([false])).await;
    let beneficiary = Address::repeat_byte(0xbe);

    // Forced non-execution: a payout signed and sent, then a cancel at its nonce that replaces it.
    let signed = w
        .payout(B256::repeat_byte(0x01), beneficiary, 100_000_000)
        .await;
    let cancel = w.pay("cancel", None, json!({"cancel": signed}), 0).await;
    w.anvil.mine(1).await;
    w.anvil.finalize().await;
    assert_eq!(w.anvil.usdc_balance(beneficiary).await, U256::ZERO);
    let outcomes = w.outcomes().await;
    let of = |id: i64| {
        outcomes
            .iter()
            .find(|(s, _)| *s == id)
            .map(|(_, o)| o.clone())
    };
    let Some(Outcome::NotExecuted {
        kind: "nonce-consumed-by-other",
        block,
    }) = of(signed)
    else {
        panic!("no proof of the forced non-execution: {outcomes:?}");
    };
    // The cancel itself executed, in that block.
    assert_eq!(of(cancel), Some(Outcome::Executed { block }));
    // The proof, checked against the chain alone (REQ-NX-2).
    type Proof = (Vec<u8>, i64, i64, i64, i64);
    let (signer, nonce, final_block, at, index): Proof = sqlx::query_as(
        "SELECT signer, nonce, final_block_number, at_block_number, at_position_index \
         FROM non_execution_proofs WHERE signed_transaction_id = $1",
    )
    .bind(signed)
    .fetch_one(&w.admin)
    .await
    .unwrap();
    assert_eq!(signer, w.lane.address.to_vec());
    assert_eq!(u64::try_from(at).unwrap(), block);
    let (_, txs) = w.pair.a.block_transactions(block).await.unwrap().unwrap();
    let consumer = &txs[usize::try_from(index).unwrap()];
    assert_eq!(consumer.from, w.lane.address);
    assert_eq!(consumer.nonce, u64::try_from(nonce).unwrap());
    assert!(consumer.input.is_empty() && consumer.value.is_zero());
    let after = w
        .pair
        .a
        .nonce(
            w.lane.address,
            bezent_evm_endpoint::rpc::At::Number(u64::try_from(final_block).unwrap()),
        )
        .await
        .unwrap();
    assert!(after > u64::try_from(nonce).unwrap());

    // A wrapped copy: the delivery key, at the next payout's nonce, calls the emitter with the
    // payout's reference outside the vault. It never proves the payout not executed.
    let reference = B256::repeat_byte(0x02);
    let signed2 = w.payout(reference, beneficiary, 100_000_000).await;
    let (payout2, _) = bezent_evm_endpoint::submit::decode_signed(
        &sqlx::query_scalar::<_, Vec<u8>>("SELECT bytes FROM signed_transactions WHERE id = $1")
            .bind(signed2)
            .fetch_one(&w.admin)
            .await
            .unwrap(),
    )
    .unwrap();
    let emit = evm::emitReferenceCall { r#ref: reference }.abi_encode();
    w.anvil
        .send(json!({
            "from": w.lane.address,
            "to": w.vault.emitter,
            "data": format!("0x{}", hex::encode(emit)),
            "nonce": format!("0x{:x}", payout2.nonce),
            "gas": "0x30000",
            "maxFeePerGas": format!("0x{:x}", payout2.max_fee_per_gas * 2),
            "maxPriorityFeePerGas": format!("0x{:x}", payout2.max_priority_fee_per_gas * 2),
        }))
        .await;
    w.anvil.mine(1).await;
    w.anvil.finalize().await;
    let Outcome::Unknown(why) = w.outcome(signed2).await else {
        panic!("a wrapped copy decided something");
    };
    assert!(why.contains("carries this reference"), "{why}");
    assert_eq!(
        w.count("SELECT count(*) FROM non_execution_proofs").await,
        1
    );
    assert_eq!(w.anvil.usdc_balance(beneficiary).await, U256::ZERO);
}

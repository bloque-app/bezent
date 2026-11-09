// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-009's fork tests: the endpoint against an anvil fork of Polygon, Base or BNB Chain (`M-2`) at the
//! block `e2e/evm/forks.env` pins, with the vault of `F-008` deployed by its own script (`contracts/evm/script/DeployVault.s.sol`)
//! and Foundry's test keys only. Nothing is sent to a real network: anvil reads the pinned state
//! from the upstream archive RPC and keeps every transaction local.
//!
//! Each network's tests run only when its upstream is named (an archive RPC): `BEZENT_EVM_FORK_URL` for
//! Polygon PoS, `BEZENT_EVM_FORK_URL_BASE` for Base, `BEZENT_EVM_FORK_URL_BSC` for BNB Chain; otherwise
//! each says so and passes. The same scenarios run on each network with its own token (USDC, EURC,
//! BTCB: 6, 6 and 18 decimals), its own vault and its own lane key. They need `anvil` and `forge` on the `PATH` and
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
use alloy::sol;
use alloy::sol_types::SolCall;
use bezent_evm_endpoint::broadcast::Broadcast;
use bezent_evm_endpoint::evm::{self, Contracts};
use bezent_evm_endpoint::network::{self, Declarations, Identity, Token};
use bezent_evm_endpoint::observe::{Follower, Step, Watch};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::proof::{Outcome, Prover};
use bezent_evm_endpoint::rpc::{Http, Rpc};
use bezent_evm_endpoint::submit::{GasBounds, Lane, Prepared, SignerLink, Submitted, Submitter};
use common::{Declared, role_pool, start_signer_on};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

/// One network under test: its pinned fork (`e2e/evm/forks.env`), its token and the amounts the
/// scenarios move, in the token's minor units.
struct Net {
    /// The network's id in the store, and the name of its vault configuration.
    name: &'static str,
    chain_id: u64,
    /// The pinned block, and its hash (read from two public providers).
    block: u64,
    hash: &'static str,
    /// The environment variable naming the upstream archive RPC.
    url_env: &'static str,
    /// The delivery key's id in the signer.
    key: &'static str,
    asset: &'static str,
    asset_precision: i16,
    token: &'static str,
    token_precision: i16,
    /// A chain id that is not this one.
    other_chain: u64,
    /// The arrival, what the vault and the depositor are funded with, a payout, and the payouts of
    /// the non-execution scenario.
    arrival: u128,
    fund_vault: u128,
    fund_depositor: u128,
    payout: u128,
    nx_payout: u128,
    /// Whether the vault declares a convertible (Polygon's USDT0).
    convertible: bool,
}

const POLYGON: Net = Net {
    name: "polygon",
    chain_id: 137,
    block: 95_400_000,
    hash: "0xcc89572742615188f853cc9e74e7f5279a369fbb4a019afc8027ec974b7fedd3",
    url_env: "BEZENT_EVM_FORK_URL",
    key: "polygon-payout",
    asset: "bUSDC",
    asset_precision: 6,
    token: "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359",
    token_precision: 6,
    other_chain: 8453,
    arrival: 123_450_000,
    fund_vault: 5_000_000_000,
    fund_depositor: 1_000_000_000,
    payout: 300_000_000,
    nx_payout: 100_000_000,
    convertible: true,
};

/// bEURC: Circle's EURC on Base, 6 decimals against the asset's 6.
const BASE: Net = Net {
    name: "base",
    chain_id: 8453,
    block: 52_500_000,
    hash: "0xe01c12b23874fe2286fa5214a307c7083c7445c32e2dfbc9493947c32a6a28fa",
    url_env: "BEZENT_EVM_FORK_URL_BASE",
    key: "base-payout",
    asset: "bEURC",
    asset_precision: 6,
    token: "0x60a3E35Cc302bFA44Cb288Bc5a4F316Fdb1adb42",
    token_precision: 6,
    other_chain: 137,
    arrival: 123_450_000,
    fund_vault: 5_000_000_000,
    fund_depositor: 1_000_000_000,
    payout: 300_000_000,
    nx_payout: 100_000_000,
    convertible: false,
};

/// bBTC: BTCB on BNB Chain, 18 decimals against the asset's 8 (a ratio of 10^10). The arrival has a
/// remainder of 9 012 345 678 (REQ-AS-9; the core scales, the endpoint records the arrival whole); the
/// payout is 12 345 678 units of bBTC times the ratio (REQ-AS-10).
const BSC: Net = Net {
    name: "bsc",
    chain_id: 56,
    block: 127_300_000,
    hash: "0xf391ca45ec033b2f95440fadaaa2c148b77c6aaf7516ef14b05d3394ffb928fe",
    url_env: "BEZENT_EVM_FORK_URL_BSC",
    key: "bsc-payout",
    asset: "bBTC",
    asset_precision: 8,
    token: "0x7130d2A12B9BCbFAe4f2634d864A1Ee1Ce3Ead9c",
    token_precision: 18,
    other_chain: 137,
    arrival: 123_456_789_012_345_678,
    fund_vault: 5_000_000_000_000_000_000,
    fund_depositor: 1_000_000_000_000_000_000,
    payout: 12_345_678 * 10_000_000_000,
    nx_payout: 100_000_000_000_000_000,
    convertible: false,
};

impl Net {
    fn token(&self) -> Address {
        self.token.parse().unwrap()
    }
}

sol! {
    /// `DirectedDeposit` (F-018, `contracts/evm/src/DirectedDeposit.sol`).
    function deposit(uint256 amount, bytes32 recipient) external;
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }
    /// Multicall3, at the same address on every network: a batch is one transaction.
    function aggregate3(Call3[] calls) external payable returns (bytes returnData);
}

/// Multicall3's address.
const MULTICALL3: &str = "0xcA11bde05977b3631167028862bE2a173976CA11";

/// Deploys `DirectedDeposit` for the vault on the fork with `forge create`: 1 to 50 000 USDC.
fn deploy_directed(anvil: &Anvil, vault: Address) -> Address {
    let out = Command::new("forge")
        .current_dir(contracts_dir())
        .args([
            "create",
            "src/DirectedDeposit.sol:DirectedDeposit",
            "--rpc-url",
            &anvil.url("127.0.0.1"),
            "--private-key",
            DEPLOYER_KEY,
            "--broadcast",
            "--json",
            "--constructor-args",
            &anvil.net.token().to_string(),
            &vault.to_string(),
            "1000000",
            "50000000000",
        ])
        .output()
        .expect("forge is on the PATH");
    assert!(
        out.status.success(),
        "forge create failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| v["deployedTo"].as_str().map(str::to_owned))
        .expect("the deployment's address")
        .parse()
        .unwrap()
}

/// `AC-A6.6`, `INV-10` (F-018, T-018-05): on the Polygon fork, with real USDC and the real
/// `DirectedDeposit`, a deposit is one `directed_arrival` at the `Transfer`'s coordinate and never
/// also an `Arrival`; a batch of two deposits in one transaction gives two pairs; a plain transfer
/// into the vault stays an `Arrival`; and no `Directed` log goes unpaired.
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_ac_a6_6_a_real_directed_deposit_is_one_directed_arrival_at_the_transfers_coordinate(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    bounded(
        concat!(module_path!(), "::fork_ac_a6_6_a_real_directed_deposit"),
        async move {
            let Some(upstream) = upstream(&POLYGON) else {
                return;
            };
            let mut w = World::new(&POLYGON, &upstream, pool, connect).await;
            let directed = deploy_directed(&w.anvil, w.vault.safe);
            let block = w
                .anvil
                .rpc("eth_blockNumber", json!([]))
                .await
                .as_str()
                .map(|n| u64::from_str_radix(n.trim_start_matches("0x"), 16).unwrap())
                .unwrap();
            w.directing = vec![(directed, block)];
            let token = POLYGON.token();
            let depositor: Address = DEPOSITOR.parse().unwrap();
            let multicall: Address = MULTICALL3.parse().unwrap();
            let recipient = B256::repeat_byte(0x5e);
            let usdc = |n: u64| U256::from(n) * U256::from(1_000_000u64);
            let send = |from: Address, to: Address, data: Vec<u8>| {
                json!({"from": from, "to": to, "data": format!("0x{}", hex::encode(data)),
                   "gas": "0x7a120"})
            };

            // 1. A deposit: approve, then deposit.
            w.anvil.fund_token(depositor, 100_000_000).await;
            w.anvil
                .send(send(
                    depositor,
                    token,
                    approveCall {
                        spender: directed,
                        amount: usdc(10),
                    }
                    .abi_encode(),
                ))
                .await;
            let first = w
                .anvil
                .send(send(
                    depositor,
                    directed,
                    depositCall {
                        amount: usdc(10),
                        recipient,
                    }
                    .abi_encode(),
                ))
                .await;

            // 2. A batch of two deposits in one transaction, by a contract (Multicall3 is the funder).
            w.anvil.fund_token(multicall, 100_000_000).await;
            let call = |target: Address, data: Vec<u8>| Call3 {
                target,
                allowFailure: false,
                callData: data.into(),
            };
            let batch = aggregate3Call {
                calls: vec![
                    call(
                        token,
                        approveCall {
                            spender: directed,
                            amount: usdc(30),
                        }
                        .abi_encode(),
                    ),
                    call(
                        directed,
                        depositCall {
                            amount: usdc(5),
                            recipient,
                        }
                        .abi_encode(),
                    ),
                    call(
                        directed,
                        depositCall {
                            amount: usdc(7),
                            recipient: B256::repeat_byte(0x6f),
                        }
                        .abi_encode(),
                    ),
                ],
            }
            .abi_encode();
            let second = w.anvil.send(send(depositor, multicall, batch)).await;

            // 3. A plain transfer into the vault: an arrival, as ever.
            let transfer = evm::transferCall {
                to: w.vault.safe,
                amount: usdc(3),
            }
            .abi_encode();
            let plain = w.anvil.send(send(depositor, token, transfer)).await;

            w.anvil.finalize().await;
            w.follow().await;
            w.follow().await;

            type Obs = (String, i64, String, String, String, Value);
            let rows: Vec<Obs> = sqlx::query_as(
                "SELECT kind, position_index, amount::text, sender, detail ->> 'tx_hash', detail \
             FROM observations WHERE network = 'polygon' AND position_kind = 'log' \
               AND kind IN ('directed_arrival', 'unpaired_direction', 'arrival') \
             ORDER BY block_number, position_index",
            )
            .fetch_all(&w.admin)
            .await
            .unwrap();
            let kinds: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
            assert_eq!(
                kinds,
                [
                    "directed_arrival",
                    "directed_arrival",
                    "directed_arrival",
                    "arrival"
                ],
                "{rows:?}"
            );
            let hash = |r: &Obs| r.4.parse::<B256>().unwrap();
            assert_eq!(hash(&rows[0]), first);
            assert_eq!((hash(&rows[1]), hash(&rows[2])), (second, second));
            assert_eq!(hash(&rows[3]), plain);
            // The amounts, the funders and the recipients each deposit named.
            assert_eq!(rows[0].2, usdc(10).to_string());
            assert_eq!(
                rows[0].3.to_lowercase(),
                depositor.to_string().to_lowercase()
            );
            assert_eq!(rows[1].2, usdc(5).to_string());
            assert_eq!(rows[2].2, usdc(7).to_string());
            assert_eq!(
                rows[1].3.to_lowercase(),
                multicall.to_string().to_lowercase()
            );
            assert_eq!(rows[1].5["recipient"], format!("0x{}", "5e".repeat(32)));
            assert_eq!(rows[2].5["recipient"], format!("0x{}", "6f".repeat(32)));
            // Each is at its Transfer's coordinate: the Directed log is the very next log, and the
            // batch's two pairs are adjacent, four logs apart at most.
            for r in &rows[..3] {
                assert_eq!(r.5["direction_log"].as_i64(), Some(r.1 + 1), "{r:?}");
                assert_eq!(r.5["directing"], directed.to_checksum(None));
            }
            assert!(rows[2].1 > rows[1].1);
        },
    )
    .await;
}

/// Foundry's default accounts and keys (anvil's), all test keys.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const OWNERS: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8,0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC,0x90F79bf6EB2c4f870365E785982E1f101E93b906";
const CONVERSION_MEMBER: &str = "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc";
// Foundry's account 7: the vault's guardian (veto and pause), none of the deployer, the owners or the roles.
const GUARDIAN: &str = "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955";
// Foundry's account 8 is the vault's conversion member (Swaps' key, REQ-CV-7). Uniswap v3's SwapRouter02
// on Polygon, and the fee tier of the declared USDT0/USDC pool (contracts/evm/config/polygon.json).
const ROUTER: &str = "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45";
const POOL_FEE: u32 = 100;

sol! {
    /// ERC-20 `approve`.
    function approve(address spender, uint256 amount) external returns (bool);
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }
    /// SwapRouter02's exact-input single swap.
    function exactInputSingle(ExactInputSingleParams params) external payable returns (uint256 amountOut);
}

/// What a hang costs: each test fails, naming itself, within `TEST_BOUND` (the slowest runs in
/// about 40 s on a runner), where an unbounded one held the job until its 40-minute limit. A bound
/// that is passed fails the test; it never skips it or lets it pass.
const TEST_BOUND: Duration = Duration::from_secs(300);
/// anvil must answer its first call within this, upstream state included.
const ANVIL_START_BOUND: Duration = Duration::from_secs(90);
/// One call to anvil: anvil itself gives up on its upstream after 60 s, and retries.
const ANVIL_CALL_BOUND: Duration = Duration::from_secs(90);
/// `forge script`'s deployment (a few seconds, up to 20 s against a slow upstream).
const DEPLOY_BOUND: Duration = Duration::from_secs(180);

/// Runs one test's body within `TEST_BOUND`, and fails the test, naming it, if it is not done.
/// This ends a body that awaits; a blocking wait (the deployment) has its own bound.
async fn bounded<F: std::future::Future<Output = ()>>(test: &str, body: F) {
    if tokio::time::timeout(TEST_BOUND, body).await.is_err() {
        panic!("{test} did not finish within {TEST_BOUND:?}: it is stuck, not slow");
    }
}

const DEPOSITOR: &str = "0x976EA74026E726554dB657fA54763abd0C3a0aa9";
fn upstream(net: &Net) -> Option<String> {
    let url = std::env::var(net.url_env).ok().filter(|u| !u.is_empty());
    if url.is_none() {
        eprintln!(
            "{} is not set: the {} fork test is skipped",
            net.url_env, net.name
        );
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
    net: &'static Net,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Anvil {
    async fn start(net: &'static Net, upstream: &str) -> Anvil {
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
                &net.block.to_string(),
                "--port",
                &port.to_string(),
                // A public upstream drops or rate-limits some requests: tried again, patiently,
                // before anvil gives up (it aborts when a state read fails for good).
                "--retries",
                "12",
                "--fork-retry-backoff",
                "1000",
                "--timeout",
                "60000",
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
            net,
        };
        let ready = async {
            while anvil.call("eth_blockNumber", json!([])).await.is_err() {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        if tokio::time::timeout(ANVIL_START_BOUND, ready)
            .await
            .is_err()
        {
            let log = std::fs::read_to_string(&anvil.log)
                .unwrap_or_default()
                .replace(&anvil.upstream, "<upstream>");
            let tail: Vec<&str> = log.lines().rev().take(20).collect();
            panic!(
                "anvil ({}) did not answer within {ANVIL_START_BOUND:?}; its last lines:\n{}",
                net.name,
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        anvil
    }

    fn url(&self, host: &str) -> String {
        format!("http://{host}:{}", self.port)
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, String> {
        match tokio::time::timeout(
            ANVIL_CALL_BOUND,
            self.rpc.request::<Value, Value>(method, params),
        )
        .await
        {
            Ok(answer) => answer.map_err(|e| e.to_string()),
            Err(_) => Err(format!("no answer within {ANVIL_CALL_BOUND:?}")),
        }
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
        // What anvil said before its backtrace: the panic's own message.
        let cause: Vec<&str> = log
            .lines()
            .filter(|l| l.contains("panicked") || l.to_lowercase().contains("error"))
            .take(5)
            .collect();
        let tail: Vec<&str> = log
            .lines()
            .filter(|l| !l.contains("Endpoint"))
            .rev()
            .take(30)
            .collect();
        panic!(
            "{method}: {last}; anvil exited: {status:?}; its first errors:\n{}\nits last lines:\n{}",
            cause.join("\n"),
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

    /// The one broadcast node: this anvil, over plain loopback http (amendment 0023). The payouts
    /// the suite sends go through here, not through the pair.
    fn broadcast(&self) -> Broadcast<Http> {
        Broadcast::new(vec![Http::new("anvil", &self.url("127.0.0.1")).unwrap()])
    }

    /// Mines `n` blocks.
    async fn mine(&self, n: u64) {
        self.rpc("anvil_mine", json!([format!("0x{n:x}")])).await;
    }

    /// Mines past finality: 64 blocks and one.
    async fn finalize(&self) {
        self.mine(65).await;
    }

    /// Sets `holder`'s balance of the network's token. The balance's storage slot is found, not
    /// assumed: each candidate slot of a Solidity mapping is written with a sentinel and the token
    /// asked (USDC and EURC keep it at 9, BTCB elsewhere).
    async fn fund_token(&self, holder: Address, amount: u128) {
        self.fund_token_of(self.net.token(), holder, amount).await;
    }

    /// The same for any token: the convertible, for the conversion scenario.
    async fn fund_token_of(&self, token: Address, holder: Address, amount: u128) {
        for slot in 0u64..=60 {
            let key = keccak256(
                [
                    holder.into_word().as_slice(),
                    U256::from(slot).to_be_bytes::<32>().as_slice(),
                ]
                .concat(),
            );
            self.rpc(
                "anvil_setStorageAt",
                json!([token, key, B256::from(U256::from(amount))]),
            )
            .await;
            if self.balance_of(token, holder).await == U256::from(amount) {
                return;
            }
            self.rpc("anvil_setStorageAt", json!([token, key, B256::ZERO]))
                .await;
        }
        panic!("no balance slot of {token} found for {holder}");
    }

    async fn token_balance(&self, holder: Address) -> U256 {
        self.balance_of(self.net.token(), holder).await
    }

    async fn balance_of(&self, token: Address, holder: Address) -> U256 {
        let data = evm::balanceOfCall { owner: holder }.abi_encode();
        let out = self
            .rpc(
                "eth_call",
                json!([{"to": token, "data": format!("0x{}", hex::encode(data))}, "latest"]),
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
    let net = anvil.net;
    let mut forge = Command::new("forge");
    forge
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
        .env("NETWORK", net.name)
        .env("VAULT_OWNERS", OWNERS)
        .env("VAULT_THRESHOLD", "2")
        .env("DELIVERY_MEMBER", lane.to_string())
        .env("GUARDIAN", GUARDIAN)
        // The Delay's cooldown and expiration (seconds): contracts/evm/config/vault.example.env.
        .env("VAULT_DELAY_COOLDOWN", "86400")
        .env("VAULT_DELAY_EXPIRATION", "604800");
    // A conversion member only where the vault has a conversion venue (Polygon's; Base and BNB Chain
    // declare none, and the script refuses a member without one).
    if net.convertible {
        forge.env("CONVERSION_MEMBER", CONVERSION_MEMBER);
    }
    // Run with its output in files and waited for up to DEPLOY_BOUND: `output()` waits for ever,
    // and a forge that never returns held a job for 40 minutes.
    let dir = common::temp_dir();
    let (out_path, err_path) = (dir.join("forge.out"), dir.join("forge.err"));
    let mut child = forge
        .stdout(Stdio::from(std::fs::File::create(&out_path).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&err_path).unwrap()))
        .spawn()
        .expect("forge is on the PATH");
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > DEPLOY_BOUND {
            let _ = child.kill();
            let _ = child.wait();
            let log = std::fs::read_to_string(&err_path).unwrap_or_default();
            let tail: Vec<&str> = log.lines().rev().take(20).collect();
            panic!(
                "DeployVault ({}) did not finish within {DEPLOY_BOUND:?}; forge's last lines:\n{}",
                net.name,
                tail.into_iter().rev().collect::<Vec<_>>().join("\n")
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let stdout = std::fs::read_to_string(&out_path).unwrap_or_default();
    assert!(
        status.success(),
        "DeployVault failed: {}",
        std::fs::read_to_string(&err_path).unwrap_or_default()
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
    net: &'static Net,
    anvil: Anvil,
    pair: Pair<Http>,
    broadcast: Broadcast<Http>,
    admin: PgPool,
    endpoint: PgPool,
    core: PgPool,
    lane: Lane,
    link: SignerLink,
    vault: Vault,
    declarations: Declarations,
    convertibles: Vec<(Address, Address)>,
    /// The declared directing contracts (F-018); none unless a test deploys one.
    directing: Vec<(Address, u64)>,
}

impl World {
    async fn new(
        net: &'static Net,
        upstream: &str,
        pool: PgPoolOptions,
        connect: PgConnectOptions,
    ) -> World {
        let anvil = Anvil::start(net, upstream).await;
        let (_, lane) = start_signer_on(
            &connect,
            common::contracts(),
            net.name,
            net.chain_id,
            net.key,
        )
        .await;
        // Foundry's account 4 carries an EIP-7702 delegation on Polygon at the pinned block (anyone
        // can sign one with a published key). bezent's delivery key has none: it is cleared here,
        // so a cancel, a transfer of nothing to the key itself, runs no code.
        anvil
            .rpc("anvil_setCode", json!([lane.address, "0x"]))
            .await;
        let vault = deploy(&anvil, lane.address);
        // The signer that signs here declares this vault's contracts, deployed only now: a payout's
        // bytes must name them (audit must-fix 2). The first signer only told the lane.
        let (link, lane) = start_signer_on(
            &connect,
            Contracts {
                roles: vault.roles,
                multisend: common::contracts().multisend,
                emitter: vault.emitter,
                delivery_role: evm::role_key("bezent.delivery").unwrap(),
            },
            net.name,
            net.chain_id,
            net.key,
        )
        .await;
        let admin = pool.connect_with(connect.clone()).await.unwrap();
        let vault_text = format!("{:#x}", vault.safe);
        common::declare_on(
            &admin,
            &vault_text,
            &Declared {
                network: net.name,
                chain_id: net.chain_id,
                asset: net.asset,
                asset_precision: net.asset_precision,
                token: &net.token().to_string(),
                token_precision: net.token_precision,
            },
        )
        .await;
        let declarations = network::declarations(&admin, net.name, vault.safe)
            .await
            .unwrap();
        assert_eq!(
            declarations.tokens.get(&net.token()),
            Some(&Token {
                text: net.token().to_string(),
                asset: net.asset.into()
            })
        );
        World {
            net,
            pair: anvil.pair(),
            broadcast: anvil.broadcast(),
            anvil,
            endpoint: role_pool(&connect, "bezent_evm_endpoint").await,
            core: role_pool(&connect, "bezent_core").await,
            admin,
            lane,
            link,
            vault,
            declarations,
            convertibles: if net.convertible {
                vec![(common::usdt0(), common::pool_address())]
            } else {
                Vec::new()
            },
            directing: Vec::new(),
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
            network: self.net.name,
            watch: Watch {
                vault: self.vault.safe,
                emitter: self.vault.emitter,
                declarations: &self.declarations,
                convertibles: &self.convertibles,
                delivery: self.lane.address,
                directing: &self.directing,
            },
            start_block: self.net.block + 1,
            max_range: 500,
        }
    }

    fn submitter(&self) -> Submitter<'_, Http> {
        Submitter {
            pair: &self.pair,
            broadcast: &self.broadcast,
            pool: &self.endpoint,
            network: self.net.name,
            chain_id: self.net.chain_id,
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
            network: self.net.name,
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
        .bind(self.net.name)
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
            json!({"to": to.to_string(), "asset": self.net.asset, "token": self.net.token().to_string()}),
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

/// The scenarios, once per network: `mod polygon`, `mod base`, `mod bsc`.
macro_rules! fork_tests {
    ($net:expr) => {
    #[sqlx::test(migrator = "bezent_store::MIGRATOR")]
    async fn fork_ac_i4_1_the_pinned_block_starts_and_another_identity_is_refused(
        _pool: PgPoolOptions,
        _connect: PgConnectOptions,
    ) {
        bounded(concat!(module_path!(), "::fork_ac_i4_1_the_pinned_block_starts_and_another_identity_is_refused"), async move {
            let Some(upstream) = upstream(&$net) else { return };
            let anvil = Anvil::start(&$net, &upstream).await;
            let pair = anvil.pair();
            // Amendment 0023: the broadcast node serves the declared chain, and not another.
            anvil.broadcast().check_chain($net.chain_id).await.unwrap();
            anvil.broadcast().check_chain($net.other_chain).await.unwrap_err();
            let polygon = Identity {
                chain_id: $net.chain_id,
                pinned_number: $net.block,
                pinned_hash: $net.hash.parse().unwrap(),
            };
            // The finalized head is 64 blocks behind: below the pinned block until mined past it.
            let e = network::check(&pair, &polygon).await.unwrap_err();
            assert!(e.contains("below the pinned block"), "{e}");
            anvil.finalize().await;
            let head = network::check(&pair, &polygon).await.unwrap();
            assert!(head.number > $net.block);
            let base = Identity {
                chain_id: $net.other_chain,
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
        })
        .await;
    }

    /// PR #39 review S4 (`AC-I4.1`) and S5: F-008's deployed Roles modifier is the vault's (avatar,
    /// target, enabled module) at the final point; another vault, or a delivery key carrying an
    /// EIP-7702 delegation, fails closed.
    #[sqlx::test(migrator = "bezent_store::MIGRATOR")]
    async fn fork_s4_ac_i4_1_the_deployed_roles_modifier_is_the_vaults(
        pool: PgPoolOptions,
        connect: PgConnectOptions,
    ) {
        bounded(concat!(module_path!(), "::fork_s4_ac_i4_1_the_deployed_roles_modifier_is_the_vaults"), async move {
            let Some(upstream) = upstream(&$net) else { return };
            let w = World::new(&$net, &upstream, pool, connect).await;
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
        })
        .await;
    }

    #[sqlx::test(migrator = "bezent_store::MIGRATOR")]
    async fn fork_ac_a2_1_inv_10_ac_c1_3_inv_14_an_arrival_and_a_payout_at_the_vault(
        pool: PgPoolOptions,
        connect: PgConnectOptions,
    ) {
        bounded(concat!(module_path!(), "::fork_ac_a2_1_inv_10_ac_c1_3_inv_14_an_arrival_and_a_payout_at_the_vault"), async move {
            let Some(upstream) = upstream(&$net) else { return };
            let w = World::new(&$net, &upstream, pool, connect).await;
            let depositor: Address = DEPOSITOR.parse().unwrap();
            w.anvil.fund_token(w.vault.safe, $net.fund_vault).await;
            w.anvil.fund_token(depositor, $net.fund_depositor).await;

            // An arrival: not recorded before it is final (AC-A2.1), then once (INV-10).
            let transfer = evm::transferCall {
                to: w.vault.safe,
                amount: U256::from($net.arrival),
            }
            .abi_encode();
            let arrival = w
                .anvil
                .send(json!({"from": depositor, "to": $net.token(), "data": format!("0x{}", hex::encode(transfer))}))
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
            assert_eq!(amount, $net.arrival.to_string());
            assert_eq!(sender, depositor.to_string());
            assert_eq!(recipient, format!("{:#x}", w.vault.safe));
            assert_eq!(tx.parse::<B256>().unwrap(), arrival);

            // A payout through the delivery role: dry run, exact bytes, signature, submission.
            let beneficiary = Address::repeat_byte(0xbe);
            let reference = B256::repeat_byte(0x5c);
            let signed = w.payout(reference, beneficiary, $net.payout).await;
            assert_eq!(
                w.anvil.token_balance(beneficiary).await,
                U256::from($net.payout)
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
            assert_eq!(token, $net.token().to_string());
            assert_eq!(amount, $net.payout.to_string());
            assert_eq!(sender, format!("{:#x}", w.vault.safe));
            assert_eq!(recipient, beneficiary.to_string());
            assert_eq!(on_chain, reference.to_vec());
            assert_eq!(
                w.count("SELECT count(*) FROM non_execution_proofs").await,
                0
            );
        })
        .await;
    }

    #[sqlx::test(migrator = "bezent_store::MIGRATOR")]
    async fn fork_ac_c2_1_req_nx_4_a_forced_non_execution_proves_and_a_wrapped_copy_never_does(
        pool: PgPoolOptions,
        connect: PgConnectOptions,
    ) {
        bounded(concat!(module_path!(), "::fork_ac_c2_1_req_nx_4_a_forced_non_execution_proves_and_a_wrapped_copy_never_does"), async move {
            let Some(upstream) = upstream(&$net) else { return };
            let w = World::new(&$net, &upstream, pool, connect).await;
            w.anvil.fund_token(w.vault.safe, $net.fund_vault).await;
            w.anvil.finalize().await;
            w.follow().await;
            // Transactions wait in the pool until a block is mined.
            w.anvil.rpc("evm_setAutomine", json!([false])).await;
            let beneficiary = Address::repeat_byte(0xbe);

            // Forced non-execution: a payout signed and sent, then a cancel at its nonce that replaces it.
            let signed = w
                .payout(B256::repeat_byte(0x01), beneficiary, $net.nx_payout)
                .await;
            let cancel = w.pay("cancel", None, json!({"cancel": signed}), 0).await;
            w.anvil.mine(1).await;
            w.anvil.finalize().await;
            assert_eq!(w.anvil.token_balance(beneficiary).await, U256::ZERO);
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
            let signed2 = w.payout(reference, beneficiary, $net.nx_payout).await;
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
            assert_eq!(w.anvil.token_balance(beneficiary).await, U256::ZERO);
        })
        .await;
    }
    };
}

mod polygon {
    use super::*;
    fork_tests!(POLYGON);
}

mod base {
    use super::*;
    fork_tests!(BASE);
}

mod bsc {
    use super::*;
    fork_tests!(BSC);
}

/// `AC-A5.1`, `AC-A5.2`, `REQ-CV-2`, `REQ-CV-4`, `REQ-CV-7`: on the Polygon fork, a convertible arrives at the
/// vault and Swaps' conversion member converts it through the declared pool under its role. The
/// endpoint records the arrival as a convertible arrival (never an arrival), and the swap as an input
/// and an output at their coordinates, the output carrying both amounts; the output is what the vault
/// received, whatever the price (`REQ-CV-5`).
#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn fork_ac_a5_1_ac_a5_2_req_cv_2_a_real_swap_is_observed_with_both_amounts(
    pool: PgPoolOptions,
    connect: PgConnectOptions,
) {
    bounded(concat!(module_path!(), "::fork_ac_a5_1_ac_a5_2_req_cv_2_a_real_swap_is_observed_with_both_amounts"), async move {
        let Some(upstream) = upstream(&POLYGON) else {
            return;
        };
        let w = World::new(&POLYGON, &upstream, pool, connect).await;
        let usdt0 = common::usdt0();
        let depositor: Address = DEPOSITOR.parse().unwrap();
        let member: Address = CONVERSION_MEMBER.parse().unwrap();
        let router: Address = ROUTER.parse().unwrap();
        let amount_in: u128 = 1_000_000_000;

        // 1 000 USDT0 arrive at the vault from a depositor.
        w.anvil.fund_token_of(usdt0, depositor, amount_in).await;
        let transfer = evm::transferCall {
            to: w.vault.safe,
            amount: U256::from(amount_in),
        }
        .abi_encode();
        let arrival = w
            .anvil
            .send(
                json!({"from": depositor, "to": usdt0, "data": format!("0x{}", hex::encode(transfer))}),
            )
            .await;

        // Swaps' key converts it through the vault's conversion role: approve the router, then the swap.
        let through_role = |to: Address, data: Vec<u8>| {
            let call = evm::execTransactionWithRoleCall {
                to,
                value: U256::ZERO,
                data: data.into(),
                operation: 0,
                roleKey: evm::role_key("bezent.conversion").unwrap(),
                shouldRevert: true,
            }
            .abi_encode();
            json!({"from": member, "to": w.vault.roles, "data": format!("0x{}", hex::encode(call)),
                   "gas": "0x7a120"})
        };
        let usdc_before = w.anvil.token_balance(w.vault.safe).await;
        w.anvil
            .send(through_role(
                usdt0,
                approveCall {
                    spender: router,
                    amount: U256::from(amount_in),
                }
                .abi_encode(),
            ))
            .await;
        let swap = w
            .anvil
            .send(through_role(
                router,
                exactInputSingleCall {
                    params: ExactInputSingleParams {
                        tokenIn: usdt0,
                        tokenOut: POLYGON.token(),
                        fee: alloy::primitives::aliases::U24::from(POOL_FEE),
                        recipient: w.vault.safe,
                        amountIn: U256::from(amount_in),
                        amountOutMinimum: U256::ZERO,
                        sqrtPriceLimitX96: alloy::primitives::aliases::U160::ZERO,
                    },
                }
                .abi_encode(),
            ))
            .await;
        let delivered = w.anvil.token_balance(w.vault.safe).await - usdc_before;
        assert!(delivered > U256::ZERO, "the swap delivered nothing");
        w.anvil.finalize().await;
        w.follow().await;
        w.follow().await;

        // The arrival is a convertible arrival: never an arrival, so nothing can mint against it (REQ-CV-1).
        let (kind, amount, tx): (String, String, String) = sqlx::query_as(
            "SELECT kind, amount::text, detail ->> 'tx_hash' FROM observations \
             WHERE detail ->> 'event' = 'ConvertibleArrival'",
        )
        .fetch_one(&w.admin)
        .await
        .unwrap();
        assert_eq!(kind, "convertible_arrival");
        assert_eq!(amount, amount_in.to_string());
        assert_eq!(tx.parse::<B256>().unwrap(), arrival);
        assert_eq!(
            w.count("SELECT count(*) FROM observations WHERE kind = 'arrival'")
                .await,
            0
        );

        // The swap: the input leaves the vault for the declared pool; the output lands in the vault, from
        // the pool, in the same transaction, in the amount the vault received (REQ-CV-2, REQ-CV-5).
        let (input_kind, input_amount, input_to, input_tx): (String, String, String, String) =
            sqlx::query_as(
                "SELECT kind, amount::text, recipient, detail ->> 'tx_hash' FROM observations \
                 WHERE detail ->> 'event' = 'ConversionInput'",
            )
            .fetch_one(&w.admin)
            .await
            .unwrap();
        assert_eq!(input_kind, "conversion_input");
        assert_eq!(input_amount, amount_in.to_string());
        assert_eq!(
            input_to.to_lowercase(),
            common::pool_address().to_string().to_lowercase()
        );
        assert_eq!(input_tx.parse::<B256>().unwrap(), swap);

        type Output = (String, String, String, String, String, Value);
        let (kind, amount, sender, recipient, tx, detail): Output = sqlx::query_as(
            "SELECT kind, amount::text, sender, recipient, detail ->> 'tx_hash', detail -> 'conversion' \
             FROM observations WHERE detail ->> 'event' = 'Conversion'",
        )
        .fetch_one(&w.admin)
        .await
        .unwrap();
        assert_eq!(kind, "conversion_output");
        assert_eq!(
            amount,
            delivered.to_string(),
            "the output is what the vault received"
        );
        assert_eq!(
            sender.to_lowercase(),
            common::pool_address().to_string().to_lowercase()
        );
        assert_eq!(recipient, format!("{:#x}", w.vault.safe));
        assert_eq!(tx.parse::<B256>().unwrap(), swap);
        // Both amounts, in the output's own record.
        assert_eq!(detail["amount_in"], amount_in.to_string());
        let lower = |v: &Value| v.as_str().unwrap().to_lowercase();
        assert_eq!(lower(&detail["token_in"]), usdt0.to_string().to_lowercase());
        assert_eq!(
            lower(&detail["pool"]),
            common::pool_address().to_string().to_lowercase()
        );
        // A stable pair at a 0.01 % tier: within one per cent of par, so this is a real swap's output.
        assert!(delivered > U256::from(amount_in) * U256::from(99) / U256::from(100));
        assert!(delivered < U256::from(amount_in) * U256::from(101) / U256::from(100));
    })
    .await;
}

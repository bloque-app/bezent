// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The bed: every service of the bridge running together on one store, each on its own login.
//!
//! What is real here, and what is not (this is repeated in the job summary and the pull request):
//!
//! - **Polygon is an anvil fork** of the real chain at the block `e2e/evm/forks.env` pins: the real USDC,
//!   Safe, Zodiac Roles and MultiSend contracts, the vault `F-008`'s script deploys, transactions
//!   that execute in anvil's EVM and are mined locally. It is a fork of a chain, not the chain.
//! - **Kreivo is `FakeChain`: an in-memory model**, not a chain. It models blocks, finality, the
//!   contract's events, mortal eras, nonces and signature checks, and nothing else. No runtime runs,
//!   no `pallet-assets`, no `pallet-contracts`. Its supply is the harness's word, derived here from
//!   the model's own events (never from the store). A redemption is an event the model is told to
//!   emit, not a holder's call that destroys anything. Zombienet is the real Kreivo; this suite does
//!   not run on it.
//! - **The services are the real code**, in process: the ingress (served over HTTP on a bound
//!   port), the core's cycle and reserve check, the signer (over its real Unix-socket link, with two
//!   callers), the EVM endpoint's follower, submitter and prover, the Kreivo endpoint's follower,
//!   supplies, submitter and prover, on the real migrations, each on a login that is a member of its
//!   own role only, checked as its `main` checks it. The passes below repeat the order of each
//!   endpoint's loop in `main.rs` (cited at [`Bed::evm_pass`] and [`Bed::kreivo_pass`]); the loops
//!   themselves live in `main.rs` and cannot be called, and the Kreivo endpoint's binary would need
//!   the light client. Operators are two logins of `bezent_operator`, as in production.
//! - **Keys** are the published test keys: Foundry's account 4 for the Polygon delivery key, a
//!   derived sr25519 seed for the Kreivo mint key. The signer holds them; no endpoint does.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::rpc::client::{ClientBuilder, RpcClient};
use alloy::sol_types::SolCall;
use bezent_core_service::reserve::check::{LogPager, ReserveCheck};
use bezent_core_service::reserve::readings::Recorded;
use bezent_core_service::{Core, Settings};
use bezent_evm_endpoint::broadcast::Broadcast;
use bezent_evm_endpoint::evm::{self, Contracts};
use bezent_evm_endpoint::network::{self, Declarations, Identity};
use bezent_evm_endpoint::observe::{Follower as EvmFollower, Step as EvmStep, Watch};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::proof::Prover as EvmProver;
use bezent_evm_endpoint::rpc::Http;
use bezent_evm_endpoint::submit::{
    GasBounds, Lane as EvmLane, SignerLink as EvmLink, Submitter as EvmSubmitter,
};
use bezent_kreivo_endpoint::chain::Chain;
use bezent_kreivo_endpoint::chain::fake::FakeChain;
use bezent_kreivo_endpoint::kreivo::{self, AccountId32, AssetId, ContractEvent, Scheme};
use bezent_kreivo_endpoint::observe::{Follower as KreivoFollower, Step as KreivoStep};
use bezent_kreivo_endpoint::proof::Prover as KreivoProver;
use bezent_kreivo_endpoint::submit::{
    Gate as KreivoGate, Lane as KreivoLane, SignerLink as KreivoLink, Submitter as KreivoSubmitter,
};
use bezent_kreivo_endpoint::supply::Supplies;
use bezent_signer::config::{Caller, Config as SignerConfig};
use bezent_signer::keys::{FileBackend, SigningEngine};
use bezent_signer::link::{self, Callers, Dialer};
use bezent_signer::log::Log;
use bezent_signer::signer::Signer;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

// ---------------------------------------------------------------------------------------------
// What the bed declares.

/// The issuing network, as the store and the signer name it.
pub const KREIVO: &str = "kreivo-test";
/// The origin network.
pub const POLYGON: &str = "polygon";
/// Its chain id, and the pinned block and hash (`e2e/evm/forks.env`; the hash as `fork.rs` reads it
/// from two public providers).
pub const CHAIN_ID: u64 = 137;
pub const PINNED_BLOCK: u64 = 95_400_000;
pub const PINNED_HASH: &str = "0xcc89572742615188f853cc9e74e7f5279a369fbb4a019afc8027ec974b7fedd3";
/// The bridged asset, its local identity on the issuing network, and its origin token (native USDC).
pub const ASSET: &str = "bUSDC";
pub const LOCAL_ASSET: u32 = 101;
pub const USDC: &str = "0x3c499c542cEF5E3811e1192ce70d8cC03d5c3359";
/// The Kreivo model's genesis and the contract's account.
pub const GENESIS: [u8; 32] = [0x6b; 32];
pub const CONTRACT: AccountId32 = [0xc0; 32];
/// Alice, prefix 42, and her account: the holder of the minted bridged asset.
pub const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
pub const ALICE_ACCOUNT: AccountId32 = [
    0xd4, 0x35, 0x93, 0xc7, 0x15, 0xfd, 0xd3, 0x1c, 0x61, 0x14, 0x1a, 0xbd, 0x04, 0xa9, 0x9f, 0xd6,
    0x82, 0x2c, 0x85, 0x58, 0x85, 0x4c, 0xcd, 0xe3, 0x9a, 0x56, 0x84, 0xe7, 0xa5, 0x6d, 0xa2, 0x7d,
];
/// The requester's credential (`BEZENT_SIDECAR_REQUESTERS`' shape): id `.` secret.
pub const REQUESTER: &str = "requester-k1.requester-secret-0123456789";

/// Foundry's default accounts and keys (anvil's), all test keys. Account 4 is the delivery key.
const DEPLOYER_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const LANE_SECRET: &str = "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a";
const OWNERS: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8,0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC,0x90F79bf6EB2c4f870365E785982E1f101E93b906";
const GUARDIAN: &str = "0x14dC79964da2C08b23698B3D3cc7Ca32193d9955";
/// Foundry's account 9: the depositor.
pub const DEPOSITOR: &str = "0xa0Ee7A142d267C1f36714E4a8F75612F20a79720";
/// The beneficiary of a payout.
pub const BENEFICIARY: Address = Address::repeat_byte(0xbe);

/// The signer's keys.
const LANE_KEY: &str = "polygon-payout";
const MINT_KEY: &str = "kreivo-mint";
const EVM_TOKEN: &str = "e2e-evm-endpoint-token";
const KREIVO_TOKEN: &str = "e2e-kreivo-endpoint-token";
const LOGIN_PASSWORD: &str = "test-only-full-chain-suite-login";
/// The two operators.
pub const OPERATORS: [&str; 2] = ["operator_alice", "operator_bob"];

/// The fork's upstream, from the environment. A suite that expects a fork and has none **fails**:
/// there is no skip branch (a skipped end-to-end run is a green nothing).
fn upstream() -> String {
    match std::env::var("BEZENT_E2E_FORK_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => panic!(
            "BEZENT_E2E_FORK_URL is not set: the full-chain suite needs an archive RPC for Polygon to fork at block {PINNED_BLOCK}, and it does not skip. Set it (any archive endpoint; e2e/evm/up.sh names public ones) or do not run this feature."
        ),
    }
}

// ---------------------------------------------------------------------------------------------
// anvil.

/// An anvil fork of Polygon, stopped on drop.
pub struct Anvil {
    child: std::sync::Mutex<Child>,
    port: u16,
    rpc: RpcClient,
    log: PathBuf,
    upstream: String,
    _scratch: Scratch,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let child = self.child.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// A scratch directory (the test keys, the signer's socket and configuration, anvil's log),
/// removed when its owner drops, a panicking scenario's unwinding included.
pub struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_dir() -> Scratch {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bzfc-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn my_uid() -> u32 {
    let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
    a.set_nonblocking(true).unwrap();
    tokio::net::UnixStream::from_std(a)
        .unwrap()
        .peer_cred()
        .unwrap()
        .uid()
}

impl Anvil {
    /// Starts the fork. The port is chosen by binding and releasing it, which another process can
    /// win in between: a start that fails (anvil exits, or never answers) is tried again, on a
    /// new port, up to three times.
    pub async fn start() -> Anvil {
        let upstream = upstream();
        let mut last = String::new();
        for _ in 0..3 {
            match Anvil::try_start(&upstream).await {
                Ok(anvil) => return anvil,
                Err(why) => last = why,
            }
        }
        panic!("anvil did not start in three tries: {last}");
    }

    async fn try_start(upstream: &str) -> Result<Anvil, String> {
        let upstream = upstream.to_owned();
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        // anvil prints the upstream URL in its banner and the URL may carry a key: its output goes
        // to a file that is redacted before anything prints it.
        let scratch = temp_dir();
        let log = scratch.join("anvil.log");
        let out = std::fs::File::create(&log).unwrap();
        let child = Command::new("anvil")
            .args([
                "--fork-url",
                &upstream,
                "--fork-block-number",
                &PINNED_BLOCK.to_string(),
                "--port",
                &port.to_string(),
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
            .expect("anvil is not on the PATH: the full-chain suite needs Foundry (anvil and forge) and does not skip");
        let rpc =
            ClientBuilder::default().http(format!("http://127.0.0.1:{port}").parse().unwrap());
        let anvil = Anvil {
            child: std::sync::Mutex::new(child),
            port,
            rpc,
            log,
            upstream,
            _scratch: scratch,
        };
        for _ in 0..120 {
            if anvil.call("eth_blockNumber", json!([])).await.is_ok() {
                return Ok(anvil);
            }
            let exited = anvil.child.lock().unwrap().try_wait().ok().flatten();
            if let Some(status) = exited {
                return Err(format!("anvil exited ({status}): {}", anvil.tail()));
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(format!("anvil did not answer: {}", anvil.tail()))
    }

    pub fn url(&self, host: &str) -> String {
        format!("http://{host}:{}", self.port)
    }

    /// Its last lines, with the upstream redacted.
    fn tail(&self) -> String {
        let log = std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .replace(&self.upstream, "<upstream>");
        let lines: Vec<&str> = log.lines().filter(|l| !l.contains("Endpoint")).collect();
        // What anvil said before its backtrace (the panic's own message), then its last lines.
        let cause: Vec<&str> = lines
            .iter()
            .copied()
            .filter(|l| l.contains("panicked") || l.to_lowercase().contains("error"))
            .take(5)
            .collect();
        format!(
            "its first errors:\n{}\nits last lines:\n{}",
            cause.join("\n"),
            lines[lines.len().saturating_sub(30)..].join("\n")
        )
    }

    async fn call(&self, method: &'static str, params: Value) -> Result<Value, String> {
        self.rpc
            .request::<Value, Value>(method, params)
            .await
            .map_err(|e| e.to_string().replace(&self.upstream, "<upstream>"))
    }

    /// A call that must succeed (a transport error is tried again twice).
    pub async fn rpc(&self, method: &'static str, params: Value) -> Value {
        let mut last = String::new();
        for _ in 0..3 {
            match self.call(method, params.clone()).await {
                Ok(v) => return v,
                Err(e) => last = e,
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("{method}: {last}; anvil's last lines:\n{}", self.tail())
    }

    /// A call that may fail: its error, not a panic.
    pub async fn try_rpc(&self, method: &'static str, params: Value) -> Result<Value, String> {
        self.call(method, params).await
    }

    /// Two providers over this anvil by two names: one node, so independence is not what these
    /// tests show (the endpoint's mock tests do); the endpoint's reads are.
    pub fn pair(&self) -> Pair<Http> {
        Pair {
            a: Http::new("a", &self.url("127.0.0.1")).unwrap(),
            b: Http::new("b", &self.url("localhost")).unwrap(),
        }
    }

    /// The one broadcast node: this anvil, over plain loopback http (amendment 0023). The payouts
    /// the suite sends go through here, not through the pair.
    pub fn broadcast(&self) -> Broadcast<Http> {
        Broadcast::new(vec![Http::new("anvil", &self.url("127.0.0.1")).unwrap()])
    }

    pub async fn mine(&self, n: u64) {
        self.rpc("anvil_mine", json!([format!("0x{n:x}")])).await;
    }

    /// Mines past finality: anvil's `finalized` tag is 64 blocks below its latest.
    pub async fn finalize(&self) {
        self.mine(65).await;
    }

    /// The number of the block anvil serves as `finalized`.
    pub async fn finalized(&self) -> u64 {
        let b = self
            .rpc("eth_getBlockByNumber", json!(["finalized", false]))
            .await;
        u64::from_str_radix(b["number"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }

    pub async fn number(&self) -> u64 {
        let v = self.rpc("eth_blockNumber", json!([])).await;
        u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }

    /// Block `number` as the node serves it: `(hash, timestamp)`.
    pub async fn block(&self, number: u64) -> (B256, u64) {
        let b = self
            .rpc(
                "eth_getBlockByNumber",
                json!([format!("0x{number:x}"), false]),
            )
            .await;
        let hash = b["hash"].as_str().unwrap().parse().unwrap();
        let time = u64::from_str_radix(
            b["timestamp"].as_str().unwrap().trim_start_matches("0x"),
            16,
        )
        .unwrap();
        (hash, time)
    }

    pub async fn send(&self, tx: Value) -> B256 {
        let h = self.rpc("eth_sendTransaction", json!([tx])).await;
        h.as_str().unwrap().parse().unwrap()
    }

    /// A receipt, once mined.
    pub async fn receipt(&self, tx: B256) -> Value {
        for _ in 0..40 {
            let r = self.rpc("eth_getTransactionReceipt", json!([tx])).await;
            if !r.is_null() {
                return r;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!("transaction {tx} was not mined");
    }

    /// Sets `holder`'s balance of the USDC token. The balance's storage slot is found, not assumed.
    pub async fn fund_usdc(&self, holder: Address, amount: u128) {
        let token: Address = USDC.parse().unwrap();
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
            if self.usdc_balance(holder).await == U256::from(amount) {
                return;
            }
            self.rpc("anvil_setStorageAt", json!([token, key, B256::ZERO]))
                .await;
        }
        panic!("no balance slot of {token} found for {holder}");
    }

    pub async fn usdc_balance(&self, holder: Address) -> U256 {
        let data = evm::balanceOfCall { owner: holder }.abi_encode();
        let out = self
            .rpc(
                "eth_call",
                json!([{"to": USDC, "data": format!("0x{}", hex::encode(data))}, "latest"]),
            )
            .await;
        U256::from_str_radix(out.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }
}

/// The vault `F-008`'s script deploys: Safe, Roles, emitter.
pub struct Vault {
    pub safe: Address,
    pub roles: Address,
    pub emitter: Address,
}

/// Deploys the vault with `contracts/evm/script/DeployVault.s.sol`, the delivery member being the
/// lane's address, two of three owners, the deployer none of them (F-008 NOTES, "Deployment").
fn deploy(anvil: &Anvil, lane: Address) -> Vault {
    let contracts = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/evm");
    let out = Command::new("forge")
        .current_dir(contracts)
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
        .env("NETWORK", POLYGON)
        .env("VAULT_OWNERS", OWNERS)
        .env("VAULT_THRESHOLD", "2")
        .env("DELIVERY_MEMBER", lane.to_string())
        .env("GUARDIAN", GUARDIAN)
        .env("VAULT_DELAY_COOLDOWN", "86400")
        .env("VAULT_DELAY_EXPIRATION", "604800")
        // Polygon's vault has a conversion venue; the script refuses a member without one and this
        // suite converts nothing, so it declares none... the script takes the member only where the
        // network's configuration has a venue, which Polygon's does.
        .env("CONVERSION_MEMBER", "0x9965507D1a55bcC2695C58ba16FB37d819B0A4dc")
        .output()
        .expect("forge is not on the PATH: the full-chain suite needs Foundry (anvil and forge) and does not skip");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "DeployVault failed (are the contracts/evm submodules checked out?): {}",
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

// ---------------------------------------------------------------------------------------------
// The signer: one process-shaped signer with both lanes and two callers.

struct Signing {
    /// The signer's socket and configuration, removed with the bed.
    _scratch: Scratch,
    evm: EvmLink,
    kreivo: KreivoLink,
    evm_lane: EvmLane,
    kreivo_lane: KreivoLane,
}

fn seed_of(id: &str) -> [u8; 32] {
    Sha256::digest(format!("bezent full-chain suite test key {id}")).into()
}

/// The two keys' file, and the public keys, before the signer exists: the vault's delivery member is
/// the lane's address, so the vault is deployed before the signer's configuration can name it.
fn write_keys() -> (Scratch, PathBuf, FileBackend) {
    let dir = temp_dir();
    let path = dir.join("keys.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "comment": "TEST KEYS, generated by the full-chain suite: Foundry's account 4 and a derived sr25519 seed",
            "environment": "test",
            "keys": [
                {"id": LANE_KEY, "scheme": "secp256k1-evm", "seed": LANE_SECRET},
                {"id": MINT_KEY, "scheme": "sr25519",
                 "seed": format!("0x{}", hex::encode(seed_of(MINT_KEY)))},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    let backend = FileBackend::load(&path).unwrap();
    (dir, path, backend)
}

async fn start_signer(
    admin: &PgPool,
    keys_path: PathBuf,
    backend: &FileBackend,
    vault: &Vault,
    chain: &FakeChain,
) -> Signing {
    let dir = temp_dir();
    let uid = my_uid();
    let caller = |name: &str, token: &str| Caller {
        name: name.into(),
        uid: Some(uid),
        dns_name: None,
        token_sha256: vec![hex::encode(Sha256::digest(token.as_bytes()))],
        allow: None,
    };
    let evm_caller = caller("evm-endpoint", EVM_TOKEN);
    let kreivo_caller = caller("kreivo-endpoint", KREIVO_TOKEN);
    let multisend = "0x9641d764fc13c8B624c04430C7356C1C7C8102e2";
    let config_path = dir.join("signer.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "socket": dir.join("s.sock"),
            "callers": [
                {"name": evm_caller.name, "uid": uid, "token_sha256": evm_caller.token_sha256},
                {"name": kreivo_caller.name, "uid": uid, "token_sha256": kreivo_caller.token_sha256},
            ],
            "networks": [
                {"id": KREIVO, "environment": "test", "issuing": true,
                 "contract_account": format!("0x{}", hex::encode(CONTRACT)),
                 "genesis": format!("0x{}", hex::encode(GENESIS))},
                {"id": POLYGON, "environment": "test", "chain_id": CHAIN_ID,
                 "cancel_max_fee_per_gas": "3000000000000",
                 "delivery": {"roles": vault.roles.to_string(), "multisend": multisend,
                              "emitter": vault.emitter.to_string(), "role": "bezent.delivery"}},
            ],
            "keys": {"backend": "file", "path": keys_path.clone()},
            "bindings": [
                {"key": MINT_KEY, "network": KREIVO, "purposes": ["mint", "refund-mint"],
                 "window_cap": "1000000000000", "window_seconds": 3600},
                {"key": LANE_KEY, "network": POLYGON, "purposes": ["payout", "cancel"],
                 "window_cap": "1000000000000", "window_seconds": 3600},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    let config = SignerConfig::load(&config_path).unwrap();
    let policy = config.policy(backend).unwrap();
    let signer_pool = login_pool(admin, "signer", bezent_store::SIGNER_ROLE, 4).await;
    bezent_store::login::check_login(&signer_pool, bezent_store::SIGNER_ROLE)
        .await
        .expect("the signer's login holds exactly its role");
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
        Arc::new(Callers::sharing_the_signer_uid_in_tests(&[
            evm_caller,
            kreivo_caller,
        ])),
        signer,
    ));
    let lane_public = backend.public_key(LANE_KEY).unwrap();
    let mint_public = backend.public_key(MINT_KEY).unwrap();
    let chain_account = chain.register_key(Scheme::Sr25519, &mint_public);
    Signing {
        _scratch: dir,
        evm: EvmLink {
            dialer: Dialer::Unix {
                socket: socket.clone(),
                uid,
            },
            token: EVM_TOKEN.into(),
        },
        kreivo: KreivoLink {
            dialer: Dialer::Unix { socket, uid },
            token: KREIVO_TOKEN.into(),
        },
        evm_lane: EvmLane {
            key: LANE_KEY.into(),
            address: evm::address_of(&lane_public).unwrap(),
            public_key: lane_public,
        },
        kreivo_lane: KreivoLane {
            key: MINT_KEY.into(),
            scheme: Scheme::Sr25519,
            public_key: mint_public,
            account: chain_account,
        },
    }
}

// ---------------------------------------------------------------------------------------------
// Logins.

/// A pool logged in as a login of its own that is a member of `role` and of nothing else, as each
/// service's login is: one pool, one login, never a switched role (#27, #46).
pub async fn login_pool(admin: &PgPool, who: &str, role: &'static str, max: u32) -> PgPool {
    bezent_store::login::connect_login(
        admin,
        &format!("e2e_{who}"),
        role,
        LOGIN_PASSWORD,
        PgPoolOptions::new().max_connections(max),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"))
}

// ---------------------------------------------------------------------------------------------
// The bed.

/// Everything running.
pub struct Bed {
    pub admin: PgPool,
    pub anvil: Anvil,
    pub vault: Vault,
    pub chain: FakeChain,
    pub pair: Pair<Http>,
    pub broadcast: Broadcast<Http>,
    pub evm_pool: PgPool,
    pub kreivo_pool: PgPool,
    pub core_pool: PgPool,
    pub ops: [PgPool; 2],
    signing: Signing,
    /// The test keys' directory, removed with the bed.
    _keys: Scratch,
    declarations: Declarations,
    core: Core,
    pub check: ReserveCheck<Recorded, LogPager>,
    ingress: std::net::SocketAddr,
    /// The Kreivo endpoint's start, and the follower's anchor.
    kreivo_start: u64,
}

impl Bed {
    pub async fn new(pool: PgPoolOptions, connect: PgConnectOptions) -> Bed {
        let admin = bezent_store::login::guard(pool)
            .connect_with(connect)
            .await
            .unwrap();
        let anvil = Anvil::start().await;

        // The delivery key and the vault. Foundry's account 4 carries an EIP-7702 delegation on
        // Polygon at the pinned block (anyone can sign one with a published key); bezent's delivery
        // key has none, so it is cleared here (F-009's fork tests do the same).
        let (keys_dir, keys_path, backend) = write_keys();
        let lane_address = evm::address_of(&backend.public_key(LANE_KEY).unwrap()).unwrap();
        anvil
            .rpc("anvil_setCode", json!([lane_address, "0x"]))
            .await;
        let vault = deploy(&anvil, lane_address);

        let chain = FakeChain::new(GENESIS, CONTRACT);
        let signing = start_signer(&admin, keys_path, &backend, &vault, &chain).await;
        // Kreivo endpoint issue #104: spec 134's own metadata, and the lane holding the
        // contract's delivery role.
        chain.set_metadata(
            std::fs::read(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../kreivo-endpoint/fixtures/kreivo-spec-134.metadata"
            ))
            .unwrap(),
        );
        chain.set_roles(kreivo::Roles {
            delivery: signing.kreivo_lane.account,
            pause: [0xee; 32],
            governance: [0xef; 32],
        });
        chain.advance(3);

        // The declarations, as the operator makes them.
        let vault_text = format!("{:#x}", vault.safe);
        let evm_login = format!("e2e_{}", bezent_store::EVM_ENDPOINT_ROLE);
        // The completion budget of the withdrawal route is a declaration, and here it is set from
        // the clocks: the Kreivo model's block times start in October 2025 and the fork's are
        // today's, a year ahead, while a deadline starts at the redemption's final block time and
        // is read against the fork's head. The budget puts the deadline two hours past the
        // fork's present, so a payout is on time until a scenario moves the fork's clock on.
        let fork_now = anvil.block(anvil.number().await).await.1;
        let kreivo_now = kreivo_time(&chain).await;
        let withdrawal_budget = (fork_now as i64 - kreivo_now as i64) + 7200;
        sqlx::raw_sql(&format!(
            "INSERT INTO networks (network, role, address_format, backend) VALUES \
               ('{KREIVO}', 'issuing', 'ss58', NULL), ('{POLYGON}', 'origin', 'evm', 'evm:{CHAIN_ID}'); \
             INSERT INTO assets (asset, precision, assurance_class, issuing_asset) VALUES \
               ('{ASSET}', 6, 'public', 'Here({LOCAL_ASSET})'); \
             INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
               ('{POLYGON}', '{USDC}', '{ASSET}', 6); \
             INSERT INTO reserves (asset, network, address) VALUES ('{ASSET}', '{POLYGON}', '{vault_text}'); \
             INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
                 window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
               ('{ASSET}', '{POLYGON}', 'deposit', 1, 1000000000, 10000000000, 86400, 100, '[]', 3600), \
               ('{ASSET}', '{POLYGON}', 'withdrawal', 1, 1000000000, 10000000000, 86400, 100, '[]', {withdrawal_budget}); \
             INSERT INTO evm_endpoint_networks (login, network) VALUES ('{evm_login}', '{POLYGON}'); \
             INSERT INTO requesters (requester) VALUES ('swaps'); \
             INSERT INTO requester_grants (requester, grant_kind) VALUES \
               ('swaps', 'deposit'), ('swaps', 'withdrawal'), ('swaps', 'refund');"
        ))
        .execute(&admin)
        .await
        .unwrap();
        let (id, secret) = REQUESTER.split_once('.').unwrap();
        sqlx::query(
            "INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
             VALUES ($1, 'swaps', $2)",
        )
        .bind(id)
        .bind(Sha256::digest(secret.as_bytes()).to_vec())
        .execute(&admin)
        .await
        .unwrap();

        // The services' logins, each checked as its service checks it at start-up.
        let evm_pool = login_pool(
            &admin,
            bezent_store::EVM_ENDPOINT_ROLE,
            bezent_store::EVM_ENDPOINT_ROLE,
            4,
        )
        .await;
        let kreivo_pool = login_pool(
            &admin,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            bezent_store::KREIVO_ENDPOINT_ROLE,
            4,
        )
        .await;
        let core_pool =
            login_pool(&admin, bezent_store::CORE_ROLE, bezent_store::CORE_ROLE, 6).await;
        let ingress_pool = login_pool(
            &admin,
            bezent_store::INGRESS_ROLE,
            bezent_store::INGRESS_ROLE,
            6,
        )
        .await;
        for (pool, role) in [
            (&evm_pool, bezent_store::EVM_ENDPOINT_ROLE),
            (&kreivo_pool, bezent_store::KREIVO_ENDPOINT_ROLE),
            (&core_pool, bezent_store::CORE_ROLE),
            (&ingress_pool, bezent_store::INGRESS_ROLE),
        ] {
            bezent_store::login::check_login(pool, role)
                .await
                .unwrap_or_else(|e| panic!("refusing to start: {e}"));
        }
        let ops = [
            login_pool(&admin, OPERATORS[0], bezent_store::OPERATOR_ROLE, 2).await,
            login_pool(&admin, OPERATORS[1], bezent_store::OPERATOR_ROLE, 2).await,
        ];

        // The EVM endpoint's start-up checks (evm-endpoint main.rs: AC-I4.1 on both providers, the
        // Roles modifier is this vault's, this login is mapped to this network), after finality has
        // passed the pinned block.
        anvil.finalize().await;
        let pair = anvil.pair();
        let broadcast = anvil.broadcast();
        let head = network::check(
            &pair,
            &Identity {
                chain_id: CHAIN_ID,
                pinned_number: PINNED_BLOCK,
                pinned_hash: PINNED_HASH.parse().unwrap(),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        network::check_vault(
            &pair,
            &head,
            vault.roles,
            vault.safe,
            signing.evm_lane.address,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        network::check_mapping(&evm_pool, POLYGON)
            .await
            .unwrap_or_else(|e| panic!("refusing to start: {e}"));
        let declarations = network::declarations(&evm_pool, POLYGON, vault.safe)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        // The ingress, served over HTTP on a bound port.
        let app = bezent_ingress::App::new(
            ingress_pool,
            bezent_ingress::Limits {
                max_in_flight: 64,
                log_page: 50,
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingress = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, bezent_ingress::router(app))
                .await
                .unwrap();
        });

        let core = Core::new(
            core_pool.clone(),
            Settings {
                retry_preparation_after: chrono::Duration::zero(),
                default_deadline: chrono::Duration::hours(1),
                ..Settings::default()
            },
        );
        let check = ReserveCheck::new(
            core_pool.clone(),
            Recorded::new(core_pool.clone()),
            LogPager,
        );

        let bed = Bed {
            admin,
            anvil,
            vault,
            chain,
            pair,
            broadcast,
            evm_pool,
            kreivo_pool,
            core_pool,
            ops,
            signing,
            _keys: keys_dir,
            declarations,
            core,
            check,
            ingress,
            kreivo_start: 1,
        };
        // The Kreivo endpoint's start-up: the anchor of its start block (kreivo-endpoint main.rs).
        bed.kreivo_follower()
            .start_anchor()
            .await
            .expect("the store")
            .unwrap_or_else(|e| panic!("{e}"));
        bed
    }

    // -----------------------------------------------------------------------------------------
    // The endpoints' loops.

    fn evm_follower(&self) -> EvmFollower<'_, Http> {
        EvmFollower {
            pair: &self.pair,
            pool: &self.evm_pool,
            network: POLYGON,
            watch: Watch {
                vault: self.vault.safe,
                emitter: self.vault.emitter,
                declarations: &self.declarations,
                convertibles: &[],
                // The account the endpoint signs from (main.rs: `delivery: checked.lane_address`).
                delivery: self.signing.evm_lane.address,
            },
            start_block: PINNED_BLOCK + 1,
            max_range: 500,
        }
    }

    fn contracts(&self) -> Contracts {
        Contracts {
            roles: self.vault.roles,
            multisend: "0x9641d764fc13c8B624c04430C7356C1C7C8102e2"
                .parse()
                .unwrap(),
            emitter: self.vault.emitter,
            delivery_role: evm::role_key("bezent.delivery").unwrap(),
        }
    }

    fn evm_submitter(&self) -> EvmSubmitter<'_, Http> {
        EvmSubmitter {
            pair: &self.pair,
            broadcast: &self.broadcast,
            pool: &self.evm_pool,
            network: POLYGON,
            chain_id: CHAIN_ID,
            contracts: self.contracts(),
            vault: self.vault.safe,
            declarations: &self.declarations,
            lane: &self.signing.evm_lane,
            signer: &self.signing.evm,
            gas: GasBounds {
                max_gas: 500_000,
                max_fee: 3_000_000_000_000,
                margin_percent: 130,
            },
        }
    }

    fn evm_prover(&self) -> EvmProver<'_, Http> {
        EvmProver {
            pair: &self.pair,
            pool: &self.evm_pool,
            network: POLYGON,
            lane: &self.signing.evm_lane,
            contracts: self.contracts(),
        }
    }

    fn kreivo_follower(&self) -> KreivoFollower<'_, FakeChain> {
        KreivoFollower {
            chain: &self.chain,
            pool: &self.kreivo_pool,
            network: KREIVO,
            contract: CONTRACT,
            start_block: self.kreivo_start,
            first_seq: 0,
            max_range: 4,
            max_walk: 10_000,
            rung: 5,
            concurrency: 4,
            skip: None,
        }
    }

    fn kreivo_submitter(&self) -> KreivoSubmitter<'_, FakeChain> {
        KreivoSubmitter {
            chain: &self.chain,
            pool: &self.kreivo_pool,
            network: KREIVO,
            genesis: GENESIS,
            contract: CONTRACT,
            lane: &self.signing.kreivo_lane,
            signer: &self.signing.kreivo,
            gate: KreivoGate::default(),
        }
    }

    fn kreivo_prover(&self) -> KreivoProver<'_, FakeChain> {
        KreivoProver {
            chain: &self.chain,
            pool: &self.kreivo_pool,
            network: KREIVO,
            contract: CONTRACT,
            lane: &self.signing.kreivo_lane,
            max_walk: 10_000,
        }
    }

    /// One pass of the EVM endpoint, in the order of its loop (`services/evm-endpoint/src/main.rs`,
    /// the body of `run`'s `loop`): follow to the final point, prepare every order, sign and submit
    /// every intent, resend, prove. Nothing is skipped to make a scenario work.
    pub async fn evm_pass(&self) {
        let follower = self.evm_follower();
        loop {
            match follower.step().await.expect("the store") {
                EvmStep::Read { .. } => continue,
                EvmStep::CaughtUp { .. } => break,
                other => panic!("the EVM follower stopped: {other:?}"),
            }
        }
        let submitter = self.evm_submitter();
        for id in submitter.orders_to_prepare().await.expect("the store") {
            submitter.prepare(id).await.expect("the store");
        }
        for id in submitter.intents_to_sign().await.expect("the store") {
            submitter.sign_and_submit(id).await.expect("the store");
        }
        submitter.resend().await.expect("the store");
        self.evm_prover().step().await.expect("the store");
    }

    /// One pass of the Kreivo endpoint, in the order of its loop (`services/kreivo-endpoint/src/main.rs`):
    /// follow to the finalized head, record the supply at it, prepare, sign and submit, resend,
    /// prove.
    pub async fn kreivo_pass(&self) {
        let follower = self.kreivo_follower();
        loop {
            match follower.step().await.expect("the store") {
                KreivoStep::Read { .. } => continue,
                KreivoStep::CaughtUp { .. } => break,
                other => panic!("the Kreivo follower stopped: {other:?}"),
            }
        }
        let supplies = Supplies {
            chain: &self.chain,
            pool: &self.kreivo_pool,
            network: KREIVO,
            contract: CONTRACT,
        };
        supplies.record().await.expect("the store");
        let finalized = Chain::finalized(&self.chain).await.expect("the model");
        let submitter = self.kreivo_submitter();
        for id in submitter
            .orders_to_prepare(&finalized)
            .await
            .expect("the store")
        {
            submitter.prepare(id).await.expect("the store");
        }
        for id in submitter
            .intents_to_sign(&finalized)
            .await
            .expect("the store")
        {
            submitter.sign_and_submit(id).await.expect("the store");
        }
        submitter.resend().await.expect("the store");
        self.kreivo_prover().step().await.expect("the store");
    }

    /// One cycle of the core, as its loop runs them (`services/core/src/main.rs`): the cycle, then
    /// the log's compromise alerts surfaced. The reserve check is [`Bed::check_reserve`].
    pub async fn core_pass(&self) {
        let report = self.core.cycle(Utc::now()).await.expect("cycle");
        assert_eq!(report.failed, 0, "a step of the core failed: {report:?}");
        bezent_core_service::signals::surface(&self.core_pool)
            .await
            .expect("the log");
    }

    /// The Kreivo model produces `blocks` blocks and finalizes them. The supply it reports at each
    /// block is the model's own count of what its events minted and destroyed, set before the block
    /// is produced: the harness's word (see the module documentation), never read from the store.
    pub async fn kreivo_blocks(&self, blocks: u64) {
        for _ in 0..blocks {
            let supply = self.kreivo_supply().await;
            self.chain.set_supply(LOCAL_ASSET, supply, supply);
            self.chain.produce();
        }
        self.chain.finalize(self.chain.best());
    }

    /// Every event of the contract in the Kreivo model, in order, as the model emitted it: the raw
    /// topics and data, before any decoding.
    pub async fn kreivo_event_logs(&self) -> Vec<(Vec<[u8; 32]>, Vec<u8>)> {
        let mut out = Vec::new();
        for number in 1..=self.chain.best() {
            let block = self.chain.block(number);
            let events = Chain::events(&self.chain, &block, &CONTRACT)
                .await
                .expect("the model");
            for e in events.emitted {
                out.push((e.topics, e.data));
            }
        }
        out
    }

    /// Every event of the contract in the Kreivo model, in order, decoded: what the model itself
    /// says happened, not what the store recorded.
    pub async fn kreivo_events(&self) -> Vec<ContractEvent> {
        self.kreivo_event_logs()
            .await
            .into_iter()
            .map(|(topics, data)| {
                kreivo::decode_event(&topics, &data).expect("the model's own event")
            })
            .collect()
    }

    /// The supply of the bridged asset on the Kreivo model: minted plus refund-minted minus
    /// redeemed, over the model's own events.
    pub async fn kreivo_supply(&self) -> u128 {
        let mut supply: i128 = 0;
        for number in 1..=self.chain.best() {
            let block = self.chain.block(number);
            let events = Chain::events(&self.chain, &block, &CONTRACT)
                .await
                .expect("the model");
            for e in events.emitted {
                match kreivo::decode_event(&e.topics, &e.data).expect("the model's own event") {
                    ContractEvent::Minted { asset, amount, .. }
                    | ContractEvent::RefundMinted { asset, amount, .. }
                        if asset == AssetId::Here(LOCAL_ASSET) =>
                    {
                        supply += i128::try_from(amount).unwrap();
                    }
                    ContractEvent::Redeemed { asset, amount, .. }
                        if asset == AssetId::Here(LOCAL_ASSET) =>
                    {
                        supply -= i128::try_from(amount).unwrap();
                    }
                    _ => {}
                }
            }
        }
        u128::try_from(supply).expect("the model's supply is never negative")
    }

    /// One turn of the whole bridge: both endpoints, the core, a Kreivo block, the endpoints and the
    /// core again; `mine` blocks on the fork between the two halves (65 reach finality).
    pub async fn turn(&self, mine: u64) {
        self.evm_pass().await;
        self.kreivo_pass().await;
        self.core_pass().await;
        self.kreivo_blocks(1).await;
        self.kreivo_pass().await;
        if mine > 0 {
            self.anvil.mine(mine).await;
        }
        self.evm_pass().await;
        self.core_pass().await;
    }

    /// Turns (mining `mine` blocks on the fork in each) until `done` holds, at most `limit`.
    pub async fn until<F, Fut>(&self, what: &str, limit: usize, mine: u64, mut done: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..limit {
            if done().await {
                return;
            }
            self.turn(mine).await;
        }
        if done().await {
            return;
        }
        panic!("{what}: not reached in {limit} turns");
    }

    // -----------------------------------------------------------------------------------------
    // The reserve check.

    /// One evaluation of the reserve equation at the latest final points, at the Kreivo model's
    /// clock (its blocks are dated October 2025, the fork's today: the check's clock is the model's,
    /// which is the one both final heads are not stale against, since a head in the future is not
    /// stale).
    pub async fn check_reserve(&self) -> bezent_core_service::reserve::check::Evaluation {
        let now = DateTime::from_timestamp_millis(
            i64::try_from(kreivo_time(&self.chain).await * 1000).unwrap(),
        )
        .unwrap();
        self.check.evaluate(ASSET, now).await.expect("the store")
    }

    // -----------------------------------------------------------------------------------------
    // Things a requester or a holder does.

    /// Calls the ingress over HTTP.
    pub async fn http(&self, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: ingress\r\nAuthorization: Bearer {REQUESTER}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        let mut stream = tokio::net::TcpStream::connect(self.ingress).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").expect("an HTTP response");
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("a status");
        // Chunked bodies: keep the JSON between the first '{' or '[' and the last '}' or ']'.
        let json_body = body
            .find(['{', '['])
            .and_then(|a| body.rfind(['}', ']']).map(|b| &body[a..=b]))
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or(Value::Null);
        (status, json_body)
    }

    /// The depositor sends `amount` of USDC to the vault on the fork; returns the transaction.
    pub async fn arrive(&self, amount: u128) -> B256 {
        let depositor: Address = DEPOSITOR.parse().unwrap();
        self.anvil.fund_usdc(depositor, amount).await;
        let data = evm::transferCall {
            to: self.vault.safe,
            amount: U256::from(amount),
        }
        .abi_encode();
        self.anvil
            .send(
                json!({"from": depositor, "to": USDC, "data": format!("0x{}", hex::encode(data))}),
            )
            .await
    }

    /// The coordinate of the transfer log in `receipt`, as a requester reads it from the chain: the
    /// block's number and hash, and the log's index in the block.
    pub fn coordinate_of(receipt: &Value) -> Value {
        let log = receipt["logs"]
            .as_array()
            .and_then(|l| l.first())
            .expect("the transfer's log");
        let index = u64::from_str_radix(
            log["logIndex"].as_str().unwrap().trim_start_matches("0x"),
            16,
        )
        .unwrap();
        let number = u64::from_str_radix(
            receipt["blockNumber"]
                .as_str()
                .unwrap()
                .trim_start_matches("0x"),
            16,
        )
        .unwrap();
        json!({
            "network": POLYGON,
            "block_number": number,
            "block_hash": receipt["blockHash"],
            "position": {"kind": "log", "index": index},
        })
    }

    /// The deposit request for `coordinate`, to Alice on the issuing network, refunding to the
    /// depositor on Polygon.
    pub fn deposit_body(reference: &str, coordinate: Value) -> Value {
        json!({
            "request_reference": reference,
            "coordinate": coordinate,
            "destination": {"network": KREIVO, "address": ALICE},
            "refund_destination": {"network": POLYGON, "address": DEPOSITOR.to_ascii_lowercase()},
        })
    }

    /// A holder's redemption of `amount` to `payout` on Polygon: the model emits the contract's
    /// `Redeemed`, which is all it can do (nothing is destroyed; the supply count follows the event).
    pub fn redeem(&self, reference: [u8; 32], amount: u128, payout: Address) {
        self.chain.emit(ContractEvent::Redeemed {
            seq: 0,
            reference,
            redeemer: ALICE_ACCOUNT,
            asset: AssetId::Here(LOCAL_ASSET),
            amount,
            backend: kreivo::Backend::Evm { chain_id: CHAIN_ID },
            payout: kreivo::Payout::Address(payout.0.0),
        });
    }

    // -----------------------------------------------------------------------------------------
    // Reading the store.

    pub async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.admin)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// The state and condition of the transfer with `bridge_reference`.
    pub async fn state_of(&self, id: i64) -> (String, Option<String>) {
        sqlx::query_as("SELECT state::text, condition::text FROM transfers WHERE id = $1")
            .bind(id)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    /// The transfer made from request `reference` (its request reference).
    pub async fn transfer_of_request(&self, reference: &str) -> i64 {
        sqlx::query_scalar("SELECT id FROM transfers WHERE request_reference = $1")
            .bind(reference)
            .fetch_one(&self.admin)
            .await
            .unwrap()
    }

    /// The only transfer of `kind`, if there is one: its id and state.
    pub async fn maybe_transfer(&self, kind: &str) -> Option<(i64, String)> {
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT id, state::text FROM transfers WHERE kind::text = $1 ORDER BY id",
        )
        .bind(kind)
        .fetch_all(&self.admin)
        .await
        .unwrap();
        assert!(rows.len() <= 1, "more than one {kind}: {rows:?}");
        rows.into_iter().next()
    }

    /// The vault's USDC balance on the fork.
    pub async fn reserve_balance(&self) -> u128 {
        u128::try_from(self.anvil.usdc_balance(self.vault.safe).await).unwrap()
    }

    pub fn evm_lane_address(&self) -> Address {
        self.signing.evm_lane.address
    }

    /// How many signatures the signer recorded.
    pub async fn signatures(&self) -> i64 {
        self.count("SELECT count(*) FROM signatures").await
    }

    /// How many distinct extrinsics were ever submitted to the Kreivo model (a broadcast repeated
    /// until the bytes are final counts once).
    pub async fn submitted_once(&self) -> usize {
        self.chain
            .submitted()
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
    }
}

/// The Kreivo model's clock: the time of its best block, in seconds.
pub async fn kreivo_time(chain: &FakeChain) -> u64 {
    bezent_kreivo_endpoint::chain::fake::time_of(chain.best()) / 1000
}

impl Bed {
    /// The reserve check at zero: an evaluation of the reserve equation at the latest final points
    /// that holds, with no difference between what the networks show and what the book holds, the
    /// networks showing `reserve` on Polygon and `supply` on Kreivo, nothing owed, surplus nil.
    pub async fn assert_balanced(&self, reserve: u128, supply: u128) {
        use bezent_core_service::reserve::check::Outcome;
        let e = self.check_reserve().await;
        let terms = |r: i128, s: i128| bezent_core_service::reserve::Terms {
            reserve: [(POLYGON.to_owned(), r)].into_iter().collect(),
            in_transit: Default::default(),
            supply: s,
            owed_mint: Default::default(),
            owed_payout: Default::default(),
            surplus: 0,
        };
        let want = terms(
            i128::try_from(reserve).unwrap(),
            i128::try_from(supply).unwrap(),
        );
        assert_eq!(e.outcome, Outcome::Holds, "{e:#?}");
        assert!(e.differences.is_empty(), "{e:#?}");
        // A network holding nothing may be listed at zero or not listed.
        let plain = |mut t: bezent_core_service::reserve::Terms| {
            t.reserve.retain(|_, v| *v != 0);
            t
        };
        assert_eq!(
            plain(e.chain.clone()),
            plain(want.clone()),
            "what the networks show: {e:#?}"
        );
        assert_eq!(
            plain(e.book.clone()),
            plain(want),
            "what the book holds: {e:#?}"
        );
    }
}

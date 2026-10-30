// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-evm-endpoint`: observes the reserve vault on one EVM origin network, prepares payouts
//! and proves non-execution; decides nothing (PLAN `DEC-3`, `DEC-8`).
//!
//! ```text
//! bezent-evm-endpoint <config.toml>           run
//! bezent-evm-endpoint --check <config.toml>   check the configuration, then exit
//! bezent-evm-endpoint health [<config.toml>]  the container's health check (issue #61)
//! ```
//!
//! `health` exits 0 only when the running endpoint's last pass reached the final point both providers
//! agree on, recently (`health.rs`). It reads a liveness file the pass rewrites and opens no
//! connection. The path defaults to the image's `/etc/bezent/evm-endpoint.toml`.
//!
//! At start-up, before anything is read or built (`AC-I4.1`, `DEC-12`): both providers must serve
//! the declared chain id, hold the pinned block at its pinned hash, and agree on the finalized head
//! they both stand on; the store must declare the network as an EVM origin network whose reserves
//! are the configured vault. Any mismatch exits with status 1.

use std::process::ExitCode;
use std::time::Duration;

use bezent_evm_endpoint::config::Config;
use bezent_evm_endpoint::network::{self, Identity};
use bezent_evm_endpoint::observe::{Follower, Step, Watch};
use bezent_evm_endpoint::pair::Pair;
use bezent_evm_endpoint::proof::Prover;
use bezent_evm_endpoint::rpc::Http;
use bezent_evm_endpoint::submit::{GasBounds, Lane, SignerLink, Submitter};

const NAME: &str = "bezent-evm-endpoint";

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [cmd, rest @ ..] = args.as_slice()
        && cmd == "health"
        && rest.len() <= 1
    {
        let path = bezent_evm_endpoint::health::config_path(rest.first());
        return bezent_health::report(NAME, bezent_evm_endpoint::health::check(&path));
    }
    let (check, path) = match args.as_slice() {
        [flag, path] if flag == "--check" => (true, path),
        [path] if !path.starts_with('-') => (false, path),
        _ => {
            eprintln!("usage: {NAME} [--check] <config.toml> | {NAME} health [<config.toml>]");
            return ExitCode::from(2);
        }
    };
    let config = match Config::from_file(std::path::Path::new(path)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(2);
        }
    };
    if check {
        let checked = match config.check() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{NAME}: {e}");
                return ExitCode::from(2);
            }
        };
        println!(
            "{NAME}: {} (chain {}), vault {}, lane {} ({}), pinned block {}: configuration checked",
            config.network,
            config.chain_id,
            checked.vault,
            config.lane.key,
            checked.lane_address,
            config.pinned_block
        );
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(run(config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            ExitCode::from(1)
        }
    }
}

async fn run(config: Config) -> Result<(), String> {
    // An earlier run's liveness file never passes for this run's (issue #61): removed before any start-up check or
    // connection.
    let heartbeat = bezent_evm_endpoint::health::heartbeat(&config);
    if let Err(e) = heartbeat.reset() {
        // Not fatal: a read-only file system must not keep a service from starting. A beat that
        // cannot be written is logged, and `health` reports it.
        tracing::warn!(error = %e.kind(), "the liveness file could not be removed; `health` may read an earlier run's");
    }
    let checked = config.check()?;
    // One login to Vault for the secrets read at start (issue #64); `--check` never gets here.
    let resolver = config.resolver()?;
    let urls = config.provider_urls(&resolver).await?;
    let pair = Pair {
        a: Http::new(&config.providers[0].name, &urls[0])?,
        b: Http::new(&config.providers[1].name, &urls[1])?,
    };
    // AC-I4.1, DEC-12: the declared network on both providers, or nothing starts.
    let head = network::check(
        &pair,
        &Identity {
            chain_id: config.chain_id,
            pinned_number: config.pinned_block,
            pinned_hash: checked.pinned_hash,
        },
    )
    .await?;
    tracing::info!(network = %config.network, head = head.number, "AC-I4.1: both providers serve the declared network");
    // AC-I4.1 (PR #39 review S4): the Roles modifier is this vault's, enabled; the lane runs no code.
    network::check_vault(
        &pair,
        &head,
        checked.contracts.roles,
        checked.vault,
        checked.lane_address,
    )
    .await?;
    tracing::info!(roles = %checked.contracts.roles, vault = %checked.vault, "AC-I4.1: the Roles modifier's avatar and target are the vault, which enables it");

    let pool = bezent_store::tls::connect(
        bezent_store::login::pool_options().max_connections(config.max_connections),
        &config.store_url(&resolver).await?,
    )
    .await
    .map_err(|e| format!("the store: {e}"))?;
    // The login is a member of bezent_evm_endpoint and of nothing else, and holds exactly its
    // grants (#46, REQ-AR-3).
    bezent_store::login::check_login(&pool, bezent_store::EVM_ENDPOINT_ROLE)
        .await
        .map_err(|e| format!("refusing to start: {e}"))?;
    let declarations = network::declarations(&pool, &config.network, checked.vault).await?;
    let lane = Lane {
        key: config.lane.key.clone(),
        public_key: checked.lane_public_key.clone(),
        address: checked.lane_address,
    };
    tracing::info!("{}", lane.declaration(&config.network));
    tracing::info!(
        "REQ-OB-1: on {} an observation is final when the network's `finalized` block, as both providers give it, is at or past its block with the same hash",
        config.network
    );
    let signer = SignerLink {
        socket: config.signer.socket.clone(),
        uid: config.signer.uid,
        token: config.signer_token(&resolver).await?.to_string(),
    };
    let follower = Follower {
        pair: &pair,
        pool: &pool,
        network: &config.network,
        watch: Watch {
            vault: checked.vault,
            emitter: checked.contracts.emitter,
            declarations: &declarations,
            convertibles: &checked.convertibles,
        },
        start_block: config.start_block,
        max_range: config.max_range,
    };
    let submitter = Submitter {
        pair: &pair,
        pool: &pool,
        network: &config.network,
        chain_id: config.chain_id,
        contracts: checked.contracts,
        vault: checked.vault,
        declarations: &declarations,
        lane: &lane,
        signer: &signer,
        gas: GasBounds {
            max_gas: config.gas.max_gas,
            max_fee: checked.max_fee_per_gas,
            margin_percent: config.gas.margin_percent,
        },
    };
    let prover = Prover {
        pair: &pair,
        pool: &pool,
        network: &config.network,
        lane: &lane,
        contracts: checked.contracts,
    };

    let mut tick = tokio::time::interval(Duration::from_millis(config.poll_ms));
    // Whether the pass before failed (the store, the prover): while it did, a walk's progress is not
    // health. False at start, so a first backlog walk beats.
    let mut last_pass_failed = false;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("stopping");
                return Ok(());
            }
            _ = tick.tick() => {}
        }
        let failed_before = last_pass_failed;
        let result: Result<bool, sqlx::Error> = async {
            let caught_up = loop {
                match follower.step().await? {
                    Step::Read { .. } => {
                        // A walk that moves is healthy (issue #61): blocks were read and recorded,
                        // so the follower advanced and the store answered, unless the pass before
                        // failed. A stuck one never gets here and goes stale.
                        if !failed_before {
                            bezent_evm_endpoint::health::beat(&heartbeat);
                        }
                    }
                    Step::CaughtUp { .. } => break true,
                    other => {
                        tracing::warn!(?other, "the follower stopped");
                        break false;
                    }
                }
            };
            for id in submitter.orders_to_prepare().await? {
                let outcome = submitter.prepare(id).await?;
                tracing::info!(order = id, ?outcome, "prepared");
                // Work done at the head is progress too, and a pass can take long (issue #61).
                if caught_up && !failed_before {
                    bezent_evm_endpoint::health::beat(&heartbeat);
                }
            }
            for id in submitter.intents_to_sign().await? {
                let outcome = submitter.sign_and_submit(id).await?;
                tracing::info!(intent = id, ?outcome, "signed");
                if caught_up && !failed_before {
                    bezent_evm_endpoint::health::beat(&heartbeat);
                }
            }
            submitter.resend().await?;
            prover.step().await?;
            Ok(caught_up)
        }
        .await;
        last_pass_failed = result.is_err();
        match result {
            // Healthy: at the final point both providers agree on, and the store answered all
            // the way through (issue #61).
            Ok(true) => bezent_evm_endpoint::health::beat(&heartbeat),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, "the store failed; trying again next cycle"),
        }
    }
}

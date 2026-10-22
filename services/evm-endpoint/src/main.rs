// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-evm-endpoint`: observes the reserve vault on one EVM origin network, prepares payouts
//! and proves non-execution; decides nothing (PLAN `DEC-3`, `DEC-8`).
//!
//! ```text
//! bezent-evm-endpoint <config.toml>           run
//! bezent-evm-endpoint --check <config.toml>   check the configuration, then exit
//! ```
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
    let (check, path) = match args.as_slice() {
        [flag, path] if flag == "--check" => (true, path),
        [path] if !path.starts_with('-') => (false, path),
        _ => {
            eprintln!("usage: {NAME} [--check] <config.toml>");
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

fn read_secret(path: &std::path::Path, what: &str) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .map_err(|e| format!("cannot read the {what} file {}: {e}", path.display()))
}

async fn run(config: Config) -> Result<(), String> {
    let checked = config.check()?;
    let provider = |i: usize| -> Result<Http, String> {
        let p = &config.providers[i];
        Http::new(&p.name, &Config::provider_url(p)?)
    };
    let pair = Pair {
        a: provider(0)?,
        b: provider(1)?,
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
        &read_secret(&config.database_url_file, "database URL")?,
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
        token: read_secret(&config.signer.token_file, "signer token")?,
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
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("stopping");
                return Ok(());
            }
            _ = tick.tick() => {}
        }
        let result: Result<(), sqlx::Error> = async {
            loop {
                match follower.step().await? {
                    Step::Read { .. } => continue,
                    Step::CaughtUp { .. } => break,
                    other => {
                        tracing::warn!(?other, "the follower stopped");
                        break;
                    }
                }
            }
            for id in submitter.orders_to_prepare().await? {
                let outcome = submitter.prepare(id).await?;
                tracing::info!(order = id, ?outcome, "prepared");
            }
            for id in submitter.intents_to_sign().await? {
                let outcome = submitter.sign_and_submit(id).await?;
                tracing::info!(intent = id, ?outcome, "signed");
            }
            submitter.resend().await?;
            prover.step().await?;
            Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = %e, "the store failed; trying again next cycle");
        }
    }
}

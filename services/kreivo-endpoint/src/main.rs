// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-kreivo-endpoint`: observes the issuing network and reports observations at coordinates;
//! decides nothing (DEC-3, DEC-5).
//!
//! ```text
//! bezent-kreivo-endpoint <config.toml>           run
//! bezent-kreivo-endpoint --check <config.toml>   check the configuration and the pinned chain spec, then exit
//! bezent-kreivo-endpoint health [<config.toml>]  the container's health check (issue #61)
//! ```
//!
//! `health` exits 0 only when the running endpoint's last pass reached the finalized head through the
//! light client, recently (`health.rs`). It reads a liveness file the pass rewrites and opens no
//! connection. The path defaults to the image's `/etc/bezent/kreivo-endpoint.toml`.
//!
//! At start-up, before anything is read or built (`AC-I4.1`): the pinned chain spec must describe
//! the declared network, its relay chain spec the declared relay chain with the declared
//! checkpoint, and its `para_id` the declared one (a parachain's identity is pinned with its relay
//! chain's), then the chain the light client follows must be it, and the contract at the declared
//! account must have the declared code hash (`contract_code_hash`, issue #24), and every history
//! source (`archive_rpc`, issue #49) must agree with the light client and serve proven state at
//! `start_block`. Any mismatch exits with status 1.

use std::process::ExitCode;

use bezent_kreivo_endpoint::config::Config;
use bezent_kreivo_endpoint::network;

const NAME: &str = "bezent-kreivo-endpoint";

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [cmd, rest @ ..] = args.as_slice()
        && cmd == "health"
        && rest.len() <= 1
    {
        let path = bezent_kreivo_endpoint::health::config_path(rest.first());
        return bezent_health::report(NAME, bezent_kreivo_endpoint::health::check(&path));
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
    let spec = match std::fs::read_to_string(&config.chain_spec) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "{NAME}: AC-I4.1: cannot read the chain spec {}: {e}",
                config.chain_spec.display()
            );
            return ExitCode::from(1);
        }
    };
    let declared = match config.declared_genesis() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = network::check_spec(declared, &spec) {
        eprintln!("{NAME}: {e}");
        return ExitCode::from(1);
    }
    let relay = match &config.relay_chain_spec {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "{NAME}: AC-I4.1: cannot read the relay chain spec {}: {e}",
                    p.display()
                );
                return ExitCode::from(1);
            }
        },
        None => None,
    };
    let declared_relay = match config.declared_relay() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = network::check_relay(&declared_relay, &spec, relay.as_deref()) {
        eprintln!("{NAME}: {e}");
        return ExitCode::from(1);
    }
    if check {
        println!(
            "{NAME}: configuration for {} checked; the pinned chain spec is the declared network (0x{}), and its relay chain the declared one",
            config.network,
            hex::encode(declared)
        );
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(run::run(config, spec, relay)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(feature = "light-client")]
mod run {
    use std::time::Duration;

    use bezent_kreivo_endpoint::chain::light::LightChain;
    use bezent_kreivo_endpoint::config::Config;
    use bezent_kreivo_endpoint::network;
    use bezent_kreivo_endpoint::observe::{Follower, Step, WALK_RUNG};
    use bezent_kreivo_endpoint::proof::Prover;
    use bezent_kreivo_endpoint::submit::{Lane, SignerLink, Submitter};
    use bezent_kreivo_endpoint::supply::{Recorded, Supplies};

    fn read_secret(path: &std::path::Path, what: &str) -> Result<String, String> {
        std::fs::read_to_string(path)
            .map(|s| s.trim().to_owned())
            .map_err(|e| format!("cannot read the {what} file {}: {e}", path.display()))
    }

    pub async fn run(config: Config, spec: String, relay: Option<String>) -> Result<(), String> {
        let declared = config.declared_genesis()?;
        let pool = bezent_store::tls::connect(
            bezent_store::login::pool_options().max_connections(config.max_connections),
            &read_secret(&config.database_url_file, "database URL")?,
        )
        .await
        .map_err(|e| format!("the store: {e}"))?;
        // The login is a member of bezent_kreivo_endpoint and of nothing else, and holds exactly
        // its grants (#46, REQ-AR-3).
        bezent_store::login::check_login(&pool, bezent_store::KREIVO_ENDPOINT_ROLE)
            .await
            .map_err(|e| format!("refusing to start: {e}"))?;
        // Issue #49: history comes from archive nodes, verified; the light client's peers keep
        // only recent state. Without one, a first start or a restart after downtime cannot read.
        if config.archive_rpc.is_empty() {
            return Err("issue #49: archive_rpc is empty; at least one history source is required; refusing to start".into());
        }
        let chain = LightChain::start(
            &spec,
            relay.as_deref(),
            &config.archive_rpc,
            config.light_client_depth,
            Duration::from_millis(config.archive_timeout_ms),
            config.archive_max_response_bytes,
        )
        .await
        .map_err(|e| e.to_string())?;
        // AC-I4.1: the chain the light client follows is the declared network, or nothing starts.
        network::check_chain(&chain, declared)
            .await
            .map_err(|e| e.to_string())?;
        // Issue #24: the account holds the declared build of the contract, or nothing starts.
        let contract = config.contract_account()?;
        network::check_contract(&chain, &contract, config.declared_code_hash()?)
            .await
            .map_err(|e| e.to_string())?;
        let follower = Follower {
            chain: &chain,
            pool: &pool,
            network: &config.network,
            contract,
            start_block: config.start_block,
            first_seq: config.first_seq,
            max_range: config.max_range,
            max_walk: config.max_walk,
            rung: WALK_RUNG.min(config.max_walk),
        };
        // Issue #49: start_block's hash is the finalized chain's, reached by parent hash from the
        // light client's head, not any source's word. A first start walks the backlog for it (its
        // anchors are kept, and the follower resumes from them); a failed walk is tried again
        // next start.
        let start_hash = match follower.start_anchor().await {
            Ok(Ok(hash)) => hash,
            Ok(Err(e)) => {
                return Err(format!(
                    "issue #49: the walk from the finalized head to start_block {} stopped: {e}; its anchors are kept and the next start resumes from them; refusing to start",
                    config.start_block
                ));
            }
            Err(e) => return Err(format!("the store: {e}")),
        };
        // Issue #49: every history source agrees with the light client and serves proven state at
        // the contract's instantiation, or nothing starts.
        if let Err(e) = chain
            .check_history(
                declared,
                config.start_block,
                start_hash,
                &contract,
                config.declared_code_hash()?,
            )
            .await
        {
            tracing::error!(reason = %e, "issue #49: the history sources failed the start-up check");
            return Err(e);
        }
        let (scheme, public_key, account) = config.lane_account()?;
        let lane = Lane {
            key: config.lane.key.clone(),
            scheme,
            public_key,
            account,
        };
        tracing::info!("{}", lane.declaration(&config.network));
        let signer = SignerLink {
            socket: config.signer.socket.clone(),
            uid: config.signer.uid,
            token: read_secret(&config.signer.token_file, "signer token")?,
        };
        let supplies = Supplies {
            chain: &chain,
            pool: &pool,
            network: &config.network,
            contract,
        };
        let submitter = Submitter {
            chain: &chain,
            pool: &pool,
            network: &config.network,
            genesis: declared,
            contract,
            lane: &lane,
            signer: &signer,
        };
        let prover = Prover {
            chain: &chain,
            pool: &pool,
            network: &config.network,
            contract,
            lane: &lane,
            max_walk: config.max_walk,
        };

        // An earlier run's liveness file never passes for this run's (issue #61).
        let heartbeat = bezent_kreivo_endpoint::health::heartbeat(&config);
        heartbeat
            .reset()
            .map_err(|e| format!("cannot remove the liveness file: {}", e.kind()))?;
        let mut lied_seen = 0u64;
        let mut tick = tokio::time::interval(Duration::from_millis(config.poll_ms));
        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("stopping");
                    return Ok(());
                }
                _ = tick.tick() => {}
            }
            let result: Result<bool, sqlx::Error> = async {
                // Caught up: the light client's finalized head is read, and every final block
                // before it (issue #61).
                let caught_up = loop {
                    match follower.step().await? {
                        Step::Read { .. } => {}
                        Step::CaughtUp { .. } => break true,
                        _ => break false,
                    }
                };
                let counts = chain.history_counts();
                let lied: u64 = counts.iter().map(|c| c.lied).sum();
                if lied > lied_seen {
                    lied_seen = lied;
                    tracing::error!(sources = ?counts, "REQ-RQ-7: history sources have answered what does not verify (counts since start-up)");
                }
                // Issue #42: the supply of every bridged asset at the finalized head, for the
                // reserve check (REQ-BK-8, NFR-5).
                match supplies.record().await? {
                    Recorded::Unavailable(why) => {
                        tracing::warn!(reason = %why, "REQ-BK-8: supply not read at the finalized head")
                    }
                    Recorded::Conflict(n) => {
                        tracing::warn!(block = n, "REQ-OB-2: another hash is recorded at the finalized head's height")
                    }
                    other => tracing::debug!(?other, "supply"),
                }
                if let Ok(finalized) = bezent_kreivo_endpoint::chain::Chain::finalized(&chain).await
                {
                    for id in submitter.orders_to_prepare(&finalized).await? {
                        let outcome = submitter.prepare(id).await?;
                        tracing::info!(order = id, ?outcome, "prepared");
                    }
                    for id in submitter.intents_to_sign(&finalized).await? {
                        let outcome = submitter.sign_and_submit(id).await?;
                        tracing::info!(intent = id, ?outcome, "signed");
                    }
                }
                submitter.resend().await?;
                prover.step().await?;
                Ok(caught_up)
            }
            .await;
            match result {
                // Healthy: at the finalized head, and the store answered all the way through.
                Ok(true) => {
                    if let Err(e) = heartbeat.beat() {
                        tracing::error!(error = %e.kind(), "the liveness file could not be written; `health` will report this endpoint unhealthy");
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "the store failed; trying again next cycle"),
            }
        }
    }
}

#[cfg(not(feature = "light-client"))]
mod run {
    use bezent_kreivo_endpoint::config::Config;

    pub async fn run(_config: Config, _spec: String, _relay: Option<String>) -> Result<(), String> {
        Err("built without the light client (feature `light-client`); only --check works".into())
    }
}

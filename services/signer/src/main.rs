// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-signer`: signs only intents the core recorded and holds no state (DEC-3, REQ-AR-3).
//!
//! `bezent-signer <config-path>` serves the link; `bezent-signer --check <config-path>` loads and
//! checks the configuration and the keys, prints the key ids and public keys, and exits. Keys are
//! read from the file the configuration names, never from the environment (`REQ-KY-1`).
//!
//! `bezent-signer health [<config-path>]` is the container's health check (issue #61): exit 0 only
//! when the running signer has reached its store recently and its socket is there. It reads the
//! configuration and a liveness file, loads no key and connects to nothing (`health.rs`). The path
//! defaults to the image's `/etc/bezent-signer/signer.json`.
//!
//! At start, before any key is read, the process forbids its own core dumps (`REQ-CU-1`). To
//! serve, it refuses a caller that runs as its own user id (`REQ-CU-1`, `REQ-AR-3`), and a store
//! login that can act as anything but `bezent_signer` (`REQ-AR-3`).

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use bezent_signer::config::{Config, KeySource, Policy};
use bezent_signer::custody::{Custody, Mixed};
use bezent_signer::keys::{FileBackend, KeyBackend};
use bezent_signer::kms::{AwsKms, KmsBackend};
use bezent_signer::link::{self, Callers};
use bezent_signer::log::Log;
use bezent_signer::process;
use bezent_signer::signer::{Signer, check_login, pool_options};

const NAME: &str = "bezent-signer";

async fn load(path: &Path) -> Result<(Config, Custody, Policy), String> {
    let config = Config::load(path).map_err(|e| e.to_string())?;
    let backend = match &config.keys {
        KeySource::File { path } => {
            Custody::File(FileBackend::load(path).map_err(|e| e.to_string())?)
        }
        KeySource::Kms {
            environment,
            region,
            endpoint,
            keys,
            vault,
        } => {
            let api = AwsKms::new(region, endpoint.as_deref()).await;
            let kms = KmsBackend::connect(api, *environment, keys)
                .await
                .map_err(|e| e.to_string())?;
            let vault = match vault {
                Some(source) => Some(
                    bezent_signer::vault::load(source, *environment)
                        .await
                        .map_err(|e| e.to_string())?,
                ),
                None => None,
            };
            Custody::Kms(Mixed::new(kms, vault).map_err(|e| e.to_string())?)
        }
    };
    let policy = config.policy(&backend).map_err(|e| e.to_string())?;
    Ok((config, backend, policy))
}

async fn run(config: Config, backend: Custody, policy: Policy, log: Log) -> Result<(), String> {
    let url_file = config
        .database_url_file
        .as_ref()
        .ok_or("configuration refused: database_url_file is required to serve")?;
    // The core and the signer never run as one user (REQ-CU-1, REQ-AR-3).
    let callers =
        Arc::new(Callers::new(&config.callers, process::euid()).map_err(|e| e.to_string())?);
    let url = zeroize::Zeroizing::new(
        std::fs::read_to_string(url_file)
            .map_err(|e| format!("cannot read {}: {}", url_file.display(), e.kind()))?,
    );
    let pool = bezent_store::tls::connect(pool_options(), url.trim())
        .await
        .map_err(|e| format!("store: {e}"))?;
    // The keys' environment is held to the networks the store declares, as the core does
    // (REQ-KY-6, AC-I4.2, issue #25).
    let production = bezent_store::environment::production_networks(&pool)
        .await
        .map_err(|e| format!("store: {e}"))?;
    bezent_signer::config::confirm_store_environment(
        &config.networks,
        backend.environment(),
        &production,
    )
    .map_err(|e| e.to_string())?;
    // The login acts as bezent_signer, and can act as nothing else (REQ-AR-3).
    check_login(&pool).await?;
    let listener = link::bind(&config.socket).map_err(|e| e.to_string())?;
    // Healthy from here: the login is checked and the socket bound (issue #61). An earlier run's
    // liveness file is removed first.
    let heartbeat = bezent_signer::health::heartbeat(&config);
    if let Err(e) = heartbeat.reset() {
        // Not fatal: a read-only file system must not keep a service from starting. A beat that
        // cannot be written is logged, and `health` reports it.
        log.line(format_args!(
            "health: the liveness file could not be removed ({})",
            e.kind()
        ));
    }
    let pulse = tokio::spawn(bezent_signer::health::pulse(
        pool.clone(),
        heartbeat,
        log.clone(),
    ));
    log.line(format_args!(
        "{} serving {} with keys {:?}",
        env!("CARGO_PKG_VERSION"),
        config.socket.display(),
        backend
    ));
    let signer = Arc::new(Signer::new(pool, policy, backend, log));
    let result = tokio::select! {
        served = link::serve(listener, callers, signer) => served.map_err(|e| e.to_string()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    pulse.abort();
    result
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    // Before anything is loaded: `health` touches no key, no KMS and no Vault (issue #61).
    if let [cmd, rest @ ..] = args.as_slice()
        && cmd == "health"
        && rest.len() <= 1
    {
        let path = bezent_signer::health::config_path(rest.first());
        return bezent_health::report(NAME, bezent_signer::health::check(&path));
    }
    let (check, path) = match args.as_slice() {
        [flag, path] if flag == "--check" => (true, path),
        [path] => (false, path),
        _ => {
            eprintln!("usage: {NAME} [--check] <config-path> | {NAME} health [<config-path>]");
            return ExitCode::from(2);
        }
    };
    let log = Log::stderr();
    // Before any key is read (REQ-CU-1). Linux must succeed; elsewhere it is best effort.
    if let Err(e) = process::forbid_core_dumps() {
        log.line(format_args!("{e}"));
        if cfg!(target_os = "linux") {
            return ExitCode::FAILURE;
        }
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            log.line(format_args!("runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let (config, backend, policy) = match runtime.block_on(load(Path::new(path))) {
        Ok(loaded) => loaded,
        Err(e) => {
            log.line(format_args!("{e}"));
            return ExitCode::FAILURE;
        }
    };
    if check {
        log.line(format_args!(
            "configuration {} is valid; keys {:?}",
            Path::new(path).display(),
            backend
        ));
        return ExitCode::SUCCESS;
    }
    match runtime.block_on(run(config, backend, policy, log.clone())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log.line(format_args!("{e}"));
            ExitCode::FAILURE
        }
    }
}

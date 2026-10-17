// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-sidecar`: bezent for other repositories' end-to-end harnesses.
//!
//! One process runs the ingress, the core's cycle and a test signer, against an external
//! PostgreSQL that the harness provides (a CI service container). It is the shape of aledgent's
//! sidecar: `bezent-sidecar serve` (the default) and `bezent-sidecar health` (the container's
//! health check).
//!
//! **TEST only.** It signs with the published test keys of `services/signer/fixtures`, serves
//! declarations for the test networks `kreivo-local` and `polygon-amoy`, and refuses to start
//! unless `BEZENT_SIDECAR_ENVIRONMENT=test` and every network in the store is one of those two
//! (`REQ-KY-6`: a test key never meets a production network). It is not a deployment: the real
//! services run as their own images, each under its own store login and its own user.
//!
//! In this process the logins are still separate. The migrations run as the owner login named by
//! `BEZENT_SIDECAR_DATABASE_URL`; the ingress, the core, the signer and the webhook dispatcher
//! each connect as a login of their own, a member of their own role and of nothing else
//! (`bezent_ingress`, `bezent_core`, `bezent_signer`, `bezent_webhooks`), created here with a
//! random password that lives in memory only, and each is checked as its service checks it at
//! start-up (#46). With the chain-event driver enabled ([`bezent_sidecar::driver`], PLAN `DEC-19`),
//! it writes as two more: one holding `bezent_kreivo_endpoint`, one holding `bezent_evm_endpoint`.
//!
//! Environment:
//!
//! | Variable | |
//! |---|---|
//! | `BEZENT_SIDECAR_ENVIRONMENT` | required, exactly `test` |
//! | `BEZENT_SIDECAR_DATABASE_URL` or `…_FILE` | required: the owner login (superuser or `CREATEROLE`) of an empty or earlier sidecar database |
//! | `BEZENT_SIDECAR_REQUESTERS` | required: `name=<credential id>.<secret>;…`; each is granted deposit, withdrawal and refund |
//! | `BEZENT_SIDECAR_LISTEN` | `0.0.0.0:8080` |
//! | `BEZENT_SIDECAR_SIGNER_SOCKET` | `/tmp/bezent-signer.sock` |
//! | `BEZENT_SIDECAR_SIGNER_TOKEN` | the signer link's token; a random one, in memory, if unset |
//! | `BEZENT_SIDECAR_CYCLE_SECONDS` | `2` |
//! | `BEZENT_SIDECAR_DRIVER_TOKEN` | binds the chain-event driver; at least 16 characters; unset, there is none |
//! | `BEZENT_SIDECAR_DRIVER_LISTEN` | the driver's address, `0.0.0.0:8090` |
//! | `BEZENT_SIDECAR_WEBHOOK_KEY_FILE` | a file (mode 600) holding the webhook key, 64 hexadecimal characters; never the key itself in the environment (`REQ-KY-1`; PR #55 review S3). A random one, in memory, if unset (then callback URLs and signing secrets do not survive a restart) |
//!
//! | `BEZENT_SIDECAR_WEBHOOK_HOSTS` | comma-separated hosts (the harness's) that webhooks may reach over plain `http` and at loopback or private addresses; none by default |
//!
//! Webhooks (issue #2) are on: `https` to global addresses, plain `http` and internal addresses only
//! to `BEZENT_SIDECAR_WEBHOOK_HOSTS`, never link-local or metadata addresses, and never this
//! process's own listeners (PR #55 review M1).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use bezent_sidecar::driver::{self, Driver, SignerLink};
use bezent_sidecar::setup::{
    LOGIN_CORE, LOGIN_DRIVER_EVM, LOGIN_DRIVER_KREIVO, LOGIN_INGRESS, LOGIN_SIGNER, LOGIN_WEBHOOKS,
    as_login, login, only_test_networks, random_secret, seed, signer_config,
};
use bezent_signer::link::{self, Callers};
use bezent_signer::log::Log;
use bezent_signer::signer::{Signer, pool_options};
use bezent_store::login::{check_login, pool_options as store_pool};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::net::TcpListener;

const NAME: &str = "bezent-sidecar";

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// What the environment says.
struct Settings {
    database_url: String,
    listen: SocketAddr,
    requesters: Vec<(String, String, String)>,
    signer_socket: PathBuf,
    signer_token: Option<String>,
    cycle: Duration,
    driver: Option<(String, SocketAddr)>,
    webhook_key: bezent_ingress::webhooks::SealingKey,
    webhook_hosts: Vec<String>,
}

fn listen_address() -> Result<SocketAddr, String> {
    var("BEZENT_SIDECAR_LISTEN")
        .unwrap_or_else(|| "0.0.0.0:8080".to_owned())
        .parse()
        .map_err(|_| "BEZENT_SIDECAR_LISTEN is not an address and port".to_owned())
}

/// The chain-event driver's token and address, when its token is set.
fn driver_settings() -> Result<Option<(String, SocketAddr)>, String> {
    let Some(token) = var("BEZENT_SIDECAR_DRIVER_TOKEN") else {
        return Ok(None);
    };
    if token.len() < driver::MIN_TOKEN {
        return Err(format!(
            "BEZENT_SIDECAR_DRIVER_TOKEN is at least {} characters",
            driver::MIN_TOKEN
        ));
    }
    let listen = var("BEZENT_SIDECAR_DRIVER_LISTEN")
        .unwrap_or_else(|| "0.0.0.0:8090".to_owned())
        .parse()
        .map_err(|_| "BEZENT_SIDECAR_DRIVER_LISTEN is not an address and port".to_owned())?;
    Ok(Some((token, listen)))
}

fn settings() -> Result<Settings, String> {
    if var("BEZENT_SIDECAR_ENVIRONMENT").as_deref() != Some("test") {
        return Err("BEZENT_SIDECAR_ENVIRONMENT must be exactly `test`: the sidecar signs with published test keys and runs on test networks only (REQ-KY-6)".to_owned());
    }
    let database_url = match (
        var("BEZENT_SIDECAR_DATABASE_URL"),
        var("BEZENT_SIDECAR_DATABASE_URL_FILE"),
    ) {
        (Some(url), None) => url,
        (None, Some(file)) => std::fs::read_to_string(&file)
            .map_err(|e| format!("cannot read {file}: {}", e.kind()))?
            .trim()
            .to_owned(),
        _ => {
            return Err(
                "set one of BEZENT_SIDECAR_DATABASE_URL, BEZENT_SIDECAR_DATABASE_URL_FILE"
                    .to_owned(),
            );
        }
    };
    let mut requesters = Vec::new();
    let list = var("BEZENT_SIDECAR_REQUESTERS")
        .ok_or("BEZENT_SIDECAR_REQUESTERS is required: name=<credential id>.<secret>;…")?;
    for entry in list.split(';').filter(|e| !e.is_empty()) {
        let parsed = entry
            .split_once('=')
            .and_then(|(name, token)| Some((name, token.split_once('.')?)))
            .filter(|(name, (id, secret))| {
                !name.is_empty() && !id.is_empty() && secret.len() >= 16
            });
        let Some((name, (id, secret))) = parsed else {
            return Err("BEZENT_SIDECAR_REQUESTERS: each entry is name=<credential id>.<secret>, the secret at least 16 characters".to_owned());
        };
        requesters.push((name.to_owned(), id.to_owned(), secret.to_owned()));
    }
    if requesters.is_empty() {
        return Err("BEZENT_SIDECAR_REQUESTERS names no requester".to_owned());
    }
    let cycle = var("BEZENT_SIDECAR_CYCLE_SECONDS")
        .map(|s| s.parse::<u64>().ok().filter(|n| *n > 0))
        .unwrap_or(Some(2))
        .ok_or("BEZENT_SIDECAR_CYCLE_SECONDS is not a positive number")?;
    Ok(Settings {
        database_url,
        listen: listen_address()?,
        requesters,
        signer_socket: var("BEZENT_SIDECAR_SIGNER_SOCKET")
            .unwrap_or_else(|| "/tmp/bezent-signer.sock".to_owned())
            .into(),
        signer_token: var("BEZENT_SIDECAR_SIGNER_TOKEN"),
        cycle: Duration::from_secs(cycle),
        driver: driver_settings()?,
        webhook_key: match var("BEZENT_SIDECAR_WEBHOOK_KEY_FILE") {
            Some(path) => bezent_ingress::webhooks::SealingKey::from_file(path.as_ref())?,
            None => bezent_ingress::webhooks::SealingKey::random(),
        },
        webhook_hosts: var("BEZENT_SIDECAR_WEBHOOK_HOSTS")
            .map(|v| {
                v.split(',')
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

async fn serve() -> Result<(), String> {
    let settings = settings()?;
    let base: PgConnectOptions = settings
        .database_url
        .parse::<PgConnectOptions>()
        .map_err(|_| "the database URL is not a PostgreSQL URL".to_owned())?;

    // The owner: migrations, declarations, logins. Not kept past start-up.
    let owner = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(base.clone())
        .await
        .map_err(|e| format!("cannot connect to the store: {e}"))?;
    // The guard comes first: on an existing store nothing is migrated, seeded or re-keyed until
    // its networks are known to be the two test ones (an empty store has none).
    only_test_networks(&owner).await?;
    bezent_store::MIGRATOR
        .run(&owner)
        .await
        .map_err(|e| format!("migrations: {e}"))?;
    only_test_networks(&owner).await?;
    seed(&owner, &settings.requesters).await?;
    let (ingress_pw, core_pw, signer_pw) = (random_secret()?, random_secret()?, random_secret()?);
    let webhooks_pw = random_secret()?;
    login(
        &owner,
        LOGIN_INGRESS,
        bezent_store::INGRESS_ROLE,
        &ingress_pw,
    )
    .await?;
    login(&owner, LOGIN_CORE, bezent_store::CORE_ROLE, &core_pw).await?;
    login(&owner, LOGIN_SIGNER, bezent_store::SIGNER_ROLE, &signer_pw).await?;
    login(
        &owner,
        LOGIN_WEBHOOKS,
        bezent_store::WEBHOOKS_ROLE,
        &webhooks_pw,
    )
    .await?;
    // The driver's logins, one per endpoint role it writes as, only when it is enabled.
    let driver_logins = match &settings.driver {
        Some(_) => {
            let (kreivo_pw, evm_pw) = (random_secret()?, random_secret()?);
            login(
                &owner,
                LOGIN_DRIVER_KREIVO,
                bezent_store::KREIVO_ENDPOINT_ROLE,
                &kreivo_pw,
            )
            .await?;
            login(
                &owner,
                LOGIN_DRIVER_EVM,
                bezent_store::EVM_ENDPOINT_ROLE,
                &evm_pw,
            )
            .await?;
            Some((kreivo_pw, evm_pw))
        }
        None => None,
    };
    owner.close().await;

    // One pool per login, never a switched role (#27); each login checked as its service checks
    // it at start-up (#46).
    let ingress_pool = store_pool()
        .max_connections(8)
        .acquire_timeout(Duration::from_millis(1000))
        .connect_with(as_login(&base, LOGIN_INGRESS, &ingress_pw))
        .await
        .map_err(|e| format!("ingress login: {e}"))?;
    check_login(&ingress_pool, bezent_store::INGRESS_ROLE).await?;
    let webhook_pool = store_pool()
        .max_connections(2)
        .connect_with(as_login(&base, LOGIN_WEBHOOKS, &webhooks_pw))
        .await
        .map_err(|e| format!("webhook dispatcher login: {e}"))?;
    check_login(&webhook_pool, bezent_store::WEBHOOKS_ROLE).await?;
    let core_pool = store_pool()
        .max_connections(2)
        .connect_with(as_login(&base, LOGIN_CORE, &core_pw))
        .await
        .map_err(|e| format!("core login: {e}"))?;
    check_login(&core_pool, bezent_store::CORE_ROLE).await?;
    let signer_pool = pool_options()
        .connect_with(as_login(&base, LOGIN_SIGNER, &signer_pw))
        .await
        .map_err(|e| format!("signer login: {e}"))?;
    check_login(&signer_pool, bezent_store::SIGNER_ROLE).await?;

    // The signer: the published test keys, on a socket in this container. Its link's token is the
    // one given, or a random one that only this process knows.
    let link_token = match &settings.signer_token {
        Some(t) => t.clone(),
        None => random_secret()?,
    };
    let (config, backend) = signer_config(&link_token)?;
    let policy = config.policy(&backend).map_err(|e| e.to_string())?;
    let callers = Arc::new(Callers::sharing_the_signer_uid_in_tests(&config.callers));
    let listener = link::bind(&settings.signer_socket).map_err(|e| e.to_string())?;
    let signer = Arc::new(Signer::new(signer_pool, policy, backend, Log::stderr()));
    let signer_task = tokio::spawn(async move {
        if let Err(e) = link::serve(listener, callers, signer).await {
            tracing::error!(error = %e, "the signer link ended");
        }
    });

    // The chain-event driver (TEST only, PLAN DEC-19): bound only with its token set, and only on
    // a store of test networks; its stand-in endpoints prepare and sign every cycle.
    let mut driver_tasks = Vec::new();
    if let (Some((token, listen)), Some((kreivo_pw, evm_pw))) = (&settings.driver, &driver_logins) {
        let kreivo_pool = store_pool()
            .max_connections(2)
            .connect_with(as_login(&base, LOGIN_DRIVER_KREIVO, kreivo_pw))
            .await
            .map_err(|e| format!("driver (kreivo endpoint) login: {e}"))?;
        check_login(&kreivo_pool, bezent_store::KREIVO_ENDPOINT_ROLE).await?;
        let evm_pool = store_pool()
            .max_connections(2)
            .connect_with(as_login(&base, LOGIN_DRIVER_EVM, evm_pw))
            .await
            .map_err(|e| format!("driver (evm endpoint) login: {e}"))?;
        check_login(&evm_pool, bezent_store::EVM_ENDPOINT_ROLE).await?;
        let driver = Arc::new(Driver::new(
            kreivo_pool,
            evm_pool,
            core_pool.clone(),
            token,
            SignerLink {
                socket: settings.signer_socket.clone(),
                token: link_token.clone(),
                uid: bezent_signer::process::euid(),
            },
        )?);
        let bound = driver::start(driver.clone(), *listen).await?;
        tracing::warn!(listen = %bound.local_addr, "TEST ONLY: the chain-event driver is bound (PLAN DEC-19)");
        driver_tasks.push(tokio::spawn(async move {
            if let Err(e) = bound.serve(std::future::pending()).await {
                tracing::error!(error = %e, "the chain-event driver stopped serving");
            }
        }));
        let cycle = settings.cycle;
        driver_tasks.push(tokio::spawn(async move {
            let mut tick = tokio::time::interval(cycle);
            loop {
                tick.tick().await;
                if let Err(e) = driver.tend().await {
                    tracing::warn!(error = %e, "the driver's endpoints did not prepare or sign this cycle");
                }
            }
        }));
    }

    // The core.
    let core =
        bezent_core_service::Core::new(core_pool.clone(), bezent_core_service::Settings::default());
    let cycle = settings.cycle;
    // Each cycle re-checks the networks; one that is not a test network stops the process.
    let stop = Arc::new(tokio::sync::Notify::new());
    let refusal: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
    let (stop_tx, refusal_tx) = (stop.clone(), refusal.clone());
    let watch_pool = core_pool.clone();
    let core_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(cycle);
        loop {
            tick.tick().await;
            if let Err(e) = only_test_networks(&watch_pool).await {
                tracing::error!(error = %e, "stopping: the store no longer holds only test networks");
                if let Ok(mut slot) = refusal_tx.lock() {
                    *slot = Some(e);
                }
                stop_tx.notify_one();
                return;
            }
            if let Err(e) = core.cycle(chrono::Utc::now()).await {
                tracing::warn!(error = %e, "cycle failed; the next one starts from the store");
            }
        }
    });

    // The ingress, last: its health answer means the store is migrated, seeded and logged in.
    let listener = TcpListener::bind(settings.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", settings.listen))?;
    tracing::info!(listen = %settings.listen, "bezent-sidecar serving (TEST networks only)");
    // Webhooks (issue #2): https to global addresses; plain http and internal addresses only for
    // the hosts BEZENT_SIDECAR_WEBHOOK_HOSTS names (the store holds only test networks), and
    // never this process's own listeners (PR #55 review M1).
    let mut hooks = bezent_ingress::webhooks::Webhooks::new(settings.webhook_key.clone());
    hooks.allow_hosts = settings.webhook_hosts.clone();
    let mut own = vec![settings.listen];
    own.extend(settings.driver.as_ref().map(|(_, listen)| *listen));
    for addr in own {
        let port = addr.port();
        let mut entries = vec![
            format!("127.0.0.1:{port}"),
            format!("[::1]:{port}"),
            format!("localhost:{port}"),
        ];
        if !addr.ip().is_unspecified() {
            entries.push(addr.to_string());
        }
        for entry in entries {
            hooks.deny.push(entry.parse()?);
        }
    }
    let (_webhooks_stop, webhooks_stopped) = tokio::sync::watch::channel(false);
    let webhook_task = tokio::spawn(
        bezent_ingress::webhooks::Dispatcher::new(webhook_pool, hooks.clone())?
            .run(webhooks_stopped),
    );
    let app = bezent_ingress::App::new(
        ingress_pool,
        bezent_ingress::Limits {
            max_in_flight: 256,
            log_page: 500,
        },
    )
    .with_webhooks(hooks);
    let stopped = async move {
        tokio::select! {
            () = shutdown_signal() => {}
            () = stop.notified() => {}
        }
    };
    let served = bezent_ingress::run(listener, app, stopped).await;
    signer_task.abort();
    core_task.abort();
    webhook_task.abort();
    for t in driver_tasks {
        t.abort();
    }
    let _ = std::fs::remove_file(&settings.signer_socket);
    if let Some(refused) = refusal.lock().ok().and_then(|mut slot| slot.take()) {
        return Err(refused);
    }
    served.map_err(|e| format!("serving failed: {e}"))
}

/// The container's health check: the ingress answers `/health` on the loopback.
async fn health() -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let port = listen_address()?.port();
    let probe = async {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
        stream
            .write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .await?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    };
    let reply = tokio::time::timeout(Duration::from_secs(2), probe)
        .await
        .map_err(|_| "no answer in 2 s".to_owned())?
        .map_err(|e| e.to_string())?;
    if reply.starts_with(b"HTTP/1.1 200") || reply.starts_with(b"HTTP/1.0 200") {
        Ok(())
    } else {
        Err("the ingress did not answer 200".to_owned())
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] | ["serve"] => {
            tracing_subscriber::fmt().with_target(false).init();
            if let Err(e) = bezent_signer::process::forbid_core_dumps() {
                tracing::warn!("{e}");
            }
            serve().await
        }
        ["health"] => health().await,
        _ => {
            eprintln!("usage: {NAME} [serve] | {NAME} health");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolves on SIGINT or, on Unix, SIGTERM.
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
}

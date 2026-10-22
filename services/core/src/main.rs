// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-core`: the core of PLAN `DEC-3` (`F-003`).
//!
//! - `bezent-core <config-path>` runs a cycle every `cycle_seconds` until SIGINT or SIGTERM, then
//!   finishes the cycle in progress and exits. Every cycle reads durable state only, so a restart
//!   picks up where the last one left off (`REQ-AR-1`). After each cycle it runs the reserve check
//!   (each asset evaluated once a minute, from the readings the endpoints recorded at final heads,
//!   `NFR-5`, `REQ-BK-8`) and publishes each asset's daily statement, signed by the signer process
//!   (`REQ-PR-1`; PLAN `DEC-16`; issue #42), paging one that is overdue. `[statement]` is required;
//!   only a test deployment may leave it out, by saying so (PR #50 review M2). A test deployment
//!   is confirmed by the store, never by the file alone: `environment = "test"` and the published
//!   test statement key are refused, at start and every cycle, while the store declares any
//!   production network (PR #50 re-check M-1; the sidecar's rule, `bezent_store::environment`).
//! - `bezent-core --check <config-path>` reads and checks the configuration, and exits.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use bezent_core_service::reserve::check::{LogPager, ReserveCheck, SystemClock};
use bezent_core_service::reserve::readings::Recorded;
use bezent_core_service::reserve::statement::{DirectoryStore, SignerProcess, Statements};

const NAME: &str = "bezent-core";

/// The most one cycle's statement publishing may take (PR #50 review S1).
const STATEMENT_BUDGET: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (check, path) = match args.as_slice() {
        [flag, path] if flag == "--check" => (true, path),
        [path] => (false, path),
        _ => {
            eprintln!("usage: {NAME} <config-path> | {NAME} --check <config-path>");
            return ExitCode::from(2);
        }
    };
    let config = match bezent_core_service::Config::from_file(&PathBuf::from(path)) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(2);
        }
    };
    if check {
        println!("{NAME}: configuration ok: {config:?}");
        return ExitCode::SUCCESS;
    }

    tracing_subscriber::fmt().with_target(false).init();
    let pool = match bezent_store::tls::connect(
        bezent_store::login::pool_options().max_connections(config.max_connections),
        &config.database_url,
    )
    .await
    {
        Ok(pool) => pool,
        Err(e) => {
            eprintln!("{NAME}: store: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The login is a member of bezent_core and of nothing else, and holds exactly the core's
    // grants (#46, REQ-AR-3).
    if let Err(e) = bezent_store::login::check_login(&pool, bezent_store::CORE_ROLE).await {
        eprintln!("{NAME}: refusing to start: {e}");
        return ExitCode::from(2);
    }
    // Test mode is the store's to confirm, never the file's alone (PR #50 re-check M-1).
    if let Err(e) = confirm_environment(&config, &pool).await {
        eprintln!("{NAME}: refusing to start: {e}");
        return ExitCode::from(2);
    }
    let statements = match config.statement.as_ref().and_then(|s| s.configured()) {
        Some(s) => {
            let token = match std::fs::read_to_string(&s.signer_token_file) {
                Ok(t) => t.trim().to_owned(),
                Err(e) => {
                    eprintln!(
                        "{NAME}: cannot read the signer token file {}: {}",
                        s.signer_token_file.display(),
                        e.kind()
                    );
                    return ExitCode::from(2);
                }
            };
            let public_key = s.public_key_bytes().unwrap_or_default();
            match SignerProcess::new(
                pool.clone(),
                s.signer_socket.clone(),
                s.signer_uid,
                token,
                public_key,
            ) {
                Ok(signer) => Some(Statements::new(
                    pool.clone(),
                    signer,
                    DirectoryStore::new(s.directory.clone()),
                )),
                Err(e) => {
                    eprintln!("{NAME}: statement key: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        // Only `statement = "disabled-test-only"` with `environment = "test"` gets here: the
        // configuration refuses anything else without a statement (PR #50 review M2).
        None => {
            tracing::error!(
                environment = ?config.environment,
                "REQ-PR-1: TEST ONLY: statement = \"disabled-test-only\": the reserve check runs, and NO daily statement is published"
            );
            None
        }
    };
    // The reserve check reads what the endpoints recorded at final heads; it pages through the
    // service's log (issue #42; NFR-5, REQ-BK-8, REQ-IC-2).
    let check = ReserveCheck::new(pool.clone(), Recorded::new(pool.clone()), LogPager);
    let check_pool = pool.clone();
    let core = bezent_core_service::Core::new(pool, config.settings());
    let mut tick = tokio::time::interval(Duration::from_secs(config.cycle_seconds));
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            _ = tick.tick() => {
                // A production network declared while test-only settings are in force stops the
                // core, as it stops the sidecar (PR #50 re-check M-1; REQ-KY-6).
                if config.needs_test_store()
                    && let Err(e) = confirm_environment(&config, &check_pool).await
                {
                    tracing::error!(error = %e, "stopping: the store no longer holds only test networks");
                    return ExitCode::from(2);
                }
                match core.cycle(chrono::Utc::now()).await {
                    Ok(report) => tracing::debug!(?report, "cycle"),
                    Err(e) => tracing::warn!(error = %e, "cycle failed; the next one starts from the store"),
                }
                match check.tick(&SystemClock).await {
                    Ok(evaluations) => {
                        for e in &evaluations {
                            tracing::debug!(asset = %e.asset, outcome = ?e.outcome, "reserve check");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "NFR-5: the reserve check could not list the assets; tried again next cycle"),
                }
                if let Some(statements) = &statements {
                    // Bounded: a signer or a store that never answers does not freeze the cycle
                    // and the reserve check; a statement it keeps from being published is paged
                    // overdue all the same (PR #50 review M2, S1).
                    let now = chrono::Utc::now();
                    match tokio::time::timeout(STATEMENT_BUDGET, statements.publish_due(now)).await {
                        Ok(Ok(published)) => {
                            for p in &published {
                                tracing::info!(asset = %p.asset, sequence = p.sequence, name = %p.name, "REQ-PR-1: statement published");
                            }
                        }
                        Ok(Err(e)) => tracing::error!(error = %e, "REQ-PR-1: statements not published; tried again next cycle"),
                        Err(_) => {
                            tracing::error!(seconds = STATEMENT_BUDGET.as_secs(), "REQ-PR-1: publishing statements did not finish in time; tried again next cycle");
                            if let Err(e) = statements.page_overdue(now).await {
                                tracing::error!(error = %e, "REQ-PR-1: overdue statements could not be checked");
                            }
                        }
                    }
                }
            }
        }
    }
    ExitCode::SUCCESS
}

/// Confirms `config` against the networks the store declares ([`bezent_store::environment`], the
/// sidecar's rule; PR #50 re-check M-1).
async fn confirm_environment(
    config: &bezent_core_service::Config,
    pool: &sqlx::PgPool,
) -> Result<(), String> {
    let production = bezent_store::environment::production_networks(pool)
        .await
        .map_err(|e| format!("store: {e}"))?;
    config.confirm_environment(&production)
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

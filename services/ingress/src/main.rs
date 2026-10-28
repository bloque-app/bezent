// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-ingress`: the request and query surfaces (DEC-3, DEC-15; F-002).
//!
//! - `bezent-ingress <config-path>` serves until SIGINT or SIGTERM, then finishes the requests in
//!   flight and exits.
//! - `bezent-ingress openapi` prints the OpenAPI document.
//! - `bezent-ingress health [<config-path>]` is the container's health check: exit 0 only when the
//!   running instance answers `/health` (the store reached, the login checked), within 5 s
//!   (`health.rs`; issue #61). The path defaults to the image's `/etc/bezent/ingress.toml`.

use std::path::PathBuf;
use std::process::ExitCode;

const NAME: &str = "bezent-ingress";

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(arg), second) = (args.next(), args.next()) else {
        eprintln!("usage: {NAME} <config-path> | {NAME} openapi | {NAME} health [<config-path>]");
        return ExitCode::from(2);
    };
    if arg == "health" && args.next().is_none() {
        let path = second.map_or_else(
            || PathBuf::from(bezent_ingress::health::DEFAULT_CONFIG),
            PathBuf::from,
        );
        return bezent_health::report(NAME, bezent_ingress::health::check(&path).await);
    }
    if second.is_some() {
        eprintln!("usage: {NAME} <config-path> | {NAME} openapi | {NAME} health [<config-path>]");
        return ExitCode::from(2);
    }
    if arg == "openapi" {
        print!("{}", bezent_ingress::openapi::document());
        return ExitCode::SUCCESS;
    }

    tracing_subscriber::fmt().with_target(false).init();
    let config = match bezent_ingress::Config::from_file(&PathBuf::from(arg)) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            return ExitCode::from(2);
        }
    };
    match bezent_ingress::serve(config, shutdown_signal()).await {
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

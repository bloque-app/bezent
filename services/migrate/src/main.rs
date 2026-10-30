// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-migrate`: brings a bezent store to this release's migrations, as the migration owner.
//!
//! ```text
//! bezent-migrate (--config <path> | --url-file <path> | --url-env <NAME>) [--check] [--login <name>]...
//!                [--grants enforce|report|skip] [--lock-timeout <seconds>]
//! ```
//!
//! - The owner's database URL is built from a configuration file (`--config`: the store's host, and
//!   the owner's login and password as files or fields in Vault, which the process logs in to
//!   itself; see `owner.rs`), or read from the file (`--url-file`: a mounted secret) or from the
//!   environment variable the command line names (`--url-env`, discouraged: the credential stays
//!   in the process environment, visible to `docker inspect` and `/proc/<pid>/environ`). It is
//!   never printed. `sslmode` and `sslrootcert` in it drive TLS (`bezent_store::tls`).
//! - Without `--check` it applies the pending migrations and prints `applied <version>` for each.
//!   It refuses, changing nothing, a store whose history diverges: a version this release lacks, a
//!   different checksum, a migration that did not finish, or tables with no history.
//! - With `--check` it changes nothing and exits non-zero unless the store is exactly at this
//!   release.
//! - Once the store is at the release it runs the catalogue-wide grants check (`--grants`, default
//!   `enforce`): `--login` names each service and operator login this deployment made, so that they
//!   are not reported as strays.
//! - It refuses a login that is a member of a service's role.
//! - A wait for a lock longer than `--lock-timeout` seconds (default 300; 0 waits for ever) fails the
//!   run with a clear error, instead of queueing behind a lock another session holds.
//! - **Not all-or-nothing.** Each migration commits on its own. A failure at version N leaves the
//!   versions before N applied, exits 1 and names N. The services refuse a store that is not exactly
//!   at their release, so they stay down until a later run applies N and what follows. The runner
//!   is re-runnable: the next run resumes at N and does not repeat the versions already applied.
//!
//! Exit status: 0 done (or current); 1 failure; 2 usage; 3 `--check`: migrations pending; 4 the
//! history diverges; 5 the grants check found something (and `--grants enforce`).

use std::process::ExitCode;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;

const NAME: &str = "bezent-migrate";
const DEFAULT_LOCK_TIMEOUT_SECS: u64 = 300;
const USAGE: &str = "usage: bezent-migrate (--config <path> | --url-file <path> | --url-env <NAME>) \
                     [--check] [--login <name>]... [--grants enforce|report|skip] \
                     [--lock-timeout <seconds>]
  --config <path>          a TOML file naming the store and the owner's login and password as files or
                           Vault fields ([vault] and [store]); the URL is built in the process
  --url-file <path>        read the owner's database URL from a file (preferred: mount it as a secret)
  --url-env <NAME>         read it from an environment variable (discouraged: the credential stays in
                           the process environment)
  --check                  change nothing; exit 3 if migrations are pending, 4 if the history diverges
  --login <name>           a service or operator login of this deployment (not a stray); repeatable
  --grants <mode>          enforce (default), report or skip the catalogue-wide grants check
  --lock-timeout <seconds> fail instead of waiting longer than this for a lock (default 300; 0 = wait)
Each migration commits on its own: a failure at version N leaves the versions before N applied, exits 1
and names N. The services refuse a store that is not exactly at their release, so they stay down until
a later run applies N and what follows; the runner is re-runnable and resumes at N.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grants {
    Enforce,
    Report,
    Skip,
}

struct Args {
    config: Option<String>,
    url_file: Option<String>,
    url_env: Option<String>,
    check: bool,
    logins: Vec<String>,
    grants: Grants,
    lock_timeout: Duration,
}

fn parse(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut parsed = Args {
        config: None,
        url_file: None,
        url_env: None,
        check: false,
        logins: Vec::new(),
        grants: Grants::Enforce,
        lock_timeout: Duration::from_secs(DEFAULT_LOCK_TIMEOUT_SECS),
    };
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{arg} needs {what}"));
        match arg.as_str() {
            "--check" => parsed.check = true,
            "--config" => parsed.config = Some(value("a path")?),
            "--url-file" => parsed.url_file = Some(value("a path")?),
            "--url-env" => parsed.url_env = Some(value("a variable name")?),
            "--login" => parsed.logins.push(value("a login name")?),
            "--lock-timeout" => {
                let seconds = value("a number of seconds")?;
                parsed.lock_timeout =
                    Duration::from_secs(seconds.parse().map_err(|_| {
                        format!("--lock-timeout takes whole seconds, not {seconds}")
                    })?);
            }
            "--grants" => {
                parsed.grants = match value("enforce, report or skip")?.as_str() {
                    "enforce" => Grants::Enforce,
                    "report" => Grants::Report,
                    "skip" => Grants::Skip,
                    other => {
                        return Err(format!(
                            "--grants takes enforce, report or skip, not {other}"
                        ));
                    }
                }
            }
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let named = [&parsed.config, &parsed.url_file, &parsed.url_env]
        .iter()
        .filter(|a| a.is_some())
        .count();
    if named != 1 {
        return Err(
            "name the database URL with exactly one of --config, --url-file and --url-env"
                .to_owned(),
        );
    }
    Ok(parsed)
}

/// The URL, from where the command line says. The error never holds the URL.
async fn read_url(args: &Args) -> Result<bezent_vault::Zeroizing<String>, String> {
    let raw = match (&args.config, &args.url_file, &args.url_env) {
        (Some(path), _, _) => {
            // Reads the file and builds the URL, logging in to Vault if a secret is there.
            let config = bezent_migrate::owner::OwnerConfig::from_file(std::path::Path::new(path))?;
            config.url().await?
        }
        (_, Some(path), _) => bezent_vault::Zeroizing::new(
            std::fs::read_to_string(path)
                .map_err(|e| format!("cannot read the URL file {path}: {}", e.kind()))?,
        ),
        (_, _, Some(name)) => bezent_vault::Zeroizing::new(
            std::env::var(name)
                .map_err(|_| format!("the environment variable {name} is not set"))?,
        ),
        _ => unreachable!("parse() requires one"),
    };
    let url = raw.trim();
    if url.is_empty() {
        return Err("the database URL is empty".to_owned());
    }
    Ok(bezent_vault::Zeroizing::new(url.to_owned()))
}

fn list(versions: &[i64]) -> String {
    versions
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{NAME}: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{NAME}: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<ExitCode, String> {
    let url = read_url(&args).await?;
    let pool = bezent_store::tls::connect(
        bezent_migrate::with_lock_timeout(
            PgPoolOptions::new()
                .max_connections(3)
                .acquire_timeout(Duration::from_secs(15)),
            args.lock_timeout,
        ),
        &url,
    )
    .await
    .map_err(|e| format!("cannot connect to the store: {e}"))?;
    drop(url);
    bezent_migrate::refuse_service_login(&pool).await?;
    let migrator = &bezent_store::MIGRATOR;

    if args.check {
        let status = bezent_migrate::status(&pool, migrator).await?;
        if !status.divergences.is_empty() {
            for d in &status.divergences {
                eprintln!("{NAME}: diverges: {d}");
            }
            return Ok(ExitCode::from(4));
        }
        if !status.pending.is_empty() {
            println!("pending {}", list(&status.pending));
            return Ok(ExitCode::from(3));
        }
        println!("current at {}", status.applied.last().copied().unwrap_or(0));
    } else {
        match bezent_migrate::apply(&pool, migrator).await {
            Ok(applied) => {
                for version in &applied {
                    println!("applied {version}");
                }
                if applied.is_empty() {
                    println!("nothing to apply");
                }
            }
            Err(e) => {
                eprintln!("{NAME}: {e}");
                let diverges = e.starts_with("refusing to migrate a store whose history diverges");
                return Ok(ExitCode::from(if diverges { 4 } else { 1 }));
            }
        }
    }

    if args.grants == Grants::Skip {
        return Ok(ExitCode::SUCCESS);
    }
    let mut conn = pool.acquire().await.map_err(|e| format!("store: {e}"))?;
    let findings = bezent_migrate::catalogue_findings(&mut conn, &args.logins).await?;
    if findings.is_empty() {
        println!("grants: as the release means them");
        return Ok(ExitCode::SUCCESS);
    }
    for f in &findings {
        eprintln!("{NAME}: grants: {f}");
    }
    Ok(if args.grants == Grants::Enforce {
        ExitCode::from(5)
    } else {
        ExitCode::SUCCESS
    })
}

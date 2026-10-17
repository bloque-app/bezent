// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the sidecar sets up before it serves: the test-network rule, the declarations, the logins
//! and the test signer's configuration. Moved out of `main.rs` so the chain-event driver
//! ([`crate::driver`]) and its tests share them.

use bezent_signer::config::Config as SignerConfig;
use bezent_signer::keys::FileBackend;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;

/// The issuing test network of the signer's published fixture.
pub const ISSUING: &str = "kreivo-local";
/// The origin test network of the signer's published fixture.
pub const ORIGIN: &str = "polygon-amoy";
/// The origin network's chain id, as the signer's fixture declares it.
pub const ORIGIN_CHAIN_ID: u64 = 80002;

/// The only networks this binary serves: two of the store's test networks.
pub const SERVED: &[&str] = &[ISSUING, ORIGIN];

/// The bridged asset the sidecar declares.
pub const ASSET: &str = "bUSDC";
/// Its identity on the issuing network, as the on-chain component names it (PLAN `DEC-7`).
pub const ISSUING_ASSET: &str = "Here(101)";
/// The origin token admitted for it on [`ORIGIN`], as the store spells it: an EVM address, as
/// every EVM network's admitted token is (the signer pays it out by that address, audit must-fix
/// 2). A store an earlier sidecar seeded with the token `usdc` keeps it beside this one.
pub const TOKEN: &str = "0x41e94eb019c0762f9bfcf9fb1e58725bfb0e7582";
/// The reserve address on [`ORIGIN`].
pub const RESERVE: &str = "0x00000000000000000000000000000000000000aa";

/// The logins this process creates, one per role.
pub const LOGIN_INGRESS: &str = "bezent_sidecar_ingress";
/// The core's login.
pub const LOGIN_CORE: &str = "bezent_sidecar_core";
/// The signer's login.
pub const LOGIN_SIGNER: &str = "bezent_sidecar_signer";
/// The webhook dispatcher's login: a member of `bezent_webhooks` only (PR #55 review M2).
pub const LOGIN_WEBHOOKS: &str = "bezent_sidecar_webhooks";
/// The chain-event driver's login on the issuing network: a member of `bezent_kreivo_endpoint`
/// only. Created only when the driver is enabled.
pub const LOGIN_DRIVER_KREIVO: &str = "bezent_sidecar_driver_kreivo";
/// The chain-event driver's login on the origin network: a member of `bezent_evm_endpoint` only.
/// Created only when the driver is enabled.
pub const LOGIN_DRIVER_EVM: &str = "bezent_sidecar_driver_evm";

/// The signer's published test configuration.
pub const SIGNER_CONFIG: &[u8] = include_bytes!("../../signer/fixtures/signer.json");
/// The signer's published test keys.
pub const TEST_KEYS: &[u8] = include_bytes!("../../signer/fixtures/test-keys.json");

/// 32 random bytes as hex, from the operating system.
pub fn random_secret() -> Result<String, String> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("no randomness: {e}"))?;
    Ok(hex::encode(bytes))
}

/// Creates (or re-keys) a login that is a member of `role` and of nothing else; a membership an
/// earlier sidecar granted (the retired `bezent_service`) is revoked (#46). The password is hex, so
/// it needs no quoting.
pub async fn login(owner: &PgPool, name: &str, role: &str, password: &str) -> Result<(), String> {
    bezent_store::login::ensure_login(owner, name, role, password).await
}

/// `base` as login `name`.
pub fn as_login(base: &PgConnectOptions, name: &str, password: &str) -> PgConnectOptions {
    bezent_store::login::as_login(base, name, password)
}

/// The declarations the test networks need, and the requesters. Idempotent. A store seeded by an
/// earlier sidecar gains what this one adds (the asset's issuing identity, the origin network's
/// backend, the withdrawal route), and nothing it holds is changed.
pub async fn seed(owner: &PgPool, requesters: &[(String, String, String)]) -> Result<(), String> {
    let mut tx = owner.begin().await.map_err(|e| format!("store: {e}"))?;
    sqlx::raw_sql(&format!(
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('{ISSUING}', 'issuing', 'ss58'), ('{ORIGIN}', 'origin', 'evm') ON CONFLICT DO NOTHING; \
         UPDATE networks SET backend = 'evm:{ORIGIN_CHAIN_ID}' \
           WHERE network = '{ORIGIN}' AND backend IS NULL; \
         INSERT INTO assets (asset, precision, assurance_class) VALUES ('{ASSET}', 6, 'public') \
           ON CONFLICT DO NOTHING; \
         UPDATE assets SET issuing_asset = '{ISSUING_ASSET}' \
           WHERE asset = '{ASSET}' AND issuing_asset IS NULL; \
         INSERT INTO admitted_tokens (network, token, asset, precision) VALUES \
           ('{ORIGIN}', '{TOKEN}', '{ASSET}', 6) ON CONFLICT DO NOTHING; \
         INSERT INTO reserves (asset, network, address) VALUES \
           ('{ASSET}', '{ORIGIN}', '{RESERVE}') ON CONFLICT DO NOTHING; \
         INSERT INTO routes (asset, network, direction, minimum, maximum, window_limit, \
             window_seconds, capacity, cutoffs, completion_budget_seconds) VALUES \
           ('{ASSET}', '{ORIGIN}', 'deposit', 100, 1000000000, 10000000000, 86400, 100, '[]', 3600), \
           ('{ASSET}', '{ORIGIN}', 'withdrawal', 100, 1000000000, 10000000000, 86400, 100, '[]', 3600) \
           ON CONFLICT DO NOTHING;"
    ))
    .execute(&mut *tx)
    .await
    .map_err(|e| format!("cannot seed the declarations: {e}"))?;
    for (name, id, secret) in requesters {
        let digest = Sha256::digest(secret.as_bytes()).to_vec();
        sqlx::query("INSERT INTO requesters (requester) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(name)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("cannot seed requester {name}: {e}"))?;
        for grant in ["deposit", "withdrawal", "refund"] {
            sqlx::query(
                "INSERT INTO requester_grants (requester, grant_kind) VALUES ($1, $2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(name)
            .bind(grant)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("cannot seed the grants of {name}: {e}"))?;
        }
        sqlx::query(
            "INSERT INTO requester_credentials (credential_id, requester, secret_sha256) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(name)
        .bind(digest)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("cannot seed the credential of {name}: {e}"))?;
    }
    tx.commit().await.map_err(|e| format!("store: {e}"))
}

/// Refuses a store that declares any network but the two test networks (`REQ-KY-6`). The rule is
/// the store's own ([`bezent_store::environment`]), which the core shares (PR #50 re-check M-1).
/// Read-only: it changes nothing.
pub async fn only_test_networks(pool: &PgPool) -> Result<(), String> {
    let others = bezent_store::environment::networks_outside(pool, SERVED)
        .await
        .map_err(|e| format!("store: {e}"))?;
    if others.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "refusing to start: the store declares networks that are not the sidecar's test networks ({}); the sidecar never runs on a production store (REQ-KY-6)",
            others.join(", ")
        ))
    }
}

/// The signer's configuration: the published test fixture, with the caller made this process and
/// its token `token`.
pub fn signer_config(token: &str) -> Result<(SignerConfig, FileBackend), String> {
    let mut config = SignerConfig::parse(SIGNER_CONFIG).map_err(|e| e.to_string())?;
    let backend = FileBackend::parse(TEST_KEYS).map_err(|e| e.to_string())?;
    for caller in &mut config.callers {
        caller.uid = bezent_signer::process::euid();
        caller.token_sha256 = vec![hex::encode(Sha256::digest(token.as_bytes()))];
    }
    Ok((config, backend))
}

#[cfg(test)]
mod tests {
    /// PR #50 re-check M-1: the sidecar and the core share one rule for a test store; the two
    /// networks the sidecar serves are test networks under it.
    #[test]
    fn m_1_req_ky_6_the_sidecars_networks_are_test_networks_under_the_shared_rule() {
        for n in super::SERVED {
            assert!(bezent_store::environment::is_test_network(n), "{n}");
        }
    }
}

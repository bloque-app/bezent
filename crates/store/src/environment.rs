// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether a store is a test one, from the networks it declares (`REQ-KY-6`: a test key never
//! meets a production network; PR #50 re-check M-1).
//!
//! A configuration line alone never makes a deployment a test one: a store is a test store only
//! when every network it declares is one of [`TEST_NETWORKS`]. The sidecar (which serves two of
//! them) and the core (which allows its test-only settings only on a test store) both read the
//! store through [`networks_outside`], so there is one rule.

use sqlx::PgPool;

/// The test networks: the public test networks bezent's test deployments run on, and the ids the
/// test harness declares. Never the id of a production network: the anvil forks of the harness
/// stand for production networks under their production ids, so they are not here.
pub const TEST_NETWORKS: &[&str] = &[
    // The sidecar's issuing and origin test networks (services/signer/fixtures).
    "kreivo-local",
    "polygon-amoy",
    // The public test networks of the other origin networks.
    "base-sepolia",
    "bnb-testnet",
    // The ids the services' tests and the zombienet harness declare.
    "kreivo-test",
    "polygon-test",
    "base-test",
];

/// Whether `network` is a test network.
pub fn is_test_network(network: &str) -> bool {
    TEST_NETWORKS.contains(&network)
}

/// The networks `pool`'s store declares that are not in `allowed`, by name; none when the store
/// has no `networks` table yet. Read-only.
pub async fn networks_outside(pool: &PgPool, allowed: &[&str]) -> sqlx::Result<Vec<String>> {
    let present: bool = sqlx::query_scalar("SELECT to_regclass('networks') IS NOT NULL")
        .fetch_one(pool)
        .await?;
    if !present {
        return Ok(Vec::new());
    }
    sqlx::query_scalar("SELECT network FROM networks WHERE NOT (network = ANY ($1)) ORDER BY 1")
        .bind(allowed)
        .fetch_all(pool)
        .await
}

/// The production networks `pool`'s store declares: every network not in [`TEST_NETWORKS`]. A
/// store is a test store exactly when this is empty.
pub async fn production_networks(pool: &PgPool) -> sqlx::Result<Vec<String>> {
    networks_outside(pool, TEST_NETWORKS).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_ky_6_test_networks_are_never_production_ids() {
        for production in ["kreivo", "polygon", "base", "bsc", "bnb", "ethereum"] {
            assert!(!is_test_network(production), "{production}");
        }
        for test in [
            "kreivo-local",
            "polygon-amoy",
            "base-sepolia",
            "bnb-testnet",
        ] {
            assert!(is_test_network(test), "{test}");
        }
    }
}

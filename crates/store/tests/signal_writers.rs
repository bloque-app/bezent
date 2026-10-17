// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! F-007 second review, S-a and S-b: the ingress cannot write the signals the endpoint treats as
//! already raised, nor the issuing network's observations that tell the prover a transaction
//! executed (`AC-I1.2`, `REQ-IC-4`, `REQ-OB-6`, `INV-8`). Each check runs under `SET LOCAL ROLE`
//! inside its own transaction, so no pooled connection keeps a role it was switched to (#27).

mod common;

use sqlx::PgPool;

/// Runs `sql` as `role` in a transaction that is rolled back; returns the SQLSTATE of its error.
async fn as_role(pool: &PgPool, role: &str, sql: &str) -> Option<String> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::raw_sql(&format!("SET LOCAL ROLE {role}"))
        .execute(&mut *tx)
        .await
        .unwrap();
    let out = sqlx::raw_sql(sql).execute(&mut *tx).await;
    tx.rollback().await.unwrap();
    out.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code().map(|c| c.into_owned()))
    })
}

const REFUSED: Option<&str> = Some(bezent_store::REFUSAL_SQLSTATE);

fn signal(kind: &str) -> String {
    signal_on(kind, "kreivo")
}

/// The reviewer's S-a: a row with the dedupe string or coordinate the endpoint will look for, seen
/// on `network` (since 0049 an endpoint raises an alert only on a network it covers).
fn signal_on(kind: &str, network: &str) -> String {
    format!(
        "INSERT INTO event_log (kind, payload) VALUES ('{kind}', \
           '{{\"dedupe\": \"{network}:signed:1:twin\", \"coordinate\": {{\"network\": \"{network}\", \
             \"block_hash\": \"0x01\", \"position_index\": \"0\"}}}}')"
    )
}

async fn declare(pool: &PgPool) {
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_a_the_ingress_cannot_record_a_compromise_or_anomaly_signal(pool: PgPool) {
    declare(&pool).await;
    for kind in ["key-compromise", "pause-requested", "delivery-anomaly"] {
        assert_eq!(
            as_role(&pool, bezent_store::INGRESS_ROLE, &signal(kind))
                .await
                .as_deref(),
            REFUSED,
            "{kind}"
        );
        // The endpoints and the core record them.
        for (role, network) in [
            (bezent_store::KREIVO_ENDPOINT_ROLE, "kreivo"),
            (bezent_store::EVM_ENDPOINT_ROLE, "polygon"),
            (bezent_store::CORE_ROLE, "kreivo"),
        ] {
            assert_eq!(
                as_role(&pool, role, &signal_on(kind, network)).await,
                None,
                "{role}: {kind}"
            );
        }
    }
    // The ingress keeps the kinds it writes.
    assert_eq!(
        as_role(&pool, bezent_store::INGRESS_ROLE, &signal("refusal")).await,
        None
    );
    // Nothing reached the log.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM event_log")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

fn observation(network: &str, index: i64) -> String {
    // The reviewer's S-b: a `transaction` observation naming a signed transaction.
    format!(
        "INSERT INTO observations (network, block_number, block_hash, block_time, position_kind, \
           position_index, sender, executed, detail) \
         VALUES ('{network}', 7, '\\x07', '2026-10-13T00:00:00Z', 'transaction', {index}, '0xlane', \
           TRUE, '{{\"signed_transaction\": \"1\"}}')"
    )
}

#[sqlx::test(migrator = "bezent_store::MIGRATOR")]
async fn s_b_the_ingress_cannot_record_an_issuing_network_observation(pool: PgPool) {
    sqlx::raw_sql(
        "INSERT INTO networks (network, role, address_format) VALUES \
           ('kreivo', 'issuing', 'ss58'), ('polygon', 'origin', 'evm')",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Any network's, the origin networks' included: an endpoint's role only. Since 0021 (#33,
    // #35) neither the ingress nor the core holds INSERT on observations, and since 0045 nor does
    // the endpoints' mark alone; the guard of 0020 still refuses the issuing network's to any
    // login without the endpoint's role.
    for network in ["kreivo", "undeclared", "polygon"] {
        for role in [
            bezent_store::INGRESS_ROLE,
            bezent_store::CORE_ROLE,
            bezent_store::SERVICE_ROLE,
            bezent_store::ENDPOINT_ROLE,
        ] {
            assert_eq!(
                as_role(&pool, role, &observation(network, 0))
                    .await
                    .as_deref(),
                Some("42501"),
                "{role}: {network}"
            );
        }
        // Since 0034 (#30) an endpoint records only its own network's: the Kreivo endpoint's
        // role the issuing network's.
        assert_eq!(
            as_role(
                &pool,
                bezent_store::KREIVO_ENDPOINT_ROLE,
                &observation(network, 0)
            )
            .await
            .as_deref(),
            if network == "kreivo" { None } else { REFUSED },
            "{network}"
        );
    }
    // A login that is granted INSERT (and the guards' helpers) but not the endpoint's role is
    // still refused the issuing network's by the guard.
    const HELPERS: &str = "observation_kind_on_network(TEXT, TEXT), \
        observation_kind_for_event(TEXT), observation_writer_covers(TEXT, OID), \
        bezent_endpoint_writer(OID), bezent_evm_endpoint_writer(OID), bezent_row_network(OID, JSONB)";
    sqlx::raw_sql(&format!(
        "GRANT INSERT ON observations TO bezent_ingress; \
         GRANT SELECT ON endpoint_scopes TO bezent_ingress; \
         GRANT EXECUTE ON FUNCTION {HELPERS} TO bezent_ingress"
    ))
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        as_role(&pool, bezent_store::INGRESS_ROLE, &observation("kreivo", 5))
            .await
            .as_deref(),
        REFUSED
    );
    sqlx::raw_sql(&format!(
        "REVOKE INSERT ON observations FROM bezent_ingress; \
         REVOKE SELECT ON endpoint_scopes FROM bezent_ingress; \
         REVOKE EXECUTE ON FUNCTION {HELPERS} FROM bezent_ingress"
    ))
    .execute(&pool)
    .await
    .unwrap();

    // Nor an outcome of the endpoint's observation.
    let id: i64 = sqlx::query_scalar(&format!("{} RETURNING id", observation("kreivo", 1)))
        .fetch_one(&pool)
        .await
        .unwrap();
    let outcome = format!(
        "INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, \
           final_block_hash) VALUES ({id}, 'final', 9, '\\x09')"
    );
    assert_eq!(
        as_role(&pool, bezent_store::INGRESS_ROLE, &outcome)
            .await
            .as_deref(),
        Some("42501")
    );
    assert_eq!(
        as_role(&pool, bezent_store::KREIVO_ENDPOINT_ROLE, &outcome).await,
        None
    );
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The grant matrix: what each role may do in the store, and all it may do (issues #35, #33;
//! `REQ-AR-3`, `REQ-OB-6`, `INV-8`, `INV-9`, `REQ-GV-5`).
//!
//! [`MATRIX`] is the one statement of it. The store test `grant_matrix` reads every role's
//! privileges from a migrated store and asserts they are exactly these, so a migration that widens
//! a grant fails CI; every service checks its own login against its role's rows at start-up
//! ([`crate::login::check_login`]), so a store whose grants were widened by hand is refused.
//! `crates/store/README.md` explains it in prose.
//!
//! A privilege is one of:
//! - `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `REFERENCES`, `TRIGGER` on a table or
//!   view of the schema, named alone when held on the whole relation, and as `table(col,…)` when
//!   held on some columns only;
//! - `USAGE`, `SELECT`, `UPDATE` on a sequence;
//! - `EXECUTE` on a function, named with its argument types: every function of the schema the role
//!   may call, but a [`PUBLIC_FUNCTIONS`] one (those run with the caller's own privileges, and
//!   every role may call them).

use sqlx::PgConnection;

/// The roles the matrix covers: every role the migrations create.
pub const ROLES: &[&str] = &[
    crate::SERVICE_ROLE,
    crate::CORE_ROLE,
    crate::INGRESS_ROLE,
    crate::ENDPOINT_ROLE,
    crate::KREIVO_ENDPOINT_ROLE,
    crate::EVM_ENDPOINT_ROLE,
    crate::WEBHOOKS_ROLE,
    crate::SIGNER_ROLE,
    crate::OPERATOR_ROLE,
];

/// The functions any role may call: helpers that run with the caller's privileges (`SECURITY
/// INVOKER`) and that check constraints, indexes and the services use. No other function of the
/// schema is PUBLIC's (F1 of migration 0017).
pub const PUBLIC_FUNCTIONS: &[&str] = &[
    "is_terminal_state(transfer_state)",
    "route_usage_locked(text,text,text,bigint)",
    "transfer_edge_needs_coordinate(transfer_state,transfer_state)",
];

/// (role, privilege, objects): every privilege each role holds. A role or privilege not listed
/// holds nothing. [`crate::SERVICE_ROLE`] and [`crate::ENDPOINT_ROLE`] hold nothing (migration
/// 0045): the first is retired, the second is only the mark the store's guards look for.
pub const MATRIX: &[(&str, &str, &[&str])] = &[
    // The core: it records intents, transitions, the book and the reserve check, and reads every
    // record but the requesters' credentials and the webhooks'.
    (
        crate::CORE_ROLE,
        "SELECT",
        &[
            "admitted_tokens",
            "assets",
            "attempt_milestones",
            "book_balances",
            "book_entries",
            "book_postings",
            "book_quarantine",
            "endpoint_scopes",
            "event_log",
            "event_log_head",
            "event_log_position",
            "final_heads",
            "intake",
            "intake_answers",
            "intents",
            "ledger_accounts",
            "ledger_accounts_listed",
            "manual_actions",
            "manual_approvals",
            "networks",
            "non_execution_proofs",
            "observation_gaps",
            "observation_outcomes",
            "observation_ranges",
            "observation_watermarks",
            "observations",
            "pauses",
            "prepared_transactions",
            "relayed_instructions",
            "reserve_evaluations",
            "reserve_readings",
            "reserve_statement_abandoned",
            "reserve_statements",
            "reserves",
            "reviews",
            "routes",
            "signatures",
            "signed_transactions",
            "surplus_funding_addresses",
            "transfer_status",
            "transfers",
            "transitions",
            "unassociated_arrivals",
            "unsigned_attempt_cancellations",
        ],
    ),
    (
        crate::CORE_ROLE,
        "INSERT",
        &[
            "book_entries",
            "book_postings",
            "book_quarantine",
            "event_log",
            "intents",
            "pauses(asset,cause,evaluation_id,quarantine_id,reading_id)",
            "reserve_evaluations",
            "reserve_statement_abandoned",
            "reserve_statements",
            "transfers",
            "transitions",
            "unsigned_attempt_cancellations",
        ],
    ),
    (
        crate::CORE_ROLE,
        "UPDATE",
        &["transfers(on_chain_reference)"],
    ),
    (
        crate::CORE_ROLE,
        "EXECUTE",
        &[
            "attempt_proof(bigint,integer)",
            "attempts_unproven(bigint)",
            "bezent_applied_migrations()",
            "bezent_writer_holds(text,oid)",
            "funding_arrival(text,text,text,bigint)",
            "one_business_day_after(timestamp with time zone)",
            "pause_reading_is_supply_mismatch(text,bigint)",
            "source_bindable(text,text,bigint,bytea,position_kind,bigint)",
        ],
    ),
    // The ingress API: requests, their answers and first transitions, the log's refusals, webhook
    // registration. It reads no intent, signature, signed bytes, proof, reading, book or reserve
    // record, and no sealed webhook column.
    (
        crate::INGRESS_ROLE,
        "SELECT",
        &[
            "admitted_tokens",
            "assets",
            "attempt_milestones",
            "book_balances",
            "event_log",
            "event_log_head",
            "event_log_position",
            "intake",
            "intake_answers",
            "ledger_accounts_listed",
            "networks",
            "observation_outcomes",
            "observations",
            "pauses",
            "relayed_instructions",
            "requester_credentials",
            "requester_grants",
            "requesters",
            "reserves",
            "routes",
            // funding_arrival(), which a deposit's binding calls (0032).
            "surplus_funding_addresses",
            "transfer_status",
            "transfers",
            "transitions",
            "webhook_condition_urls(id,requester,registered_at)",
            "webhook_deliveries",
            "webhook_secrets(id,requester,created_at)",
            "webhook_transfer_urls(id,requester,transfer_id,url_digest,previous_digest,after_seq,registered_at)",
        ],
    ),
    (
        crate::INGRESS_ROLE,
        "INSERT",
        &[
            "event_log",
            "intake",
            "intake_answers",
            "relayed_instructions",
            "transfers",
            "transitions",
            "webhook_condition_urls",
            "webhook_secrets",
            "webhook_transfer_urls",
        ],
    ),
    (crate::INGRESS_ROLE, "UPDATE", &["transfers(expires_at)"]),
    (
        crate::INGRESS_ROLE,
        "EXECUTE",
        &[
            "bezent_applied_migrations()",
            "bezent_writer_holds(text,oid)",
            "funding_arrival(text,text,text,bigint)",
            "source_bindable(text,text,bigint,bytea,position_kind,bigint)",
        ],
    ),
    // The endpoints: each alone appends what it observes and delivers; the guards of migrations
    // 0034 to 0036 narrow each to its own networks.
    // The Kreivo endpoint alone walks long histories from a final head (migration 0052).
    (
        crate::KREIVO_ENDPOINT_ROLE,
        "SELECT",
        KREIVO_ENDPOINT_SELECT,
    ),
    (
        crate::KREIVO_ENDPOINT_ROLE,
        "INSERT",
        KREIVO_ENDPOINT_INSERT,
    ),
    (crate::KREIVO_ENDPOINT_ROLE, "EXECUTE", ENDPOINT_EXECUTE),
    (crate::EVM_ENDPOINT_ROLE, "SELECT", ENDPOINT_SELECT),
    (crate::EVM_ENDPOINT_ROLE, "INSERT", ENDPOINT_INSERT),
    (crate::EVM_ENDPOINT_ROLE, "EXECUTE", ENDPOINT_EXECUTE),
    // The webhook dispatcher (migration 0044): the sealed secrets and URLs, the cursor, the holds
    // and the outbox.
    (
        crate::WEBHOOKS_ROLE,
        "SELECT",
        &[
            "event_log",
            "event_log_head",
            "intake",
            "intake_answers",
            "requester_grants",
            "transfers",
            "webhook_condition_holds",
            "webhook_condition_urls",
            "webhook_cursor",
            "webhook_deliveries",
            "webhook_secrets",
            "webhook_transfer_urls",
        ],
    ),
    (
        crate::WEBHOOKS_ROLE,
        "INSERT",
        &["webhook_condition_holds", "webhook_deliveries"],
    ),
    (
        crate::WEBHOOKS_ROLE,
        "UPDATE",
        &[
            "webhook_cursor(after)",
            "webhook_deliveries(state,attempts,next_attempt_at,leased_until,last_status,last_error,finished_at)",
        ],
    ),
    (crate::WEBHOOKS_ROLE, "DELETE", &["webhook_condition_holds"]),
    (
        crate::WEBHOOKS_ROLE,
        "EXECUTE",
        &["bezent_applied_migrations()"],
    ),
    // The signer (migration 0012): it reads the intent, the core's order and the prepared bytes,
    // and records the signature. Since 0049 (audit must-fix 2) it also reads which token an
    // order's asset is paid in, and which signing intent a signed transaction is of (a cancel's
    // target), to compare the bytes it decodes with the core's order. Nothing else (REQ-AR-3).
    (
        crate::SIGNER_ROLE,
        "SELECT",
        &[
            "admitted_tokens",
            "event_log(seq)",
            "intents",
            "prepared_transactions",
            "signatures",
            "signed_transactions(id,intent_id)",
        ],
    ),
    // The alert and pause request of a transaction whose decoded bytes do not answer the core's order
    // (migration 0049, audit must-fix 2): the store admits only those two kinds from it.
    (crate::SIGNER_ROLE, "INSERT", &["event_log", "signatures"]),
    (
        crate::SIGNER_ROLE,
        "EXECUTE",
        &[
            "attempt_gate_open(text,text,text,bytea,integer)",
            "bezent_applied_migrations()",
            "statement_place_refusal(text,bigint,bytea,jsonb)",
        ],
    ),
    // Each operator's own login (REQ-GV-5): manual actions and approvals, naming neither who nor
    // when; ledger accounts; reading what a review needs.
    (
        crate::OPERATOR_ROLE,
        "SELECT",
        &[
            "book_quarantine",
            "ledger_accounts",
            "ledger_accounts_listed",
            "manual_actions",
            "manual_approvals",
            "reserve_evaluations",
            "reserve_readings",
            "reserve_statement_abandoned",
            "reserve_statements",
            "reviews",
            "surplus_funding_addresses",
            "unassociated_arrivals",
        ],
    ),
    (
        crate::OPERATOR_ROLE,
        "INSERT",
        &[
            "ledger_accounts(network,address,kind,reason)",
            "manual_actions(kind,transfer_id,to_state,network,block_number,block_hash,block_time,justification)",
            "manual_approvals(action_id,justification)",
        ],
    ),
    (
        crate::OPERATOR_ROLE,
        "EXECUTE",
        &[
            "bezent_applied_migrations()",
            "funding_arrival(text,text,text,bigint)",
            "one_business_day_after(timestamp with time zone)",
            "source_bindable(text,text,bigint,bytea,position_kind,bigint)",
        ],
    ),
];

const ENDPOINT_SELECT: &[&str] = &[
    "admitted_tokens",
    "assets",
    "endpoint_scopes",
    "event_log",
    "final_heads",
    "intents",
    "networks",
    "non_execution_proofs",
    "observation_gaps",
    "observation_outcomes",
    "observation_ranges",
    "observation_watermarks",
    "observations",
    "prepared_transactions",
    "reserve_readings",
    "reserves",
    "signatures",
    "signed_transactions",
];

const ENDPOINT_INSERT: &[&str] = &[
    "event_log",
    "final_heads",
    "non_execution_proofs",
    "observation_outcomes",
    "observation_ranges",
    "observations",
    "prepared_transactions",
    "reserve_readings",
    "signed_transactions",
];

/// [`ENDPOINT_SELECT`] and the Kreivo endpoint's walk anchors.
const KREIVO_ENDPOINT_SELECT: &[&str] = &[
    "admitted_tokens",
    "assets",
    "endpoint_scopes",
    "event_log",
    "final_heads",
    "intents",
    "networks",
    "non_execution_proofs",
    "observation_gaps",
    "observation_outcomes",
    "observation_ranges",
    "observation_walk_anchors",
    "observation_watermarks",
    "observations",
    "prepared_transactions",
    "reserve_readings",
    "reserves",
    "signatures",
    "signed_transactions",
];

/// [`ENDPOINT_INSERT`] and the Kreivo endpoint's walk anchors.
const KREIVO_ENDPOINT_INSERT: &[&str] = &[
    "event_log",
    "final_heads",
    "non_execution_proofs",
    "observation_outcomes",
    "observation_ranges",
    "observation_walk_anchors",
    "observations",
    "prepared_transactions",
    "reserve_readings",
    "signed_transactions",
];

const ENDPOINT_EXECUTE: &[&str] = &[
    "bezent_applied_migrations()",
    "bezent_endpoint_writer(oid)",
    "bezent_evm_endpoint_writer(oid)",
    "bezent_row_network(oid,jsonb)",
    "bezent_writer_holds(text,oid)",
    "observation_kind_for_event(text)",
    "observation_kind_on_network(text,text)",
    "observation_writer_covers(text,oid)",
];

/// `role`'s rows of [`MATRIX`], as sorted (privilege, object) pairs.
pub fn expected(role: &str) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = MATRIX
        .iter()
        .filter(|(r, _, _)| *r == role)
        .flat_map(|(_, privilege, objects)| {
            objects
                .iter()
                .map(move |o| ((*privilege).to_owned(), (*o).to_owned()))
        })
        .collect();
    rows.sort();
    rows
}

/// Every privilege `role` holds in the store `conn` is connected to (a role or a login; what it
/// inherits and what PUBLIC holds included), as sorted (privilege, object) pairs in the form of
/// [`MATRIX`]. Every schema but the server's own is read from the catalogue, so an object added
/// anywhere is covered without a change here: an object outside `public` is named
/// `schema.object`; a privilege on a schema is named `schema <name>` (`USAGE` on `public`, which
/// PUBLIC holds, aside) and one on the database `database` (`CONNECT`, which PUBLIC holds, aside).
pub async fn held(conn: &mut PgConnection, role: &str) -> sqlx::Result<Vec<(String, String)>> {
    sqlx::query_as(HELD).bind(role).fetch_all(conn).await
}

const HELD: &str = "\
WITH rels AS (
    SELECT c.oid, CASE WHEN n.nspname = 'public' THEN c.relname::text
                       ELSE n.nspname || '.' || c.relname END AS relname
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND c.relkind IN ('r', 'v', 'm', 'p', 'f')
), privs(p) AS (
    VALUES ('SELECT'), ('INSERT'), ('UPDATE'), ('DELETE'), ('TRUNCATE'), ('REFERENCES'), ('TRIGGER')
), held AS (
    SELECT p, relname AS obj FROM rels, privs WHERE has_table_privilege($1, rels.oid, p)
    UNION ALL
    SELECT p, relname || '(' || string_agg(a.attname::text, ',' ORDER BY a.attnum) || ')'
    FROM rels, privs, pg_attribute a
    WHERE a.attrelid = rels.oid AND a.attnum > 0 AND NOT a.attisdropped
      AND p IN ('SELECT', 'INSERT', 'UPDATE', 'REFERENCES')
      AND NOT has_table_privilege($1, rels.oid, p)
      AND has_column_privilege($1, rels.oid, a.attnum, p)
    GROUP BY p, relname
    UNION ALL
    SELECT s.p, CASE WHEN n.nspname = 'public' THEN c.relname::text
                     ELSE n.nspname || '.' || c.relname END
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    CROSS JOIN (VALUES ('USAGE'), ('SELECT'), ('UPDATE')) AS s(p)
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND c.relkind = 'S' AND has_sequence_privilege($1, c.oid, s.p)
    UNION ALL
    SELECT 'EXECUTE', f.oid::regprocedure::text FROM pg_proc f
    JOIN pg_namespace n ON n.oid = f.pronamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND has_function_privilege($1, f.oid, 'EXECUTE')
      AND (f.prosecdef OR NOT EXISTS (
          SELECT 1 FROM aclexplode(coalesce(f.proacl, acldefault('f', f.proowner))) a
          WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'))
    UNION ALL
    SELECT s.p, 'schema ' || n.nspname FROM pg_namespace n
    CROSS JOIN (VALUES ('USAGE'), ('CREATE')) AS s(p)
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND has_schema_privilege($1, n.oid, s.p)
      AND NOT (n.nspname = 'public' AND s.p = 'USAGE')
    UNION ALL
    SELECT s.p, 'database' FROM (VALUES ('CREATE'), ('TEMPORARY')) AS s(p)
    WHERE has_database_privilege($1, current_database(), s.p)
)
SELECT p, obj FROM held ORDER BY 1, 2";

/// Every grant in the store's catalogue that the matrix does not account for, as readable lines,
/// sorted; the store test asserts there is none (review S3 of PR #56). It reads every relation
/// (tables, views, sequences), column (review S3b), function and schema of every schema but the server's own, and the
/// database itself, from their access lists, so an object added later is covered without a change
/// here. A grant is accounted for when its grantee is the object's owner, or one of [`ROLES`]
/// (whose privileges [`held`] compares with [`MATRIX`]), or PUBLIC holding `USAGE` on the schema
/// `public`, `CONNECT` on the database, or `EXECUTE` on a function that runs with the caller's
/// privileges and is a [`PUBLIC_FUNCTIONS`] one or a trigger function (which only its trigger
/// calls). Any other grantee (a login, a `pg_*` role, a role the matrix does not name) and any
/// default privilege (`ALTER DEFAULT PRIVILEGES`) is listed.
pub async fn stray_grants(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(STRAY)
        .bind(ROLES)
        .bind(PUBLIC_FUNCTIONS)
        .fetch_all(conn)
        .await
}

const STRAY: &str = "\
WITH objs AS (
    SELECT 'relation ' || c.oid::regclass::text AS obj, c.relowner AS owner,
           '{}'::text[] AS public_ok,
           aclexplode(coalesce(c.relacl,
                               acldefault((CASE WHEN c.relkind = 'S' THEN 's' ELSE 'r' END)::\"char\",
                                          c.relowner))) AS a
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND c.relkind IN ('r', 'v', 'm', 'p', 'f', 'S')
    UNION ALL
    SELECT 'column ' || c.oid::regclass::text || '.' || quote_ident(a.attname::text), c.relowner,
           '{}'::text[], aclexplode(a.attacl)
    FROM pg_attribute a
    JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
      AND a.attnum > 0 AND NOT a.attisdropped AND a.attacl IS NOT NULL
    UNION ALL
    SELECT 'function ' || f.oid::regprocedure::text, f.proowner,
           CASE WHEN NOT f.prosecdef
                     AND (f.prorettype = 'trigger'::regtype
                          OR f.oid::regprocedure::text = ANY ($2))
                THEN ARRAY['EXECUTE'] ELSE '{}'::text[] END,
           aclexplode(coalesce(f.proacl, acldefault('f', f.proowner)))
    FROM pg_proc f JOIN pg_namespace n ON n.oid = f.pronamespace
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
    UNION ALL
    SELECT 'schema ' || n.nspname, n.nspowner,
           CASE WHEN n.nspname = 'public' THEN ARRAY['USAGE'] ELSE '{}'::text[] END,
           aclexplode(coalesce(n.nspacl, acldefault('n', n.nspowner)))
    FROM pg_namespace n
    WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') AND n.nspname NOT LIKE 'pg\\_%'
    UNION ALL
    SELECT 'database', d.datdba, ARRAY['CONNECT'],
           aclexplode(coalesce(d.datacl, acldefault('d', d.datdba)))
    FROM pg_database d WHERE d.datname = current_database()
)
SELECT obj || ': ' || (a).privilege_type || ' to '
       || CASE WHEN (a).grantee = 0 THEN 'PUBLIC' ELSE pg_get_userbyid((a).grantee)::text END
FROM objs
WHERE (a).grantee <> owner
  AND NOT ((a).grantee <> 0 AND pg_get_userbyid((a).grantee)::text = ANY ($1))
  AND NOT ((a).grantee = 0 AND (a).privilege_type = ANY (public_ok))
UNION ALL
SELECT 'default privileges of ' || pg_get_userbyid(d.defaclrole)::text || ' on '
       || d.defaclobjtype::text || ': ' || d.defaclacl::text
FROM pg_default_acl d
ORDER BY 1";

/// Every role that reaches one of [`ROLES`] through a membership and is neither one of them nor
/// one of `logins` (each a role that can log in), as readable lines, sorted; the store test asserts
/// there is none (review S3b of PR #56). Memberships are followed transitively, so a role granted
/// an expected login (and through it the login's store role) is listed too. The memberships among
/// [`ROLES`] themselves (the marks) are the role-edges test's.
pub async fn stray_members(
    conn: &mut PgConnection,
    logins: &[String],
) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(STRAY_MEMBERS)
        .bind(ROLES)
        .bind(logins)
        .fetch_all(conn)
        .await
}

const STRAY_MEMBERS: &str = "\
WITH RECURSIVE reach(member, via) AS (
    SELECT m.member, g.rolname::text FROM pg_auth_members m
    JOIN pg_roles g ON g.oid = m.roleid
    WHERE g.rolname = ANY ($1)
    UNION
    SELECT m.member, r.via FROM pg_auth_members m JOIN reach r ON m.roleid = r.member
)
SELECT DISTINCT u.rolname::text || ' reaches ' || r.via
FROM reach r JOIN pg_roles u ON u.oid = r.member
WHERE NOT (u.rolname = ANY ($1))
  AND NOT (u.rolcanlogin AND u.rolname = ANY ($2))
ORDER BY 1";

/// The functions of the schema that PUBLIC may call and that run with the caller's privileges,
/// trigger functions aside (they cannot be called but by their trigger), sorted; the store test asserts they are [`PUBLIC_FUNCTIONS`]. A `SECURITY DEFINER` function
/// PUBLIC may call shows in every role's [`held`] rows instead.
pub async fn public_functions(conn: &mut PgConnection) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT f.oid::regprocedure::text FROM pg_proc f \
         JOIN pg_namespace n ON n.oid = f.pronamespace \
         WHERE n.nspname = 'public' AND NOT f.prosecdef \
           AND f.prorettype <> 'trigger'::regtype \
           AND EXISTS (SELECT 1 FROM aclexplode(coalesce(f.proacl, acldefault('f', f.proowner))) a \
                       WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE') \
         ORDER BY 1",
    )
    .fetch_all(conn)
    .await
}

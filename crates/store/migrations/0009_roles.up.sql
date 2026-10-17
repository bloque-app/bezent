-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 (review S1): the role the services use is not the role that owns the schema (SPEC §9: the
-- store layer survives any application change; REQ-AR-5).
--
-- The migrations run as the migration owner, which owns every table, trigger and function. The
-- services log in as a role that is a member of `bezent_service` and of nothing that owns the
-- schema. `bezent_service` is not an owner, so it cannot ALTER, DROP or DISABLE TRIGGER, and it may
-- create no function, trigger or temporary table of its own. It holds:
--   * SELECT and INSERT on every append-only table;
--   * on transfers: SELECT, INSERT, and UPDATE of the write-once columns only (refund destination,
--     expiry, source binding, on-chain reference). The state, the condition and the milestones
--     are written by transitions_apply(), which runs as the owner (SECURITY DEFINER);
--   * SELECT on the log head and the derived views; the head moves only through
--     event_log_assign_seq(), which also runs as the owner.
-- The service binaries embed only these up migrations; the down migrations live with the tests.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_service') THEN
        BEGIN
            CREATE ROLE bezent_service NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;

    IF current_user = 'bezent_service'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_service' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as the service role (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_service' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_service must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;

    -- No role but the owner may create temporary objects (a temporary trigger function could
    -- otherwise reach the transfers at trigger depth 2).
    EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database());
END
$$;

REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO bezent_service;

GRANT SELECT, INSERT ON
    intake, transitions, observations, observation_outcomes, book_entries, book_postings,
    intents, signed_transactions, event_log
    TO bezent_service;

GRANT SELECT, INSERT ON transfers TO bezent_service;
GRANT UPDATE (refund_destination_network, refund_destination, expires_at,
              source_network, source_block_number, source_block_hash,
              source_position_kind, source_position_index, on_chain_reference)
    ON transfers TO bezent_service;

GRANT SELECT ON event_log_head, transfer_status, book_balances, event_log_position TO bezent_service;

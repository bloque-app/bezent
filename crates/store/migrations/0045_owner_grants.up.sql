-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Issues #35, #33, #46, #27: each service role holds only what its service needs; the shared
-- roles hold nothing (SPEC §4.1 REQ-AR-3, §8.1 REQ-OB-6, §8.3 REQ-EO-1, §9 INV-8, INV-9;
-- REQ-GV-5). Additive: no table, trigger or function changes.
--
-- Until now every service but the signer and the webhook dispatcher logged in as a member of
-- `bezent_service`, so the internet-facing ingress could read and append much of what only the
-- core or an endpoint should: the core's login also held `bezent_core`, and the endpoints held
-- `bezent_endpoint`, which inherited `bezent_service`. From here on:
--
--   * `bezent_service` holds nothing. It stays as a role (roles are shared by every database on
--     the server), and a member of it gains nothing in this database.
--   * `bezent_endpoint` holds nothing either. It stays as the mark the store's guards look for
--     ("an endpoint wrote this": 0020, 0036), held by `bezent_kreivo_endpoint` and
--     `bezent_evm_endpoint` without SET (0034, 0035).
--   * Each service's own role holds exactly what that service reads and writes:
--       bezent_core             the core (intents, transitions, the book, the reserve check)
--       bezent_ingress          the ingress API (intake, requests, webhook registration)
--       bezent_kreivo_endpoint  the Kreivo endpoint  } observations, outcomes, read ranges, final
--       bezent_evm_endpoint     the EVM endpoint     } heads, prepared and signed bytes, proofs,
--                                                      readings; the store's guards narrow each to
--                                                      its networks (0034, 0035)
--       bezent_webhooks         the webhook dispatcher (0044, unchanged)
--       bezent_signer           the signer (0012, unchanged)
--       bezent_operator         each operator's own login (0014, unchanged; REQ-GV-5)
--   * Every service login is a member of its own role and of nothing else, and cannot SET ROLE into
--     any other: the services check this at start-up and refuse to run otherwise (#46).
--
-- The memberships `bezent_ingress` -> `bezent_service` (0044) and `bezent_endpoint` ->
-- `bezent_service` (0019) are server-wide and stay, without SET: a member can no longer become
-- `bezent_service`, and inheriting it gives nothing. They are not revoked because an earlier
-- migration of another database on the same server would grant them again with SET (tests create
-- one database per test).
--
-- The full matrix of what each role may do is in crates/store/README.md, and the store test
-- `grant_matrix` asserts it: a migration that widens any grant fails CI.
--
-- Trigger functions fire without an EXECUTE grant; the three that run as the owner (SECURITY
-- DEFINER) are no longer PUBLIC's to call. The helper functions the store's guards call run as the
-- inserting role, so the roles that insert into a guarded table keep EXECUTE on them.

DO $$
BEGIN
    IF current_setting('server_version_num')::int >= 160000 THEN
        EXECUTE 'GRANT bezent_service TO bezent_ingress WITH SET FALSE';
        EXECUTE 'GRANT bezent_service TO bezent_endpoint WITH SET FALSE';
        IF EXISTS (SELECT 1 FROM pg_auth_members m
                   JOIN pg_roles g ON g.oid = m.roleid
                   JOIN pg_roles u ON u.oid = m.member
                   WHERE g.rolname IN ('bezent_service', 'bezent_endpoint') AND m.set_option
                     AND u.rolname IN ('bezent_ingress', 'bezent_endpoint',
                                       'bezent_kreivo_endpoint', 'bezent_evm_endpoint')) THEN
            RAISE EXCEPTION 'a service role must not SET ROLE into bezent_service or bezent_endpoint (#46)'
                USING ERRCODE = 'BZ001';
        END IF;
    ELSE
        -- Before PostgreSQL 16 every member may SET ROLE: drop the memberships instead. The
        -- services refuse to start on such a server anyway (#46).
        REVOKE bezent_service FROM bezent_ingress;
        REVOKE bezent_service FROM bezent_endpoint;
    END IF;

    -- No service role is a member of another, but for the marks above.
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname LIKE 'bezent\_%' AND u.rolname LIKE 'bezent\_%'
                 AND (g.rolname, u.rolname) NOT IN (
                     ('bezent_service', 'bezent_ingress'),
                     ('bezent_service', 'bezent_endpoint'),
                     ('bezent_endpoint', 'bezent_kreivo_endpoint'),
                     ('bezent_endpoint', 'bezent_evm_endpoint'))
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_ingress',
                                   'bezent_endpoint', 'bezent_kreivo_endpoint',
                                   'bezent_evm_endpoint', 'bezent_webhooks', 'bezent_signer',
                                   'bezent_operator')) THEN
        RAISE EXCEPTION 'the service roles must not be members of each other (REQ-AR-3, #35)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname IN ('bezent_core', 'bezent_ingress', 'bezent_kreivo_endpoint',
                                 'bezent_evm_endpoint', 'bezent_webhooks', 'bezent_signer',
                                 'bezent_operator')
                 AND (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb OR rolcanlogin)) THEN
        RAISE EXCEPTION 'a service role must not log in, be a superuser, bypass row security or create roles or databases'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- The shared roles: nothing.

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM bezent_service, bezent_endpoint;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM bezent_service, bezent_endpoint;
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA public FROM bezent_service, bezent_endpoint;
REVOKE USAGE ON SCHEMA public FROM bezent_service, bezent_endpoint;

-- The trigger functions that run as the owner are nobody's to call.
REVOKE EXECUTE ON FUNCTION event_log_assign_seq(), transfers_log_created(), transitions_apply()
    FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- The core (bezent_core). Kept from 0012, 0017, 0030, 0031, 0040, 0041, 0043: intents, the book,
-- the reserve check's records and pauses, attempt_proof, attempts_unproven,
-- pause_reading_is_supply_mismatch, source_bindable, funding_arrival, the ledger accounts.
-- Added: what it read and appended through bezent_service. It reads every record but the
-- requesters' credentials and the webhooks'; it appends transfers (an unassociated arrival's
-- refund), transitions and the log's signals, and sets a transfer's on-chain reference once.

GRANT SELECT ON
    admitted_tokens, assets, attempt_milestones, endpoint_scopes, event_log, event_log_head,
    final_heads, intake, intake_answers, manual_actions, manual_approvals, networks,
    non_execution_proofs, observation_outcomes, observation_ranges, observations,
    prepared_transactions, relayed_instructions, reserve_readings, reserves, routes, signatures,
    signed_transactions, surplus_funding_addresses, transfers, transitions,
    book_balances, event_log_position, observation_gaps, observation_watermarks, reviews,
    transfer_status, unassociated_arrivals
    TO bezent_core;
GRANT INSERT ON transfers, transitions, event_log TO bezent_core;
GRANT UPDATE (on_chain_reference) ON transfers TO bezent_core;
GRANT EXECUTE ON FUNCTION one_business_day_after(TIMESTAMPTZ), bezent_writer_holds(TEXT, OID)
    TO bezent_core;

-- ---------------------------------------------------------------------------------------------
-- The ingress API (bezent_ingress). Kept from 0044: registering webhook URLs and secrets. Added:
-- what it read and appended through bezent_service. It reads the declarations, the requests, the
-- transfers and the log; it appends requests, their answers, relayed instructions, new transfers,
-- their first transitions and the log's refusals; it sets a transfer's expiry once. It reads no
-- intent, signature, signed bytes, proof, reading, book or reserve record, and writes none.

GRANT USAGE ON SCHEMA public TO bezent_ingress;
GRANT SELECT ON
    admitted_tokens, assets, attempt_milestones, event_log, event_log_head, intake,
    intake_answers, networks, observation_outcomes, observations, pauses, relayed_instructions,
    requester_credentials, requester_grants, requesters, reserves, routes,
    surplus_funding_addresses, transfers, transitions,
    book_balances, event_log_position, ledger_accounts_listed, transfer_status, webhook_deliveries
    TO bezent_ingress;
GRANT SELECT (id, requester, created_at) ON webhook_secrets TO bezent_ingress;
GRANT SELECT (id, requester, transfer_id, url_digest, previous_digest, after_seq, registered_at)
    ON webhook_transfer_urls TO bezent_ingress;
GRANT SELECT (id, requester, registered_at) ON webhook_condition_urls TO bezent_ingress;
GRANT INSERT ON intake, intake_answers, relayed_instructions, transfers, transitions, event_log
    TO bezent_ingress;
GRANT UPDATE (expires_at) ON transfers TO bezent_ingress;
GRANT EXECUTE ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    funding_arrival(TEXT, TEXT, TEXT, BIGINT), bezent_writer_holds(TEXT, OID)
    TO bezent_ingress;

-- ---------------------------------------------------------------------------------------------
-- The endpoints (bezent_kreivo_endpoint, bezent_evm_endpoint): the same grants, which the guards
-- of 0034, 0035 and 0036 narrow to each one's networks. They alone append observations, outcomes,
-- read ranges, final heads, prepared and signed transactions, proofs of non-execution and
-- readings, and the log's signals with the core. They read what they observe and deliver against:
-- the declarations, the intents, the signatures and their own records.

GRANT SELECT ON
    admitted_tokens, assets, endpoint_scopes, event_log, final_heads, intents, networks,
    non_execution_proofs, observation_outcomes, observation_ranges, observations,
    prepared_transactions, reserve_readings, reserves, signatures, signed_transactions,
    observation_gaps, observation_watermarks
    TO bezent_kreivo_endpoint, bezent_evm_endpoint;
GRANT INSERT ON
    event_log, final_heads, non_execution_proofs, observation_outcomes, observation_ranges,
    observations, prepared_transactions, reserve_readings, signed_transactions
    TO bezent_kreivo_endpoint, bezent_evm_endpoint;
GRANT EXECUTE ON FUNCTION
    bezent_writer_holds(TEXT, OID), bezent_endpoint_writer(OID), bezent_evm_endpoint_writer(OID),
    bezent_row_network(OID, JSONB), observation_kind_on_network(TEXT, TEXT),
    observation_kind_for_event(TEXT), observation_writer_covers(TEXT, OID)
    TO bezent_kreivo_endpoint, bezent_evm_endpoint;

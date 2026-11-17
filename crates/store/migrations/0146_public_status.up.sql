-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-10 (amendment 0029, Open 8, ruled yes, `d-0029-status`; SPEC REQ-ST-8, REQ-ID-4): what
-- the public status route may read, and nothing else.
--
-- The route answers anyone, with no credential, about two things only: a directed deposit, found by
-- its arrival's coordinate, and a withdrawal, found by its redemption's on-chain reference. Its answer
-- carries the state, the named conditions and the coordinates, and nothing a requester supplied
-- (REQ-ST-8): no request reference, no intake body, no requester, no callback, no extra.
--
--   * `bezent_public`, the route's own role. Its login is a member of it and of nothing else, as every
--     service's is (#46), and it holds SELECT on one view, `public_status`, and the migrations' list
--     (`bezent_applied_migrations()`, which every login's start-up check reads). It reads no table.
--   * `public_status`, owned by the migration owner, so it reads the tables with the owner's rights
--     and its reader needs none. It has no column a requester supplied: a row is a transfer's
--     bridge reference, its role, its state and condition, its coordinates and their finality, and
--     the time of its last change. Its rows are only:
--       - a directed deposit (the core's deposit bound to a `directed_arrival`, with no intake answer)
--         and the core's return of a directed arrival (the refund of an unassociated arrival bound to
--         one, with none), each found by its own bridge reference (`bz-d-…`, `bz-u-…`, derived from the
--         coordinate);
--       - a refund of such a deposit (the review edge of 0145), found by the deposit's;
--       - a withdrawal, and its refund mint, found by the withdrawal's bridge reference (`bz-w-…`,
--         derived from the redeemer and the redemption's reference).
--     A transfer a request made or reached (a requester's deposit, a requester's refund of an
--     unassociated arrival) is no row: its coordinate reads as unknown (REQ-ID-4). A withdrawal is a
--     row whatever a requester added to it (`POST /v0/withdrawals/extras`): the extras are not.
--
-- Every lookup is by `transfers.bridge_reference` (unique) and, for a refund, `transfers.refund_of`
-- (unique); the observation at a transfer's source by `observations_coordinate_once`; the answer by
-- `intake_answers_transfer`; the last change by `transitions_transfer`; an attempt's inclusion by
-- `attempt_milestones_once`. No lookup scans a table, so an unauthenticated caller costs bezent a
-- handful of index reads at most (`crates/store/tests/public_status.rs` holds the plan to that).

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_public') THEN
        BEGIN
            CREATE ROLE bezent_public NOLOGIN;
            -- PostgreSQL 16 records the creator as a member of the role it made (ADMIN OPTION, and SET and
            -- INHERIT too if createrole_self_grant says so). The owner keeps ADMIN OPTION (it grants the role to
            -- the route's login) and drops the rest, so it never acts as the role nor inherits it.
            IF EXISTS (SELECT 1 FROM pg_auth_members m
                       JOIN pg_roles g ON g.oid = m.roleid
                       JOIN pg_roles u ON u.oid = m.member
                       WHERE g.rolname = 'bezent_public' AND u.rolname = current_user
                         AND (m.set_option OR m.inherit_option)) THEN
                REVOKE SET OPTION FOR bezent_public FROM CURRENT_USER;
                REVOKE INHERIT OPTION FOR bezent_public FROM CURRENT_USER;
            END IF;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;
    IF current_user = 'bezent_public'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_public' AND u.rolname = current_user
                    AND (m.set_option OR m.inherit_option)) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_public (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_public' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_public must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    -- No store role holds it, and it holds no role.
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE (g.rolname = 'bezent_public'
                      AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_endpoint',
                                        'bezent_kreivo_endpoint', 'bezent_evm_endpoint',
                                        'bezent_signer', 'bezent_operator', 'bezent_ingress',
                                        'bezent_webhooks'))
                  OR u.rolname = 'bezent_public') THEN
        RAISE EXCEPTION 'bezent_public and the services'' roles must not be members of each other (REQ-ST-8)'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

-- The rows anyone may read, and their keys. `lookup_reference` is the bridge reference a caller's
-- coordinate or redemption derives; `subject` what was asked about; `role` what the row's transfer is
-- to it. A security barrier (review L1 of PR #139): the view reads as its owner for a login that reads
-- no table, so no function of a caller's query is run on a row before the view's own conditions have
-- kept it; the route's keys (`=` on text, `= ANY` of a text array) are leakproof, so they still reach
-- the index (`services/ingress/tests/public_status.rs` holds the plan to that).
CREATE VIEW public_status WITH (security_barrier) AS
SELECT k.lookup_reference,
       k.subject,
       k.role,
       t.bridge_reference,
       t.state::text AS state,
       t.condition::text AS condition,
       t.source_network,
       t.source_block_number,
       t.source_block_hash,
       t.source_position_kind::text AS source_position_kind,
       t.source_position_index,
       t.source_final_network IS NOT NULL AS source_final,
       -- A transfer's own inclusion milestone is written when it completes; while it is `Delivered`,
       -- the inclusion is its current attempt's (as the query surface shows it, REQ-TR-4).
       COALESCE(t.destination_included_network, m.network) AS included_network,
       COALESCE(t.destination_included_block_number, m.block_number) AS included_block_number,
       COALESCE(t.destination_included_block_hash, m.block_hash) AS included_block_hash,
       t.destination_final_network AS final_network,
       t.destination_final_block_number AS final_block_number,
       t.destination_final_block_hash AS final_block_hash,
       COALESCE((SELECT x.seen_at FROM transitions x WHERE x.transfer_id = t.id
                 ORDER BY x.id DESC LIMIT 1), t.created_at) AS updated_at
FROM (
    -- A directed deposit, and the core's return of a directed arrival: bound to a directed arrival,
    -- with no request.
    SELECT d.bridge_reference AS lookup_reference, 'directed_deposit'::text AS subject,
           CASE d.kind WHEN 'deposit' THEN 'deposit' ELSE 'return' END AS role, d.id
    FROM transfers d
    WHERE d.kind IN ('deposit', 'refund') AND d.refund_of IS NULL
      AND EXISTS (SELECT 1 FROM observations o
                  WHERE o.network = d.source_network AND o.block_number = d.source_block_number
                    AND o.block_hash = d.source_block_hash
                    AND o.position_kind = d.source_position_kind
                    AND o.position_index = d.source_position_index
                    AND o.kind = 'directed_arrival')
      AND NOT EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = d.id)
    UNION ALL
    -- A directed deposit's refund (0145's review edge), found by the deposit's bridge reference.
    SELECT p.bridge_reference, 'directed_deposit', 'refund', r.id
    FROM transfers p JOIN transfers r ON r.refund_of = p.id
    WHERE p.kind = 'deposit' AND p.refund_of IS NULL
      AND EXISTS (SELECT 1 FROM observations o
                  WHERE o.network = p.source_network AND o.block_number = p.source_block_number
                    AND o.block_hash = p.source_block_hash
                    AND o.position_kind = p.source_position_kind
                    AND o.position_index = p.source_position_index
                    AND o.kind = 'directed_arrival')
      AND NOT EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = p.id)
      AND NOT EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = r.id)
    UNION ALL
    -- A withdrawal: made from a redemption, by the core.
    SELECT w.bridge_reference, 'withdrawal', 'withdrawal', w.id
    FROM transfers w
    WHERE w.kind = 'withdrawal'
    UNION ALL
    -- A withdrawal's refund mint (REQ-RF-4), found by the withdrawal's bridge reference.
    SELECT p.bridge_reference, 'withdrawal', 'refund', r.id
    FROM transfers p JOIN transfers r ON r.refund_of = p.id
    WHERE p.kind = 'withdrawal'
) k
JOIN transfers t ON t.id = k.id
LEFT JOIN LATERAL (
    SELECT a.network, a.block_number, a.block_hash FROM attempt_milestones a
    WHERE a.transfer_id = t.id AND a.milestone = 'included' AND t.state = 'Delivered'
    ORDER BY a.attempt DESC LIMIT 1
) m ON TRUE;

COMMENT ON VIEW public_status IS
    'REQ-ST-8, REQ-ID-4 (F-018, T-018-10): the state, condition and coordinates of a directed deposit '
    '(and its return or refund) and of a withdrawal (and its refund mint), by the bridge reference '
    'its coordinate or redemption derives (lookup_reference). Nothing a requester supplied; no '
    'transfer a request made or reached but a withdrawal. Read by bezent_public, which reads nothing else.';

GRANT USAGE ON SCHEMA public TO bezent_public;
GRANT SELECT ON public_status TO bezent_public;
GRANT EXECUTE ON FUNCTION bezent_applied_migrations() TO bezent_public;

-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-003 and F-007 together: the two-step delivery of PLAN DEC-3 on the endpoint's real tables, and
-- each role narrowed to what it writes (issues #33, #35; SPEC §8.2 REQ-NX-1/2, §8.3 REQ-EO-6, §9
-- INV-8, INV-9, REQ-AR-3).
--
-- F-003 (0014, 0017) was written before F-007's tables existed, so attempt_proof and
-- attempt_gate_open read non_execution_proofs only when it existed (to_regclass, dynamic SQL), and
-- kept every gate closed without it. 0018 now always makes the table before them: both read it
-- directly, so a missing or renamed table is an error at migration time, not a gate closed for
-- good. Their behaviour, owner, SECURITY DEFINER and grants are 0017's.
--
-- Grants (#33, #35): every service logs in as a member of bezent_service, the internet-facing
-- ingress included. The signed bytes and every network's observations and outcomes are an
-- endpoint's record: what it signed and submitted (REQ-EO-1), and what it read at coordinates
-- (REQ-OB-6). Only bezent_endpoint may insert them now, as it alone inserts proofs, prepared
-- transactions, read ranges and final heads (0015, 0019). 0020's guard for the issuing network
-- stays, and now holds for origin networks as well through the grant. The migration owner keeps
-- writing all of them, for drills and restores (NFR-7).
--
-- INV-8, REQ-EO-6 (review round 3, S-1): an attempt stands proven not executed only when every
-- signature recorded for it has its signed bytes recorded, and every one of those signed
-- transactions, whatever its bytes, has a proof at finality. A signature whose bytes no endpoint
-- recorded (a crash between the link's answer and the insert) keeps the attempt unproven. The
-- store holds this by itself: it leans neither on signatures_once_per_reference_attempt, which
-- skips a lane without a reference, nor on the core signing an attempt once.
--
-- F1 of 0017, for F-007's functions: none is PUBLIC's. The trigger functions fire without an
-- EXECUTE grant; bezent_writer_holds runs as the inserting role inside them, so the services keep it.

CREATE OR REPLACE FUNCTION attempt_proof(
    p_transfer BIGINT, p_attempt INTEGER,
    OUT network TEXT, OUT final_block_number BIGINT, OUT final_block_hash BYTEA
)
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT n.network, n.final_block_number, n.final_block_hash
    FROM non_execution_proofs n
    JOIN signed_transactions st ON st.id = n.signed_transaction_id
    JOIN intents i ON i.id = st.intent_id
    WHERE i.transfer_id = p_transfer AND i.attempt = p_attempt
      AND NOT EXISTS (SELECT 1 FROM signed_transactions st2
                      JOIN intents i2 ON i2.id = st2.intent_id
                      WHERE i2.transfer_id = p_transfer AND i2.attempt = p_attempt
                        AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n2
                                        WHERE n2.signed_transaction_id = st2.id))
      AND NOT EXISTS (SELECT 1 FROM signatures s3
                      JOIN intents i3 ON i3.id = s3.intent_id
                      WHERE i3.transfer_id = p_transfer AND i3.attempt = p_attempt
                        AND NOT EXISTS (SELECT 1 FROM signed_transactions st3
                                        WHERE st3.intent_id = s3.intent_id))
    ORDER BY n.final_block_number DESC, n.id DESC
    LIMIT 1
$$;

CREATE OR REPLACE FUNCTION attempt_gate_open(
    p_network TEXT, p_purpose TEXT, p_scope TEXT, p_reference BYTEA, p_attempt INTEGER
) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT CASE
        WHEN p_attempt = 1 THEN TRUE
        WHEN p_reference IS NULL OR p_attempt < 1 THEN FALSE
        ELSE (
            WITH previous AS (
                SELECT i.id FROM intents i
                WHERE i.network = p_network AND i.purpose = p_purpose
                  AND i.reference_scope IS NOT DISTINCT FROM p_scope
                  AND i.on_chain_reference = p_reference AND i.attempt = p_attempt - 1
            ), lane AS (
                SELECT st.id FROM signed_transactions st
                JOIN previous p ON p.id = st.intent_id
            )
            SELECT EXISTS (SELECT 1 FROM lane)
               AND NOT EXISTS (SELECT 1 FROM lane l WHERE NOT EXISTS
                               (SELECT 1 FROM non_execution_proofs n
                                WHERE n.signed_transaction_id = l.id))
               AND NOT EXISTS (SELECT 1 FROM signatures s
                               JOIN previous p ON p.id = s.intent_id
                               WHERE NOT EXISTS (SELECT 1 FROM signed_transactions st
                                                 WHERE st.intent_id = s.intent_id))
        )
    END
$$;

-- Only an endpoint records signed bytes, observations and outcomes.
REVOKE INSERT ON signed_transactions, observations, observation_outcomes FROM bezent_service;
GRANT INSERT ON signed_transactions, observations, observation_outcomes TO bezent_endpoint;

REVOKE EXECUTE ON FUNCTION
    -- 0018_kreivo_endpoint, 0019_endpoint_role
    observation_ranges_guard_insert(),
    non_execution_proofs_guard_insert(),
    -- 0020_signal_writers
    bezent_writer_holds(TEXT, OID),
    event_log_guard_signal(),
    observations_guard_writer(),
    observation_outcomes_guard_writer()
FROM PUBLIC;
GRANT EXECUTE ON FUNCTION bezent_writer_holds(TEXT, OID) TO bezent_service;

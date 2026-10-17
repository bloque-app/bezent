-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): 0017's functions and the grants before 0021.
GRANT EXECUTE ON FUNCTION
    observation_ranges_guard_insert(),
    non_execution_proofs_guard_insert(),
    bezent_writer_holds(TEXT, OID),
    event_log_guard_signal(),
    observations_guard_writer(),
    observation_outcomes_guard_writer()
TO PUBLIC;
REVOKE EXECUTE ON FUNCTION bezent_writer_holds(TEXT, OID) FROM bezent_service;
REVOKE INSERT ON signed_transactions, observations, observation_outcomes FROM bezent_endpoint;
GRANT INSERT ON signed_transactions, observations, observation_outcomes TO bezent_service;

CREATE OR REPLACE FUNCTION attempt_proof(
    p_transfer BIGINT, p_attempt INTEGER,
    OUT network TEXT, OUT final_block_number BIGINT, OUT final_block_hash BYTEA
)
    LANGUAGE plpgsql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF to_regclass('public.non_execution_proofs') IS NULL THEN
        RETURN;
    END IF;
    EXECUTE 'SELECT n.network, n.final_block_number, n.final_block_hash '
         || 'FROM non_execution_proofs n '
         || 'JOIN signed_transactions st ON st.id = n.signed_transaction_id '
         || 'JOIN intents i ON i.id = st.intent_id '
         || 'WHERE i.transfer_id = $1 AND i.attempt = $2 '
         || 'AND NOT EXISTS (SELECT 1 FROM signed_transactions st2 '
         ||                 'JOIN intents i2 ON i2.id = st2.intent_id '
         ||                 'WHERE i2.transfer_id = $1 AND i2.attempt = $2 '
         ||                 'AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n2 '
         ||                                 'WHERE n2.signed_transaction_id = st2.id)) '
         || 'ORDER BY n.final_block_number DESC, n.id DESC LIMIT 1'
        INTO network, final_block_number, final_block_hash
        USING p_transfer, p_attempt;
END
$$;

-- REQ-EO-6: whether attempt `p_attempt` may be recorded or signed in its lane (network, purpose,
-- reference scope, on-chain reference). Attempt 1 always; attempt n+1 only with the same reference,
-- once attempt n has at least one signed transaction in the lane and every one of them is proven
-- not executed. Attempt n itself could only be recorded once n-1 was proven, so every earlier one
-- is proven too. SECURITY DEFINER: the signer may read intents but not the proofs.
CREATE OR REPLACE FUNCTION attempt_gate_open(
    p_network TEXT, p_purpose TEXT, p_scope TEXT, p_reference BYTEA, p_attempt INTEGER
) RETURNS BOOLEAN
    LANGUAGE plpgsql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    proven BOOLEAN;
BEGIN
    IF p_attempt = 1 THEN
        RETURN TRUE;
    END IF;
    IF p_reference IS NULL OR p_attempt < 1 OR to_regclass('public.non_execution_proofs') IS NULL THEN
        RETURN FALSE;
    END IF;
    EXECUTE 'WITH lane AS (SELECT st.id FROM signed_transactions st '
         ||               'JOIN intents i ON i.id = st.intent_id '
         ||               'WHERE i.network = $1 AND i.purpose = $2 '
         ||               'AND i.reference_scope IS NOT DISTINCT FROM $3 '
         ||               'AND i.on_chain_reference = $4 AND i.attempt = $5) '
         || 'SELECT EXISTS (SELECT 1 FROM lane) '
         || 'AND NOT EXISTS (SELECT 1 FROM lane l WHERE NOT EXISTS '
         ||                 '(SELECT 1 FROM non_execution_proofs n WHERE n.signed_transaction_id = l.id))'
        INTO proven
        USING p_network, p_purpose, p_scope, p_reference, p_attempt - 1;
    RETURN proven;
END
$$;


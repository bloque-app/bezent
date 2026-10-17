-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (F-003, verification review of PR #32): the functions of 0014 and 0015
-- back to PostgreSQL's default grant, and the proof rules back to 0014's.
REVOKE EXECUTE ON FUNCTION one_business_day_after(TIMESTAMPTZ) FROM bezent_service, bezent_operator;
REVOKE EXECUTE ON FUNCTION attempt_gate_open(TEXT, TEXT, TEXT, BYTEA, INTEGER) FROM bezent_signer;
REVOKE EXECUTE ON FUNCTION attempt_proof(BIGINT, INTEGER), attempts_unproven(BIGINT) FROM bezent_core;
GRANT EXECUTE ON FUNCTION
    attempt_proof(BIGINT, INTEGER),
    attempt_gate_open(TEXT, TEXT, TEXT, BYTEA, INTEGER),
    intents_guard_attempt(),
    signatures_from_intent_attempt(),
    transfers_redeemer_unchanged(),
    manual_approvals_second_person(),
    attempt_milestones_guard_insert(),
    transfer_edge(transfer_state, transfer_state),
    resolution_moves_value(transfer_state),
    transitions_apply_lifecycle(),
    one_business_day_after(TIMESTAMPTZ),
    transfers_destination_reference_unchanged(),
    transfer_attempts_closed(BIGINT),
    attempts_unproven(BIGINT),
    intents_guard_attempt_set(),
    signatures_attempt_set_open(),
    transitions_refund_every_attempt_proven(),
    manual_actions_by_session(),
    manual_approvals_by_session()
TO PUBLIC;

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
         || 'WHERE i.transfer_id = $1 AND i.attempt = $2 ORDER BY n.id LIMIT 1'
        INTO network, final_block_number, final_block_hash
        USING p_transfer, p_attempt;
END
$$;

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
    EXECUTE 'SELECT EXISTS (SELECT 1 FROM non_execution_proofs n '
         || 'JOIN signed_transactions st ON st.id = n.signed_transaction_id '
         || 'JOIN intents i ON i.id = st.intent_id '
         || 'WHERE i.network = $1 AND i.purpose = $2 AND i.reference_scope IS NOT DISTINCT FROM $3 '
         || 'AND i.on_chain_reference = $4 AND i.attempt = $5)'
        INTO proven
        USING p_network, p_purpose, p_scope, p_reference, p_attempt - 1;
    RETURN proven;
END
$$;

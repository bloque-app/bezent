-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-003, verification review of PR #32 (F1, F2). Additive: 0014_lifecycle and 0015_lifecycle_review
-- are left as they were. Numbered 0017 because 0016 is taken by PR #31's branch (F-007), so neither
-- PR renumbers the other.
--
-- F2 (INV-8 "every payout attempt", REQ-EO-6): an attempt is proven not executed only when EVERY
--     signed transaction recorded for it has a proof of non-execution at finality, never when any
--     one has. One attempt can have several signed transactions: an order prepared again once its
--     era passed unsigned gets a second signing intent, and one intent can be signed into more than
--     one transaction. A proof for the dead one says nothing about the live one. The store no
--     longer leans on the signer signing once per (lane, attempt) for this (issue #24).
-- F1: PostgreSQL grants EXECUTE on every new function to PUBLIC. Every function 0014 and 0015 made
--     loses that grant here, and only the roles that call them get it back. Above all
--     transfer_attempts_closed, which runs SELECT ... FOR SHARE on transfers as the owner: any login
--     could otherwise hold row locks on every transfer and stall the core and every refund.
--     Trigger functions need no EXECUTE grant to fire, and the SECURITY DEFINER ones call the
--     others as the owner.

-- ---------------------------------------------------------------------------------------------
-- F2: every signed transaction of the attempt, proven.

-- The proof of non-execution of a transfer's attempt, once every signed transaction recorded for
-- it (through any intent of that transfer and attempt) has one: the latest of those proofs, the
-- final block at which the whole attempt stands proven (REQ-NX-2). Every column NULL while any
-- signed transaction of the attempt is unproven, or none is recorded.
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

-- attempts_unproven (0015) reads attempt_proof, so it now counts an attempt as unproven while any
-- of its signed transactions is.

-- ---------------------------------------------------------------------------------------------
-- F1: no function of 0014 or 0015 is PUBLIC's.

REVOKE EXECUTE ON FUNCTION
    -- 0014_lifecycle
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
    -- 0015_lifecycle_review
    transfers_destination_reference_unchanged(),
    transfer_attempts_closed(BIGINT),
    attempts_unproven(BIGINT),
    intents_guard_attempt_set(),
    signatures_attempt_set_open(),
    transitions_refund_every_attempt_proven(),
    manual_actions_by_session(),
    manual_approvals_by_session()
FROM PUBLIC;

-- The core reads an attempt's proof and its unproven attempts (review evidence, the refund
-- decision). Its login holds bezent_core besides bezent_service; the endpoints and ingress, which
-- hold bezent_service only, do not need them.
GRANT EXECUTE ON FUNCTION attempt_proof(BIGINT, INTEGER), attempts_unproven(BIGINT) TO bezent_core;

-- The signer checks the gate before it signs (REQ-EO-6); it learns only open or closed.
GRANT EXECUTE ON FUNCTION attempt_gate_open(TEXT, TEXT, TEXT, BYTEA, INTEGER) TO bezent_signer;

-- The views `reviews` and `unassociated_arrivals` call it, and a view's functions run with the
-- privileges of whoever reads the view.
GRANT EXECUTE ON FUNCTION one_business_day_after(TIMESTAMPTZ) TO bezent_service, bezent_operator;

-- transfer_attempts_closed, transfer_edge and resolution_moves_value are called only by the
-- owner's SECURITY DEFINER triggers; the rest are trigger functions. No role gets them.

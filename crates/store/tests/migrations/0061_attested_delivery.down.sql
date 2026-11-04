-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reverts 0061_attested_delivery, in tests only.

DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('transitions_apply_lifecycle()'::regprocedure);
BEGIN
    EXECUTE replace(src, $new$WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt)
           AND NOT bezent_attested_hand_over_open(t.id) THEN$new$,
        $old$WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt) THEN$old$);
END
$migration$;

DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('transfers_guard_writer()'::regprocedure);
BEGIN
    EXECUTE replace(src, $new$WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order')
                   AND NOT (bezent_attested_hand_over_open(NEW.refund_of)
                            AND EXISTS (SELECT 1 FROM transitions x
                                        WHERE x.transfer_id = NEW.refund_of
                                          AND x.attempt IS NOT NULL)) THEN$new$,
        $old$WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order') THEN$old$);
END
$migration$;

DROP FUNCTION bezent_attested_hand_over_open(BIGINT);

CREATE OR REPLACE FUNCTION attempts_unproven(p_transfer BIGINT) RETURNS INTEGER[]
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
SELECT COALESCE(array_agg(k.attempt ORDER BY k.attempt), '{}')
FROM (SELECT DISTINCT attempt FROM intents WHERE transfer_id = p_transfer) k
WHERE (SELECT p.network FROM attempt_proof(p_transfer, k.attempt) p) IS NULL
  AND NOT EXISTS (SELECT 1 FROM unsigned_attempt_cancellations c
                  WHERE c.transfer_id = p_transfer AND c.attempt = k.attempt)
$$;

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

-- Reports of non-execution may exist, and the table is append-only: the narrower check is added
-- NOT VALID, so it binds new rows and leaves those alone.
ALTER TABLE attested_reports DROP CONSTRAINT attested_reports_kind_check;
ALTER TABLE attested_reports ADD CONSTRAINT attested_reports_kind_check
    CHECK (kind IN ('arrival', 'payout', 'positions', 'reversal', 'restoration')) NOT VALID;

-- The record function as 0060 made it: 0061's four edits, undone in the opposite order.
DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('bezent_record_attested_observation(bigint)'::regprocedure);
    edits TEXT[][] := ARRAY[
        ARRAY[$new$o_sender, o_to, o_ref, r.kind <> 'payout-not-executed', o_detail, o_kind)$new$,
              $old$o_sender, o_to, o_ref, TRUE, o_detail, o_kind)$old$],
        ARRAY[$new$            o_ref := COALESCE(t.destination_reference, t.on_chain_reference);
            IF o_ref IS NULL THEN
                RAISE EXCEPTION 'report % of %: its transfer has no reference to carry yet (REQ-OT-2)',
                    r.report_id, r.network
                    USING ERRCODE = 'BZ001';
            END IF;
        WHEN 'payout-not-executed' THEN
            o_kind := 'other';
            o_event := 'PayoutNotExecuted';
            o_amount := NULL;
            o_detail := o_detail || jsonb_build_object(
                'bridge_reference', r.body -> 'bridge_reference', 'attempt', r.body -> 'attempt');$new$,
              $old$            o_ref := t.destination_reference;$old$],
        ARRAY[$new$                RAISE EXCEPTION 'report % of %: its payout is of no transfer of % there (REQ-OT-2)',$new$,
              $old$                RAISE EXCEPTION 'report % of %: its payout is of no withdrawal of % there (REQ-OT-2)',$old$],
        ARRAY[$new$              AND (x.kind = 'withdrawal' OR (x.kind = 'refund' AND x.refund_of IS NULL))
              AND x.destination_network = r.network$new$,
              $old$              AND x.kind = 'withdrawal' AND x.destination_network = r.network$old$]
    ];
    i INT;
BEGIN
    FOR i IN 1 .. array_length(edits, 1) LOOP
        IF position(edits[i][1] IN src) = 0 THEN
            RAISE EXCEPTION 'bezent_record_attested_observation: not found: %', edits[i][1];
        END IF;
        src := replace(src, edits[i][1], edits[i][2]);
    END LOOP;
    EXECUTE src;
END
$migration$;

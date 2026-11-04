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

ALTER TABLE attested_reports DROP CONSTRAINT attested_reports_kind_check;
ALTER TABLE attested_reports ADD CONSTRAINT attested_reports_kind_check
    CHECK (kind IN ('arrival', 'payout', 'positions', 'reversal', 'restoration'));

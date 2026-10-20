-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #25, the cancellation of an unsigned attempt.
CREATE OR REPLACE FUNCTION attempts_unproven(p_transfer BIGINT) RETURNS INTEGER[]
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
SELECT COALESCE(array_agg(k.attempt ORDER BY k.attempt), '{}')
FROM (SELECT DISTINCT attempt FROM intents WHERE transfer_id = p_transfer) k
WHERE (SELECT p.network FROM attempt_proof(p_transfer, k.attempt) p) IS NULL
$$;

DROP TRIGGER signed_transactions_refuse_cancelled_attempt ON signed_transactions;
DROP TRIGGER signatures_refuse_cancelled_attempt ON signatures;
DROP TRIGGER intents_refuse_cancelled_attempt ON intents;
DROP FUNCTION refuse_cancelled_attempt();
DROP TABLE unsigned_attempt_cancellations;
DROP FUNCTION unsigned_attempt_cancellations_guard();

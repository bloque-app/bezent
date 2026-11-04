-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-014 (issue #14; REQ-ST-6, AC-H3.1; PLAN DEC-18): fee positions become observable.
--
-- A fee position is the native token an address needs to pay network costs (SPEC glossary): the
-- network's delivery account's native balance. An endpoint reads it at each final head it records
-- and writes it here, beside the head, as it does a reserve reading (0039). A separate table, not
-- a `reserve_readings` kind: the reserve check takes any unknown reserve reading as an incomplete
-- evaluation and pauses the asset (REQ-IC-2), and a fee balance that could not be read must never
-- do that. A reading is the balance both providers agree on at the final block, or unknown with
-- its reason (DEC-12): never the higher, never the lower, never a guess (REQ-NX-3).
--
-- Only the EVM endpoint records one in this change; the Kreivo endpoint records none (which amount
-- of a Substrate account is the fee position is not decided: F-014 NOTES, Open 2).
--
-- `ops_observables()` gains two rows per network, `fee_position_amount` and
-- `fee_position_age_seconds`, and is otherwise the function of 0090. It is replaced whole, because
-- a function body cannot be amended: a later migration that replaces it must start from this body.
-- A network with no reading shows NULL, which is "unknown", not "absent".

CREATE TABLE fee_position_readings (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network        TEXT NOT NULL CHECK (network <> ''),
    block_number   BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash     BYTEA NOT NULL CHECK (length(block_hash) > 0),
    -- The delivery account, as the store spells it. Never in a metric (ops_observables()).
    holder         TEXT NOT NULL CHECK (holder <> ''),
    -- The native balance in the network's smallest unit. NULL is unknown, with its reason.
    amount         minor_units,
    unknown_reason TEXT CHECK (unknown_reason <> ''),
    -- What each provider answered.
    detail         JSONB NOT NULL DEFAULT '{}',
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT fee_position_readings_once UNIQUE (network, block_number, holder),
    CONSTRAINT fee_position_readings_known_or_why CHECK ((amount IS NULL) = (unknown_reason IS NOT NULL))
);

COMMENT ON TABLE fee_position_readings IS
    'REQ-ST-6, AC-H3.1, DEC-18: the native balance of a network''s delivery account at a final head '
    'its endpoint recorded. NULL amount is unknown, with its reason. Append-only; only the '
    'network''s endpoint records it.';

CREATE TRIGGER fee_position_readings_append_only
    BEFORE UPDATE OR DELETE ON fee_position_readings
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-ST-6, INV-16');
CREATE TRIGGER fee_position_readings_no_truncate
    BEFORE TRUNCATE ON fee_position_readings
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-6, INV-16');

CREATE FUNCTION fee_position_readings_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT observation_writer_covers(NEW.network, TG_RELID) THEN
        RAISE EXCEPTION 'fee position on % refused: only that network''s endpoint records its readings (REQ-OB-6, INV-8)',
            NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM final_heads h
                   WHERE h.network = NEW.network AND h.block_number = NEW.block_number
                     AND h.block_hash = NEW.block_hash) THEN
        RAISE EXCEPTION 'fee position on % refused: block % with that hash is not a recorded final head (REQ-OB-5, REQ-ST-6)',
            NEW.network, NEW.block_number
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER fee_position_readings_guard_insert
    BEFORE INSERT ON fee_position_readings
    FOR EACH ROW EXECUTE FUNCTION fee_position_readings_guard_insert();
-- 0035 and 0049: a login holding an endpoint role writes only its own networks' rows.
CREATE TRIGGER fee_position_readings_evm_endpoint_guard
    BEFORE INSERT ON fee_position_readings
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER fee_position_readings_endpoint_scope
    BEFORE INSERT ON fee_position_readings
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();

REVOKE ALL ON fee_position_readings FROM PUBLIC;
-- The EVM endpoint reads (to know a head is read) and appends. Nothing else holds a privilege: the
-- core and the operators read it through ops_observables(), a definer.
GRANT SELECT, INSERT ON fee_position_readings TO bezent_evm_endpoint;

-- ---------------------------------------------------------------------------------------------
-- ops_observables(): 0090's function with the two fee position rows.

CREATE OR REPLACE FUNCTION ops_observables()
    RETURNS TABLE (metric TEXT, asset TEXT, network TEXT, direction TEXT, value NUMERIC)
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
WITH nonfinal AS (
    SELECT t.id, t.asset, t.state, t.condition, t.created_at, t.kind AS direction,
           CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END AS network
    FROM transfers t
    WHERE NOT is_terminal_state(t.state)
),
unknown AS (
    SELECT n.id, n.asset, n.network, n.direction, a.attempt,
           min(s.recorded_at) AS since
    FROM nonfinal n
    CROSS JOIN LATERAL unnest(attempts_unproven(n.id)) AS a(attempt)
    JOIN intents i ON i.transfer_id = n.id AND i.attempt = a.attempt
    JOIN signed_transactions s ON s.intent_id = i.id AND s.attempt = a.attempt
    WHERE n.state IN ('Delivering', 'Needs-Review')
    GROUP BY n.id, n.asset, n.network, n.direction, a.attempt
),
latest_evaluation AS (
    SELECT DISTINCT ON (asset) asset, outcome, evaluated_at
    FROM reserve_evaluations
    ORDER BY asset, evaluated_at DESC, id DESC
),
latest_reserve AS (
    SELECT DISTINCT ON (r.network, r.subject, r.holder)
           r.network, r.subject, r.amount
    FROM reserve_readings r
    WHERE r.kind = 'reserve'
    ORDER BY r.network, r.subject, r.holder, r.block_number DESC, r.id DESC
)
-- The age of the oldest non-final transfer, per route.
SELECT 'oldest_nonfinal_transfer_age_seconds'::text AS metric,
       asset, network, direction,
       extract(epoch FROM now() - min(created_at))::numeric AS value
FROM nonfinal GROUP BY asset, network, direction
UNION ALL
-- A declared route with nothing in flight reads zero: a silent route is not a missing one.
SELECT 'oldest_nonfinal_transfer_age_seconds', r.asset, r.network, r.direction, 0
FROM routes r
WHERE NOT EXISTS (SELECT 1 FROM nonfinal n
                  WHERE n.asset = r.asset AND n.network IS NOT DISTINCT FROM r.network
                    AND n.direction = r.direction)
UNION ALL
SELECT 'nonfinal_transfers', asset, network, direction, count(*)::numeric
FROM nonfinal GROUP BY asset, network, direction
UNION ALL
SELECT 'unknown_legs', r.asset, r.network, r.direction,
       (SELECT count(*) FROM unknown u
        WHERE u.asset = r.asset AND u.network IS NOT DISTINCT FROM r.network
          AND u.direction = r.direction)::numeric
FROM routes r
UNION ALL
SELECT 'unknown_legs_oldest_age_seconds', r.asset, r.network, r.direction,
       coalesce((SELECT extract(epoch FROM now() - min(u.since)) FROM unknown u
                 WHERE u.asset = r.asset AND u.network IS NOT DISTINCT FROM r.network
                   AND u.direction = r.direction), 0)::numeric
FROM routes r
UNION ALL
-- Observer lag, estimated (see above): NULL when the network has never recorded a final head.
SELECT 'observer_lag_estimate_seconds', NULL, n.network, NULL,
       CASE WHEN h.block_time IS NULL THEN NULL
            ELSE greatest(0, extract(epoch FROM now() - h.block_time) - n.finality_seconds)::numeric
       END
FROM networks n
LEFT JOIN LATERAL (SELECT f.block_time FROM final_heads f WHERE f.network = n.network
                    ORDER BY f.block_number DESC LIMIT 1) h ON true
UNION ALL
SELECT 'open_gaps', NULL, n.network, NULL,
       (SELECT count(*) FROM observation_gaps g WHERE g.network = n.network AND NOT g.filled)::numeric
FROM networks n
UNION ALL
-- The reserve equation: 0 holds, 1 diverges, 2 could not be made; and how long ago it was made.
SELECT 'reserve_equation_outcome', asset, NULL, NULL,
       CASE outcome WHEN 'holds' THEN 0 WHEN 'diverges' THEN 1 ELSE 2 END::numeric
FROM latest_evaluation
UNION ALL
SELECT 'reserve_equation_age_seconds', asset, NULL, NULL,
       extract(epoch FROM now() - evaluated_at)::numeric
FROM latest_evaluation
UNION ALL
-- Reserve per network, in the origin token's minor units, from the latest reading of each holder.
-- NULL when a reading is unknown.
SELECT 'reserve_amount', a.asset, r.network, NULL,
       CASE WHEN bool_or(r.amount IS NULL) THEN NULL ELSE sum(r.amount) END
FROM latest_reserve r
JOIN admitted_tokens a ON a.network = r.network AND a.token = r.subject
GROUP BY a.asset, r.network
UNION ALL
-- Transfers held for a fee position that cannot pay (CND-FeeShort), the proxy for REQ-ST-6's fee
-- positions until a balance is read.
SELECT 'fee_short_transfers', asset, network, direction, count(*)::numeric
FROM nonfinal WHERE condition = 'CND-FeeShort' GROUP BY asset, network, direction
UNION ALL
-- Fee positions (REQ-ST-6, AC-H3.1; migration 0110): the native balance of the network's delivery
-- account at the latest final head an endpoint recorded it at, in the network's smallest unit,
-- exact. One row per declared network. NULL (unknown, never zero) when no endpoint has recorded a
-- reading or the latest one could not be read; the age is NULL only when none was ever recorded.
SELECT 'fee_position_amount', NULL, n.network, NULL, f.amount
FROM networks n
LEFT JOIN LATERAL (SELECT r.amount FROM fee_position_readings r WHERE r.network = n.network
                    ORDER BY r.block_number DESC, r.id DESC LIMIT 1) f ON true
UNION ALL
SELECT 'fee_position_age_seconds', NULL, n.network, NULL,
       extract(epoch FROM now() - f.recorded_at)::numeric
FROM networks n
LEFT JOIN LATERAL (SELECT r.recorded_at FROM fee_position_readings r WHERE r.network = n.network
                    ORDER BY r.block_number DESC, r.id DESC LIMIT 1) f ON true
UNION ALL
-- Undelivered notifications (REQ-ST-5): pending, and dead (retries spent).
SELECT 'undelivered_notifications', NULL, NULL, NULL,
       (SELECT count(*) FROM webhook_deliveries WHERE state = 'pending')::numeric
UNION ALL
SELECT 'undelivered_notifications_oldest_age_seconds', NULL, NULL, NULL,
       coalesce((SELECT extract(epoch FROM now() - min(created_at)) FROM webhook_deliveries
                 WHERE state = 'pending'), 0)::numeric
UNION ALL
SELECT 'dead_notifications', NULL, NULL, NULL,
       (SELECT count(*) FROM webhook_deliveries WHERE state = 'dead')::numeric
UNION ALL
-- Arrivals in CND-Unassociated, with the age of the oldest.
SELECT 'unassociated_arrivals', admitted_for, network, NULL, count(*)::numeric
FROM unassociated_arrivals GROUP BY admitted_for, network
UNION ALL
SELECT 'unassociated_arrivals_oldest_age_seconds', admitted_for, network, NULL,
       extract(epoch FROM now() - min(final_at))::numeric
FROM unassociated_arrivals GROUP BY admitted_for, network
UNION ALL
-- Reviews open (the operations function's service level, REQ-RV-1) and pauses in force.
SELECT 'reviews_open', NULL, NULL, NULL, (SELECT count(*) FROM reviews)::numeric
UNION ALL
SELECT 'reviews_oldest_age_seconds', NULL, NULL, NULL,
       coalesce((SELECT extract(epoch FROM now() - min(entered_at)) FROM reviews), 0)::numeric
UNION ALL
SELECT 'pauses_in_force', NULL, NULL, NULL, (SELECT count(*) FROM pauses)::numeric
$$;

COMMENT ON FUNCTION ops_observables() IS
    'REQ-ST-6, AC-H3.1: the continuously observable quantities, one row per metric and subject. '
    'Counts, ages, outcomes, reserve amounts and fee positions only; no row of any table. A fee '
    'position is the delivery account''s native balance (0110); fee_short_transfers stays as the '
    'transfers it already held. NULL value = not known.';

REVOKE EXECUTE ON FUNCTION ops_observables() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION ops_observables() TO bezent_core, bezent_operator;

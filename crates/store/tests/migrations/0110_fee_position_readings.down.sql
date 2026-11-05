-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #14, fee positions. 0090's function is restored.
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


DROP TABLE fee_position_readings;
DROP FUNCTION fee_position_readings_guard_insert();
COMMENT ON FUNCTION ops_observables() IS
    'REQ-ST-6, AC-H3.1: the continuously observable quantities, one row per metric and subject. '
    'Counts, ages, outcomes and reserve amounts only; no row of any table. Fee positions are not '
    'here (no balance is read); fee_short_transfers is their proxy. NULL value = not known.';

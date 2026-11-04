-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-014: the observables of REQ-ST-6 (AC-H3.1), as one read-only function.
--
-- REQ-ST-6 asks that these be observable continuously: the age of the oldest non-final transfer
-- per route; the count and age of unknown legs; observation lag and open gaps per network; the
-- reserve equation per asset; reserve per network; fee positions; undelivered notifications; and
-- arrivals in CND-Unassociated with their age. Each already lives in the store, in a table or a
-- view of its own; none was one place a person or an alarm could read. `ops_observables()` is that
-- place. It adds no table, changes no row and holds nothing of its own.
--
-- One row per (metric, asset, network, direction); a column that does not apply is NULL, and a
-- metric that cannot be read (no final head yet, an unknown reserve amount) has a NULL value: it
-- is "not known", never a zero (REQ-NX-3).
--
--   * Routes. A deposit belongs to the route of its source network (NULL until it is bound), a
--     withdrawal to the route of its destination network, a refund to its destination network
--     under direction `refund`.
--   * Unknown legs. An attempt of a transfer in `Delivering` or `Needs-Review` that has a signed
--     transaction and no proof of non-execution (`attempts_unproven`), aged from the first signed
--     transaction of the attempt. Once the leg is seen included the transfer leaves `Delivering`
--     and the leg is no longer unknown. A transfer in review for another reason may count its
--     executed attempt here: the count errs toward showing a leg, never toward hiding one.
--   * Observer lag. Seconds since the network's endpoint recorded its latest final head
--     (`final_heads.seen_at`), which is what NFR-6 bounds at 60 s.
--   * Fee positions are NOT in this view: no service reads a delivery account's balance yet. The
--     transfers held in CND-FeeShort are (`fee_short_transfers`), and are the only signal of a
--     short fee position the store has. F-014 NOTES lists the gap.
--
-- A function and not a view: it runs as its owner (SECURITY DEFINER), so a reader needs no grant
-- on the tables it counts (webhook_deliveries is the webhook dispatcher's alone) and none on
-- `attempts_unproven`. It exposes counts, ages, outcomes and reserve amounts, and no row of a
-- table: no reference, destination, URL or secret.

CREATE FUNCTION ops_observables()
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
-- Observation lag: NULL when the network has never recorded a final head.
SELECT 'observer_lag_seconds', NULL, n.network, NULL,
       extract(epoch FROM now() - (SELECT max(h.seen_at) FROM final_heads h
                                   WHERE h.network = n.network))::numeric
FROM networks n
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

COMMENT ON FUNCTION ops_observables() IS
    'REQ-ST-6, AC-H3.1: the continuously observable quantities, one row per metric and subject. '
    'Counts, ages, outcomes and reserve amounts only; no row of any table. Fee positions are not '
    'here (no balance is read); fee_short_transfers is their proxy. NULL value = not known.';

REVOKE EXECUTE ON FUNCTION ops_observables() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION ops_observables() TO bezent_core, bezent_operator;

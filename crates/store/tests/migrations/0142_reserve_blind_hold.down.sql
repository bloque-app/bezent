-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): amendment 0028, the reserve check's blind hold. Its holds go
-- with it; the guard, the unpause request and the constraints return to 0095 and 0091.
REVOKE DELETE ON pauses FROM bezent_core;
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

CREATE OR REPLACE FUNCTION pause_holds_intent(p_intent BIGINT) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1
        FROM intents i
        JOIN transfers t ON t.id = i.transfer_id
        WHERE i.id = p_intent
          AND i.purpose IN ('mint', 'payout')
          AND t.kind IN ('deposit', 'withdrawal')
          AND NOT EXISTS (
              SELECT 1 FROM intents j JOIN signatures s ON s.intent_id = j.id
              WHERE j.transfer_id = i.transfer_id AND j.attempt = i.attempt)
          AND EXISTS (
              SELECT 1 FROM pauses p
              WHERE (p.asset IS NULL OR p.asset = t.asset)
                AND (p.network IS NULL OR p.network =
                     CASE t.kind WHEN 'deposit' THEN t.source_network
                                 ELSE t.destination_network END)
                AND (p.direction IS NULL OR p.direction = t.kind)))
$$;
DROP FUNCTION reserve_check_holds(TEXT);
DROP TRIGGER reserve_evaluations_stamp ON reserve_evaluations;
DROP FUNCTION reserve_evaluations_stamp();
DROP INDEX reserve_evaluations_latest_by_id;
DROP INDEX event_log_reserve_blind_pages;
DROP TRIGGER pauses_guard_delete ON pauses;
DROP FUNCTION pauses_guard_delete();
DELETE FROM pauses WHERE cause = 'blind';
DROP INDEX pauses_one_hold_per_asset;
ALTER TABLE pauses
    DROP CONSTRAINT pauses_held_heads_on_a_hold,
    DROP COLUMN held_heads,
    DROP COLUMN kind,
    DROP CONSTRAINT pauses_cause_names_its_reason,
    DROP CONSTRAINT pauses_cause_known,
    ADD CONSTRAINT pauses_cause_known CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2', 'INV-4', 'REQ-IC-4')),
    ADD CONSTRAINT pauses_cause_names_its_reason CHECK (
        (cause IS NULL AND evaluation_id IS NULL AND quarantine_id IS NULL AND reading_id IS NULL
            AND alert_seq IS NULL)
        OR (cause = 'REQ-BK-1' AND evaluation_id IS NOT NULL AND quarantine_id IS NULL
            AND reading_id IS NULL AND alert_seq IS NULL)
        OR (cause = 'REQ-IC-2' AND (evaluation_id IS NULL) <> (quarantine_id IS NULL)
            AND reading_id IS NULL AND alert_seq IS NULL)
        OR (cause = 'INV-4' AND evaluation_id IS NOT NULL AND reading_id IS NOT NULL
            AND quarantine_id IS NULL AND alert_seq IS NULL)
        OR (cause = 'REQ-IC-4' AND alert_seq IS NOT NULL
            AND evaluation_id IS NULL AND quarantine_id IS NULL AND reading_id IS NULL
            AND asset IS NULL AND network IS NULL AND direction IS NULL));
ALTER TABLE networks DROP COLUMN reserve_blind_page_seconds;
CREATE OR REPLACE FUNCTION operator_request_unpause(p_pause BIGINT, p_justification TEXT) RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    p pauses%ROWTYPE;
    rid BIGINT;
BEGIN
    PERFORM operator_require();
    SELECT * INTO p FROM pauses WHERE id = p_pause;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'unpause request refused: no pause %', p_pause USING ERRCODE = 'BZ001';
    END IF;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES ('unpause-request',
                'pause ' || p_pause,
                jsonb_build_object('pause_id', p.id, 'asset', p.asset, 'network', p.network,
                                   'direction', p.direction, 'cause', p.cause),
                p_justification)
        RETURNING id INTO rid;
    RETURN rid;
END
$$;
CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.alert_seq IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM event_log e WHERE e.seq = NEW.alert_seq AND e.kind = 'key-compromise') THEN
        RAISE EXCEPTION 'pause refused: it names no key-compromise alert (REQ-IC-4, AC-I1.2)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF NEW.asset IS NULL OR NEW.network IS NOT NULL OR NEW.direction IS NOT NULL
       OR NOT (
           (NEW.cause = 'REQ-BK-1' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'diverges'))
           OR (NEW.cause = 'REQ-IC-2' AND NEW.quarantine_id IS NULL AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'incomplete'))
           OR (NEW.cause = 'REQ-IC-2' AND NEW.evaluation_id IS NULL AND EXISTS (
               SELECT 1 FROM book_quarantine q
               WHERE q.id = NEW.quarantine_id AND q.asset = NEW.asset))
           OR (NEW.cause = 'INV-4' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset)
               AND CASE WHEN NEW.cause = 'INV-4'
                        THEN pause_reading_is_supply_mismatch(NEW.asset, NEW.reading_id)
                        ELSE false END)) THEN
        RAISE EXCEPTION 'pause refused: the core pauses only an asset''s routes, on a divergence of that asset, an evaluation of it that could not be made, an observation of it the book could not post, or a supply mismatch of it (REQ-BK-1, REQ-IC-2, INV-4)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

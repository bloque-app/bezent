-- SPDX-License-Identifier: GPL-3.0-or-later
-- Amendment 0028 (@pandres95's ruling of 2026-11-07, in chat: "Yes, write the amendment and build
-- it."): when the reserve check cannot see, the asset's routes are held, not paused, and the hold
-- lifts itself once an evaluation holds again. When the money differs it is a pause, as before,
-- lifted only by two operators (REQ-GV-2). Additive.
--
--   * pauses gains the cause `blind` and a generated `kind`: `hold` for a blind row, `pause` for
--     every other. A hold is a row of `pauses` so that everything that refuses new work during a
--     pause refuses it during a hold, with no reader changed: the core's gate, the ingress's route
--     limits and route listing, the signer's `pause_holds_intent` (0125, amendment 0022 A1b), and
--     the time a payout waits under it (amendment 0019 A1d). A hold is asset-scoped, names the
--     evaluation it began at (its `evaluated_at` is when the check went blind), and there is at
--     most one per asset.
--   * The core may insert a hold only naming an evaluation of that asset that did not hold.
--   * The core may delete a row of `pauses` only if it is a hold and the latest evaluation of its
--     asset, by id, holds, was made (names the book entry it cut at) and is later than the one the
--     hold names. Every other row (a divergence,
--     a quarantine, a supply mismatch, a compromise, an operator's pause, and a REQ-IC-2 stall pause
--     recorded before this migration) is lifted only as before: two operators (0091). PostgreSQL
--     has no column-level DELETE, so the trigger is the restriction; the owner is exempt, so
--     `operator_approve_unpause` (SECURITY DEFINER) is unchanged.
--   * The core no longer inserts a REQ-IC-2 pause naming an evaluation: an evaluation that could
--     not be made is a hold now. Rows of that form already recorded stay valid and stay pauses.
--   * `operator_request_unpause` refuses a hold: it lifts itself, and lifting it by hand would let
--     new work start while the check cannot see.
--   * networks.reserve_blind_page_seconds: how long an asset may be held blind before a person is
--     paged (default 600; the asset's bound is the smallest of its networks'). A finality stall
--     pages at once, whatever this says.
--   * event_log gains a partial index for the core's `reserve-blind` page, which it looks up by the
--     hold it names so that each hold pages once.
--   * A hold records `held_heads`: each of the asset's networks' latest recorded final head (number
--     and block time) and read watermark when it was placed, apart from the evaluation's points, so
--     "the final head has not moved since the hold began" holds for a hold placed at a cold start
--     or by a failed evaluation as well.
--   * `reserve_check_holds(asset)`: whether the asset's latest evaluation, by id, holds, was made
--     (it names the book entry it cut at) and was recorded within 180 s (`recorded_at`, which the
--     store now stamps for every writer but the owner). The core's gate and the signer's `pause_holds_intent`
--     treat an asset for which it is false as held, whatever `pauses` says: an asset never
--     evaluated, one declared while the core runs, and the moment after a restart are held until
--     the check has seen them. The hold row stays the record, and what pages.
--   * The latest evaluation is the latest by id here and in the core (a new index); the evaluation
--     and the record of a failure take a per-asset advisory lock first, so a hold and its lift
--     follow the order the evaluations are recorded in.
--   * `ops_observables()`: `pauses_in_force` counts pauses only; `holds_in_force` and
--     `hold_age_seconds` count holds.

ALTER TABLE networks
    ADD COLUMN reserve_blind_page_seconds INTEGER NOT NULL DEFAULT 600
        CONSTRAINT networks_reserve_blind_page_seconds_range
        CHECK (reserve_blind_page_seconds > 0 AND reserve_blind_page_seconds <= 3600);

COMMENT ON COLUMN networks.reserve_blind_page_seconds IS
    'Amendment 0028 (REQ-IC-2, NFR-5): how long the reserve check may be blind (no evaluation that holds) '
    'for an asset that uses this network before a person is paged; the asset''s bound is the smallest '
    'of its networks''. Default 600, at most 3600. A finality stall pages at once.';

ALTER TABLE pauses
    DROP CONSTRAINT pauses_cause_known,
    ADD CONSTRAINT pauses_cause_known
        CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2', 'INV-4', 'REQ-IC-4', 'blind')),
    DROP CONSTRAINT pauses_cause_names_its_reason,
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
            AND asset IS NULL AND network IS NULL AND direction IS NULL)
        OR (cause = 'blind' AND evaluation_id IS NOT NULL AND asset IS NOT NULL
            AND quarantine_id IS NULL AND reading_id IS NULL AND alert_seq IS NULL
            AND network IS NULL AND direction IS NULL)),
    ADD COLUMN kind TEXT NOT NULL
        GENERATED ALWAYS AS (CASE WHEN cause = 'blind' THEN 'hold' ELSE 'pause' END) STORED;

CREATE UNIQUE INDEX pauses_one_hold_per_asset ON pauses (asset) WHERE cause = 'blind';

ALTER TABLE pauses
    ADD COLUMN held_heads JSONB,
    ADD CONSTRAINT pauses_held_heads_on_a_hold
        CHECK ((held_heads IS NOT NULL) = (cause IS NOT DISTINCT FROM 'blind')
               AND (held_heads IS NULL OR jsonb_typeof(held_heads) = 'object'));

COMMENT ON COLUMN pauses.held_heads IS
    'Amendment 0028: for a hold, each of its asset''s networks'' latest recorded final head (number, '
    'block time) and read watermark when the hold was placed, so a head that has not moved since is '
    'a finality stall whatever its block time says. NULL on every other row.';

CREATE INDEX reserve_evaluations_latest_by_id ON reserve_evaluations (asset, id DESC);

-- Whether the reserve check has seen `p_asset`, and lately: its latest evaluation, by id, holds, was
-- made (it names the book entry it cut at), and was recorded within the last 180 s by the store's
-- clock. False for an asset never evaluated, and for one whose check has stopped: the check
-- evaluates each asset once a minute (NFR-5), so three missed evaluations mean it is not running
-- for the asset (a core that died, an evaluation that never records), and the asset is held,
-- whatever the core does. The store's clock (`recorded_at`, stamped below), not the core's
-- `evaluated_at`: it is the one the gate and the signer share. SECURITY DEFINER so that
-- `pause_holds_intent`, the signer's only read, and the core share one rule.
CREATE FUNCTION reserve_check_holds(p_asset TEXT) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT COALESCE((
        SELECT v.outcome = 'holds' AND v.book_entry_cut IS NOT NULL
               AND v.recorded_at >= now() - interval '180 seconds'
        FROM reserve_evaluations v WHERE v.asset = p_asset ORDER BY v.id DESC LIMIT 1), false)
$$;

COMMENT ON FUNCTION reserve_check_holds(TEXT) IS
    'Amendment 0028 (REQ-IC-2): whether the asset''s latest reserve evaluation, by id, holds, was '
    'made, and was recorded within 180 s (three of the check''s one-minute periods). An asset for which '
    'it is false is held: no new work starts and the signer signs no new mint or payout of it.';

-- The store stamps when an evaluation is recorded, for every writer but the owner (a restore keeps
-- what it restores): `reserve_check_holds` judges freshness by it, so no writer may date it.
CREATE FUNCTION reserve_evaluations_stamp() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        NEW.recorded_at := now();
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER reserve_evaluations_stamp
    BEFORE INSERT ON reserve_evaluations
    FOR EACH ROW EXECUTE FUNCTION reserve_evaluations_stamp();

-- Amendment 0022 A1b (0125), widened by amendment 0028: an unsigned mint or payout is also held
-- while the reserve check has not seen its asset.
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
          AND (EXISTS (
                  SELECT 1 FROM pauses p
                  WHERE (p.asset IS NULL OR p.asset = t.asset)
                    AND (p.network IS NULL OR p.network =
                         CASE t.kind WHEN 'deposit' THEN t.source_network
                                     ELSE t.destination_network END)
                    AND (p.direction IS NULL OR p.direction = t.kind))
               OR NOT reserve_check_holds(t.asset)))
$$;

COMMENT ON COLUMN pauses.kind IS
    'Amendment 0028: `hold` when the reserve check cannot see (cause `blind`): it refuses new work as a '
    'pause does and is lifted by the core when an evaluation holds; `pause` for every other row, lifted '
    'only by two operators (REQ-GV-2). Generated from the cause, so no writer can mislabel a row.';

CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- A compromise pause rests on a real alert, whoever writes it (only the store does, 0095; a
    -- service holds no privilege on the column).
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
           -- A divergence of this asset (REQ-BK-1).
           (NEW.cause = 'REQ-BK-1' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'diverges'))
           -- The check cannot see this asset: an evaluation of it that did not hold (amendment
           -- 0028, REQ-IC-2). A hold, never a pause.
           OR (NEW.cause = 'blind' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome <> 'holds'))
           -- An observation of this asset the book could not post (REQ-IC-2).
           OR (NEW.cause = 'REQ-IC-2' AND NEW.evaluation_id IS NULL AND EXISTS (
               SELECT 1 FROM book_quarantine q
               WHERE q.id = NEW.quarantine_id AND q.asset = NEW.asset))
           -- A supply reading of this asset on the issuing network flagged as a mismatch between
           -- the on-chain component's count and the issuance (INV-4), seen by an evaluation of it.
           OR (NEW.cause = 'INV-4' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset)
               AND CASE WHEN NEW.cause = 'INV-4'
                        THEN pause_reading_is_supply_mismatch(NEW.asset, NEW.reading_id)
                        ELSE false END)) THEN
        RAISE EXCEPTION 'pause refused: the core pauses only an asset''s routes, on a divergence of that asset, an observation of it the book could not post, or a supply mismatch of it, and holds them only on an evaluation of it that did not hold (REQ-BK-1, REQ-IC-2, INV-4; amendment 0028)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- The core lifts its own hold, and nothing else (amendment 0028; REQ-GV-2 for every other row).
CREATE FUNCTION pauses_guard_delete() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        RETURN OLD;
    END IF;
    IF OLD.cause IS DISTINCT FROM 'blind' OR NOT EXISTS (
        SELECT 1 FROM (SELECT v.id, v.outcome, v.book_entry_cut FROM reserve_evaluations v
                       WHERE v.asset = OLD.asset ORDER BY v.id DESC LIMIT 1) latest
        WHERE latest.outcome = 'holds' AND latest.book_entry_cut IS NOT NULL
          AND latest.id > OLD.evaluation_id) THEN
        RAISE EXCEPTION 'lift refused: the core lifts only its own hold of an asset, once the asset''s latest evaluation holds; every pause is lifted by two operators (REQ-GV-2; amendment 0028)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN OLD;
END
$$;

CREATE TRIGGER pauses_guard_delete
    BEFORE DELETE ON pauses
    FOR EACH ROW EXECUTE FUNCTION pauses_guard_delete();

-- Step one of lifting a pause (0091), refusing a hold.
CREATE OR REPLACE FUNCTION operator_request_unpause(p_pause BIGINT, p_justification TEXT)
    RETURNS BIGINT
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
    IF p.kind = 'hold' THEN
        RAISE EXCEPTION 'unpause request refused: % is a hold (the reserve check cannot see asset %); it lifts itself once an evaluation holds, and is not lifted by hand (amendment 0028, REQ-IC-2)', p_pause, p.asset
            USING ERRCODE = 'BZ001';
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

CREATE INDEX event_log_reserve_blind_pages ON event_log ((payload ->> 'hold'))
    WHERE kind = 'reserve-blind';

-- ops_observables(): 0110's function, with the latest evaluation by id and holds apart.
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
    ORDER BY asset, id DESC
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
-- The reserve equation: 0 holds, 1 diverges, 2 could not be made or cannot see (amendment 0028);
-- and how long ago it was made. The latest evaluation is the latest by id (0142).
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
-- Pauses (kind `pause`) and holds (kind `hold`, amendment 0028) apart: a hold lifts itself and pages
-- through the core's `reserve-blind` once it outlasts its bound, so an alarm on pauses_in_force
-- never pages for one.
SELECT 'pauses_in_force', NULL, NULL, NULL,
       (SELECT count(*) FROM pauses WHERE kind = 'pause')::numeric
UNION ALL
SELECT 'holds_in_force', NULL, NULL, NULL,
       (SELECT count(*) FROM pauses WHERE kind = 'hold')::numeric
UNION ALL
-- How long the oldest hold of each held asset has stood, by the evaluation it began at.
SELECT 'hold_age_seconds', p.asset, NULL, NULL,
       extract(epoch FROM now() - v.evaluated_at)::numeric
FROM pauses p JOIN reserve_evaluations v ON v.id = p.evaluation_id
WHERE p.kind = 'hold'
$$;

COMMENT ON FUNCTION ops_observables() IS
    'REQ-ST-6, AC-H3.1: the continuously observable quantities, one row per metric and subject. '
    'Counts, ages, outcomes, reserve amounts and fee positions only; no row of any table. A fee '
    'position is the delivery account''s native balance (0110); fee_short_transfers stays as the '
    'transfers it already held. pauses_in_force counts pauses only, holds_in_force and '
    'hold_age_seconds the reserve check''s holds (0142). NULL value = not known.';



-- Grants. No function here is PUBLIC's; the trigger functions fire without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION pauses_guard_delete() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION reserve_evaluations_stamp() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION operator_request_unpause(BIGINT, TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION operator_request_unpause(BIGINT, TEXT) TO bezent_operator;

REVOKE EXECUTE ON FUNCTION reserve_check_holds(TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION reserve_check_holds(TEXT) TO bezent_core;
REVOKE EXECUTE ON FUNCTION ops_observables() FROM PUBLIC;
GRANT EXECUTE ON FUNCTION ops_observables() TO bezent_core, bezent_operator;

GRANT DELETE ON pauses TO bezent_core;
GRANT INSERT (held_heads) ON pauses TO bezent_core;

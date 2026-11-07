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
--     asset, by id, holds and is later than the one the hold names. Every other row (a divergence,
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
        SELECT 1 FROM (SELECT v.id, v.outcome FROM reserve_evaluations v
                       WHERE v.asset = OLD.asset ORDER BY v.id DESC LIMIT 1) latest
        WHERE latest.outcome = 'holds' AND latest.id > OLD.evaluation_id) THEN
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

-- Grants. No function here is PUBLIC's; the trigger functions fire without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION pauses_guard_delete() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION operator_request_unpause(BIGINT, TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION operator_request_unpause(BIGINT, TEXT) TO bezent_operator;

GRANT DELETE ON pauses TO bezent_core;

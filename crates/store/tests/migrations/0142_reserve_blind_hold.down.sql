-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): amendment 0028, the reserve check's blind hold. Its holds go
-- with it; the guard, the unpause request and the constraints return to 0095 and 0091.
REVOKE DELETE ON pauses FROM bezent_core;
DROP INDEX event_log_reserve_blind_pages;
DROP TRIGGER pauses_guard_delete ON pauses;
DROP FUNCTION pauses_guard_delete();
DELETE FROM pauses WHERE cause = 'blind';
DROP INDEX pauses_one_hold_per_asset;
ALTER TABLE pauses
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

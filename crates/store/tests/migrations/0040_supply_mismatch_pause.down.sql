-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): PR #50 review M1's INV-4 pause.
ALTER TABLE pauses
    DROP CONSTRAINT pauses_cause_names_its_reason,
    DROP CONSTRAINT pauses_cause_known,
    DROP COLUMN reading_id,
    ADD CONSTRAINT pauses_cause_known CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2')),
    ADD CONSTRAINT pauses_cause_names_its_reason CHECK (
        (cause IS NULL AND evaluation_id IS NULL AND quarantine_id IS NULL)
        OR (cause = 'REQ-BK-1' AND evaluation_id IS NOT NULL AND quarantine_id IS NULL)
        OR (cause = 'REQ-IC-2' AND (evaluation_id IS NULL) <> (quarantine_id IS NULL)));
CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF NEW.asset IS NULL OR NEW.network IS NOT NULL OR NEW.direction IS NOT NULL
       OR NOT (
           -- A divergence of this asset (REQ-BK-1).
           (NEW.cause = 'REQ-BK-1' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'diverges'))
           -- An evaluation of this asset that could not be made, or an observation of it the book
           -- could not post (REQ-IC-2).
           OR (NEW.cause = 'REQ-IC-2' AND NEW.quarantine_id IS NULL AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'incomplete'))
           OR (NEW.cause = 'REQ-IC-2' AND NEW.evaluation_id IS NULL AND EXISTS (
               SELECT 1 FROM book_quarantine q
               WHERE q.id = NEW.quarantine_id AND q.asset = NEW.asset))) THEN
        RAISE EXCEPTION 'pause refused: the core pauses only an asset''s routes, on a divergence of that asset, an evaluation of it that could not be made, or an observation of it the book could not post (REQ-BK-1, REQ-IC-2)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
DROP FUNCTION pause_reading_is_supply_mismatch(TEXT, BIGINT);

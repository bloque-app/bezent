-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): the review fixes of F-004 (quarantine, lag bound, REQ-IC-2
-- pauses, operator funding addresses).
REVOKE INSERT (quarantine_id) ON pauses FROM bezent_core;
DROP TABLE surplus_funding_addresses;
CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF NEW.cause IS DISTINCT FROM 'REQ-BK-1' OR NEW.asset IS NULL
       OR NEW.network IS NOT NULL OR NEW.direction IS NOT NULL
       OR NOT EXISTS (SELECT 1 FROM reserve_evaluations v
                      WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset
                        AND v.outcome = 'diverges') THEN
        RAISE EXCEPTION 'pause refused: the core pauses only an asset''s routes, on an evaluation of that asset that diverged (REQ-BK-1, REQ-IC-2)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
REVOKE EXECUTE ON FUNCTION pauses_guard_insert() FROM PUBLIC;
ALTER TABLE pauses
    DROP CONSTRAINT pauses_cause_names_its_reason,
    DROP CONSTRAINT pauses_cause_known,
    DROP COLUMN quarantine_id,
    ADD CONSTRAINT pauses_cause_check CHECK (cause IN ('REQ-BK-1')),
    ADD CONSTRAINT pauses_cause_names_its_evaluation CHECK ((cause IS NULL) = (evaluation_id IS NULL));
ALTER TABLE networks DROP COLUMN reserve_point_lag_seconds;
DROP TABLE book_quarantine;

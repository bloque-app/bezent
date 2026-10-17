-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): PR #50 re-check S-1, S-3's abandoned statement places.
DROP FUNCTION statement_place_refusal(TEXT, BIGINT, BYTEA, JSONB);
CREATE OR REPLACE FUNCTION reserve_statements_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.sequence > 1 AND NOT EXISTS (
        SELECT 1 FROM reserve_statements s
        WHERE s.asset = NEW.asset AND s.sequence = NEW.sequence - 1
          AND s.sha256 = NEW.previous_sha256) THEN
        RAISE EXCEPTION 'statement % of % refused: it must name the hash of statement % (REQ-PR-2)',
            NEW.sequence, NEW.asset, NEW.sequence - 1
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
ALTER TABLE reserve_statements
    DROP CONSTRAINT reserve_statements_first_has_no_previous,
    ADD CONSTRAINT reserve_statements_first_has_no_previous
        CHECK ((sequence = 1) = (previous_sha256 IS NULL));
DROP TABLE reserve_statement_abandoned;
DROP FUNCTION reserve_statement_abandoned_guard_insert();

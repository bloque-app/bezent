-- SPDX-License-Identifier: AGPL-3.0-or-later
-- PR #50 review M1 (issue #42; INV-4, REQ-IC-2, REQ-IC-4; PLAN DEC-16, 2026-10-15): a difference
-- between the on-chain component's own supply count and the asset's issuance on the issuing
-- network is a breach of INV-4 (value minted or destroyed outside the component). The chain's
-- issuance is the supply; the core raises the key-compromise alert and pauses the asset's routes.
-- Additive.
--
--   * pauses gains `reading_id`: the flagged supply reading an INV-4 pause rests on. A login that
--     does not own the table may insert an INV-4 pause only for the asset whose issuing identity
--     the reading is of, on the issuing network, with the reading flagged as a mismatch, naming an
--     evaluation of that asset. The pause is the core's to record and, as every pause, only the
--     operator's to lift: a later book correction does not lift it.

ALTER TABLE pauses
    ADD COLUMN reading_id BIGINT REFERENCES reserve_readings (id),
    DROP CONSTRAINT pauses_cause_known,
    ADD CONSTRAINT pauses_cause_known CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2', 'INV-4')),
    DROP CONSTRAINT pauses_cause_names_its_reason,
    ADD CONSTRAINT pauses_cause_names_its_reason CHECK (
        (cause IS NULL AND evaluation_id IS NULL AND quarantine_id IS NULL AND reading_id IS NULL)
        OR (cause = 'REQ-BK-1' AND evaluation_id IS NOT NULL AND quarantine_id IS NULL
            AND reading_id IS NULL)
        OR (cause = 'REQ-IC-2' AND (evaluation_id IS NULL) <> (quarantine_id IS NULL)
            AND reading_id IS NULL)
        OR (cause = 'INV-4' AND evaluation_id IS NOT NULL AND reading_id IS NOT NULL
            AND quarantine_id IS NULL));

COMMENT ON COLUMN pauses.reading_id IS
    'INV-4 (PR #50 review M1): the supply reading, flagged as a mismatch between the on-chain '
    'component''s count and the asset''s issuance, an INV-4 pause rests on.';

-- Whether `reading` is a supply reading of `asset_name`'s issuing identity, on the issuing network,
-- flagged as a mismatch. SECURITY DEFINER: the core's role alone reads neither `assets` nor
-- `reserve_readings`; it is EXECUTE-able by the core only, never by PUBLIC.
CREATE FUNCTION pause_reading_is_supply_mismatch(asset_name TEXT, reading BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM reserve_readings r
        JOIN networks n ON n.network = r.network AND n.role = 'issuing'
        JOIN assets a ON a.issuing_asset = r.subject
        WHERE r.id = reading AND a.asset = asset_name AND r.kind = 'supply' AND r.mismatch)
$$;

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
               WHERE q.id = NEW.quarantine_id AND q.asset = NEW.asset))
           -- A supply reading of this asset on the issuing network flagged as a mismatch between
           -- the on-chain component's count and the issuance (INV-4), seen by an evaluation of it.
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

-- Grants. No function here is PUBLIC's; the trigger function fires without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION pauses_guard_insert() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION pause_reading_is_supply_mismatch(TEXT, BIGINT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pause_reading_is_supply_mismatch(TEXT, BIGINT) TO bezent_core;

GRANT INSERT (reading_id) ON pauses TO bezent_core;

-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #77 (REQ-IC-2, REQ-IC-4, AC-I1.2, INV-15): a compromise signal applies the pause. Until now
-- a detector appended `pause-requested` to the log and nothing read it, so the bridge kept minting
-- and signing until a person acted. Additive; every grant stays as it was.
--
--   * A `key-compromise` alert pauses the bridge in the transaction that records it, whichever role
--     raises it, with the core or any other service down. The store does it, on the alert's insert,
--     so no detector can record an alert and leave the pause out, and none needs a privilege on
--     `pauses`: the signer and the endpoints hold none, and the core gains none. The authority is
--     the one each already has, to record an alert it detected (0049).
--   * The pause is global (F-005 NOTES 5; `features/F-005-signer/NOTES.md`, "The compromise pause"),
--     names its alert and rests on it: `alert_seq` is an alert in the log, one pause per alert, as
--     the guard checks (no foreign key: a reference to the log would change what truncating it says,
--     REQ-ST-4).
--     The one exception is the core's supply mismatch (`scope` `asset`, INV-4), which pauses the
--     asset's routes under 0040 and names a flagged reading.
--   * The owner and a superuser are exempt (NFR-7): a restore or replay of the log must not pause the
--     bridge again for an old alert, nor collide with the pause row it brings.
--   * A repeated detection writes no alert (0049's unique index), so it pauses nothing, however long
--     after the pause was lifted. A new detection is a new alert and a new pause, whatever else is
--     in force: another alert's pause or a person's can be lifted by someone who never saw this one.
--   * `pauses_guard_insert` is not widened: a login that does not own the table still cannot insert
--     a global pause, or name an alert, and no service can delete or update a pause. It only gains
--     the check of the alert. Lifting a pause stays the operators' (REQ-GV-2, AC-I2.2).

ALTER TABLE pauses
    ADD COLUMN alert_seq BIGINT,
    ADD CONSTRAINT pauses_one_per_alert UNIQUE (alert_seq),
    DROP CONSTRAINT pauses_cause_known,
    ADD CONSTRAINT pauses_cause_known CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2', 'INV-4', 'REQ-IC-4')),
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
            AND asset IS NULL AND network IS NULL AND direction IS NULL)),
    -- Strict: with no cause the check above is NULL for a row that names an alert, and passes.
    ADD CONSTRAINT pauses_alert_is_a_compromise_pause
        CHECK ((alert_seq IS NOT NULL) = (cause IS NOT DISTINCT FROM 'REQ-IC-4'));

COMMENT ON COLUMN pauses.alert_seq IS
    'REQ-IC-4, AC-I1.2 (issue #77): the key-compromise alert a compromise pause rests on, its position '
    'in the event log. Written only by the store, in the transaction that records the alert.';

CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    -- A compromise pause rests on a real alert, whoever writes it (only the store does, below; a
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

-- SECURITY DEFINER: the writer of an alert holds no privilege on `pauses`, and the guard admits the
-- owner. Fires only for a row the writer may already append (0046, 0049): the stamp has run, so
-- `detector` is the store's word, not the writer's, and only the core can name an asset's scope.
CREATE FUNCTION event_log_compromise_pauses() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    -- The owner (and a superuser) restores and replays the log (NFR-7; the exemption of 0020, 0047,
    -- 0049, 0050): replaying an old alert must not pause the bridge again, or resurrect a pause that
    -- was lifted, or collide with the pause row the same restore brings. `session_user`, not
    -- `current_user`: this function runs as its owner. A service never connects as the owner, so
    -- this admits no detector. An alert the owner writes outside a restore pauses nothing either;
    -- the owner pauses by inserting the pause, naming the alert (the guard admits it).
    IF pg_has_role(session_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE')
       OR (SELECT rolsuper FROM pg_roles WHERE rolname = session_user) THEN
        RETURN NULL;
    END IF;
    IF NEW.payload ->> 'detector' = 'core' AND NEW.payload ->> 'scope' = 'asset' THEN
        RETURN NULL;
    END IF;
    INSERT INTO pauses (cause, alert_seq) VALUES ('REQ-IC-4', NEW.seq);
    RETURN NULL;
END
$$;

CREATE TRIGGER event_log_compromise_pauses
    AFTER INSERT ON event_log
    FOR EACH ROW
    WHEN (NEW.kind = 'key-compromise')
    EXECUTE FUNCTION event_log_compromise_pauses();

-- No function here is PUBLIC's; the trigger function fires without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION event_log_compromise_pauses() FROM PUBLIC;

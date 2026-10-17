-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-001 T-001-03/04: the sequenced event log (SPEC §7.C REQ-ST-4; PLAN DEC-4).
--
-- Every state change and condition is appended here, gapless and readable by position. Positions
-- come from a single-row head that is advanced in the inserting transaction: a rolled-back insert
-- rolls the head back too, so no position is ever skipped (a PostgreSQL sequence would leave gaps).
-- Concurrent appends serialise on the head row. The log and its head are never rewritten.

CREATE TABLE event_log (
    seq         BIGINT PRIMARY KEY CHECK (seq >= 1),
    kind        TEXT NOT NULL CHECK (kind <> ''),
    transfer_id BIGINT REFERENCES transfers (id),
    payload     JSONB NOT NULL,
    appended_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE event_log_head (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    seq       BIGINT NOT NULL CHECK (seq >= 0)
);

INSERT INTO event_log_head (singleton, seq) VALUES (TRUE, 0);

-- Assigns the next position. A caller may leave seq empty, or name exactly the next position;
-- any other value would leave a gap or repeat one, and is refused.
-- SECURITY DEFINER: the head is advanced by the migration owner; the service role may only read it.
CREATE FUNCTION event_log_assign_seq() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    next_seq BIGINT;
BEGIN
    UPDATE event_log_head SET seq = seq + 1 WHERE singleton RETURNING seq INTO next_seq;
    IF NEW.seq IS NOT NULL AND NEW.seq <> next_seq THEN
        RAISE EXCEPTION 'event log position % refused: the next position is %; the log is gapless (REQ-ST-4)',
            NEW.seq, next_seq
            USING ERRCODE = 'BZ001';
    END IF;
    NEW.seq := next_seq;
    RETURN NEW;
END
$$;

-- The head moves only by one, and only from event_log_assign_seq().
CREATE FUNCTION event_log_head_guard() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP <> 'UPDATE' OR pg_trigger_depth() < 2 OR NEW.seq <> OLD.seq + 1
       OR NEW.singleton IS DISTINCT FROM OLD.singleton THEN
        RAISE EXCEPTION '% on event_log_head is refused: the head advances only by appending to the log (REQ-ST-4)', TG_OP
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER event_log_assign_seq
    BEFORE INSERT ON event_log
    FOR EACH ROW EXECUTE FUNCTION event_log_assign_seq();
CREATE TRIGGER event_log_append_only
    BEFORE UPDATE OR DELETE ON event_log
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-ST-4');
CREATE TRIGGER event_log_no_truncate
    BEFORE TRUNCATE ON event_log
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-4');
CREATE TRIGGER event_log_head_guard
    BEFORE INSERT OR UPDATE OR DELETE ON event_log_head
    FOR EACH ROW EXECUTE FUNCTION event_log_head_guard();
CREATE TRIGGER event_log_head_no_truncate
    BEFORE TRUNCATE ON event_log_head
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-4');

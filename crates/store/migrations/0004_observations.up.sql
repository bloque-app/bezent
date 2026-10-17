-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-03/04: observations (SPEC §8.1, §5.5; PLAN DEC-4).
--
-- An observation is what an endpoint saw at a coordinate. It is recorded once per coordinate, so
-- one observation is applied once (INV-10, REQ-IN-1), and it is never changed or deleted.
-- What later becomes of it (final, or withdrawn because it stopped matching the chain before
-- finality: REQ-OB-2, REQ-IN-7) is a separate append-only record, at most one per observation.

CREATE TABLE observations (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network            TEXT NOT NULL CHECK (network <> ''),
    block_number       BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash         BYTEA NOT NULL CHECK (length(block_hash) > 0),
    block_time         TIMESTAMPTZ NOT NULL,
    position_kind      position_kind NOT NULL,
    position_index     BIGINT NOT NULL CHECK (position_index BETWEEN 0 AND 4294967295),
    -- What was seen: the movement, as the network reports it (REQ-ST-3).
    token              TEXT CHECK (token <> ''),
    amount             minor_units,
    sender             TEXT,
    recipient          TEXT,
    on_chain_reference BYTEA CHECK (length(on_chain_reference) > 0),
    -- The transaction's execution result, not only its inclusion (REQ-OB-6).
    executed           BOOLEAN NOT NULL,
    -- The network's own data and error text (REQ-ST-3).
    detail             JSONB NOT NULL DEFAULT '{}'::jsonb,
    observed_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT observations_coordinate_once
        UNIQUE (network, block_number, block_hash, position_kind, position_index)
);

CREATE TABLE observation_outcomes (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    observation_id      BIGINT NOT NULL UNIQUE REFERENCES observations (id),
    outcome             TEXT NOT NULL CHECK (outcome IN ('final', 'withdrawn')),
    -- The final point at which the outcome was decided (REQ-OB-2).
    final_block_number  BIGINT NOT NULL CHECK (final_block_number >= 0),
    final_block_hash    BYTEA NOT NULL CHECK (length(final_block_hash) > 0),
    decided_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON COLUMN observations.amount IS
    'INV-11, REQ-ST-3: exact integer count of the observed token''s own minor units, as the network '
    'reports it; never scaled.';

-- REQ-OB-2: the final point at which an outcome is decided is at or past the observed block.
CREATE FUNCTION observation_outcomes_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    observed BIGINT;
BEGIN
    SELECT block_number INTO observed FROM observations WHERE id = NEW.observation_id;
    IF NEW.final_block_number < observed THEN
        RAISE EXCEPTION 'outcome of observation % is decided at block %, below the observed block % (REQ-OB-2)',
            NEW.observation_id, NEW.final_block_number, observed
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observation_outcomes_guard_insert
    BEFORE INSERT ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION observation_outcomes_guard_insert();

CREATE TRIGGER observations_append_only
    BEFORE UPDATE OR DELETE ON observations
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-10');
CREATE TRIGGER observations_no_truncate
    BEFORE TRUNCATE ON observations
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-10');
CREATE TRIGGER observation_outcomes_append_only
    BEFORE UPDATE OR DELETE ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-10, REQ-IN-7');
CREATE TRIGGER observation_outcomes_no_truncate
    BEFORE TRUNCATE ON observation_outcomes
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-10, REQ-IN-7');

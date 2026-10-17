-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-004 T-004-01 to T-004-05: the reserve book's movements, the one-minute reserve check and the
-- daily signed statement (SPEC §5.3 REQ-BK-1 to REQ-BK-10, §4.6 REQ-PR-1/2, REQ-BE-3, REQ-BE-6,
-- REQ-CV-4, §8.7 REQ-IC-2, INV-1, INV-17; PLAN DEC-4, DEC-16).
--
-- Numbered 0030, not 0022: F-009 (branch feat/F-009-evm-endpoint) is built in parallel and may add
-- migrations from 0022. Whichever of the two PRs merges second renumbers if the numbers meet.
-- Additive only: columns, tables, a trigger and grants; nothing of 0005 changes.
--
--   * book_entries gains the movement it records (`kind`), the final point it rests on (network,
--     block number and hash: an entry rests on one network, so the book can be read "as of" one
--     final point per network, the points the check reads the networks at), and the observation
--     or transfer it posts. A movement is posted once: (kind, reference) is unique, corrections
--     aside (INV-17: a correction is a new entry naming the one it corrects).
--   * book_postings gains `item`: the arrival, withdrawal or venue leg a posting to owed mint,
--     owed payout or value in transit belongs to, so each of those terms is a list, never a
--     residual (REQ-BK-2; F-001 NOTES N5).
--   * assets gains the surplus floor and the level the alert fires at, above it (REQ-BK-10).
--   * reserve_evaluations: every evaluation of the reserve equation, append-only (REQ-BK-1, NFR-5).
--   * pauses gains why: the core pauses an asset's routes on a divergence (REQ-BK-1, REQ-IC-2),
--     and only so; lifting a pause stays the operator's.
--   * reserve_statements: every statement published, chained by hash, append-only (REQ-PR-1,
--     REQ-PR-2).
--
-- Grants: the book is the core's record. Only bezent_core may append to it now; the ingress and
-- the endpoints, which hold bezent_service only, may read it.

-- ---------------------------------------------------------------------------------------------
-- The book's movements (REQ-BK-7, INV-17, REQ-AS-9, REQ-BK-4, REQ-BE-6).

ALTER TABLE book_entries
    ADD COLUMN kind TEXT CHECK (kind IN (
        'arrival', 'conversion', 'mint', 'redemption', 'payout', 'refund-mint',
        'unassociated-refund', 'surplus-contribution', 'surplus-withdrawal', 'network-cost',
        'venue-fee', 'reversal', 'restoration', 'correction')),
    ADD COLUMN network TEXT CHECK (network <> ''),
    ADD COLUMN block_number BIGINT CHECK (block_number >= 0),
    ADD COLUMN block_hash BYTEA CHECK (length(block_hash) > 0),
    ADD COLUMN observation_id BIGINT REFERENCES observations (id),
    ADD COLUMN transfer_id BIGINT REFERENCES transfers (id),
    ADD CONSTRAINT book_entries_point_whole
        CHECK ((network IS NULL) = (block_number IS NULL) AND (network IS NULL) = (block_hash IS NULL)),
    ADD CONSTRAINT book_entries_correction_names_its_entry
        CHECK (kind IS DISTINCT FROM 'correction' OR corrects IS NOT NULL);

COMMENT ON COLUMN book_entries.kind IS
    'REQ-BK-7: the movement the entry records. NULL only for entries written before F-004.';
COMMENT ON COLUMN book_entries.network IS
    'The network of the point the movement rests on: its own coordinate''s block for a movement '
    'observed on chain. NULL for a movement that rests on a report or a declaration. The reserve '
    'check reads the book as of one final point per network (REQ-BK-8).';

-- One entry per movement, forever (INV-17): a second posting of the same movement is refused.
CREATE UNIQUE INDEX book_entries_movement_once ON book_entries (kind, reference)
    WHERE kind IS NOT NULL AND corrects IS NULL;
CREATE INDEX book_entries_observation ON book_entries (observation_id) WHERE observation_id IS NOT NULL;

ALTER TABLE book_postings ADD COLUMN item TEXT CHECK (item <> '');

COMMENT ON COLUMN book_postings.item IS
    'REQ-BK-2: what the posting belongs to: an arrival''s coordinate (owed mint), a withdrawal''s '
    'bridge reference (owed payout), a venue leg (in transit), an attested position (reserve), or '
    'the deposit whose REQ-AS-9 remainder a surplus posting records.';

CREATE INDEX book_postings_asset_account ON book_postings (asset, account, item);

-- ---------------------------------------------------------------------------------------------
-- The surplus floor (REQ-BK-10): declared by the operator, in the asset's book unit. The alert
-- fires below `surplus_alert`, before the floor is reached; below the floor after a reversal, new
-- mints pause (REQ-BE-6).

ALTER TABLE assets
    ADD COLUMN surplus_floor minor_units,
    ADD COLUMN surplus_alert minor_units,
    ADD CONSTRAINT assets_surplus_alert_above_floor
        CHECK (surplus_alert IS NULL OR surplus_floor IS NULL OR surplus_alert > surplus_floor);

COMMENT ON COLUMN assets.surplus_floor IS
    'REQ-BK-10: the surplus the operator keeps the asset above, in the book unit (the origin '
    'token''s minor unit). NULL is a floor of zero.';
COMMENT ON COLUMN assets.surplus_alert IS
    'REQ-BK-10: the alert fires when surplus falls below this level, which is above the floor, so '
    'it fires before the floor is reached. NULL: the alert fires below the floor only.';

-- ---------------------------------------------------------------------------------------------
-- Evaluations of the reserve equation (REQ-BK-1, REQ-BK-8, NFR-5, INV-1).

CREATE TABLE reserve_evaluations (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    asset        TEXT NOT NULL REFERENCES assets (asset),
    -- The core's clock when it evaluated (a controllable clock in tests).
    evaluated_at TIMESTAMPTZ NOT NULL,
    outcome      TEXT NOT NULL CHECK (outcome IN ('holds', 'diverges', 'incomplete')),
    -- Why an evaluation could not be made (no final point, no reading).
    reason       TEXT CHECK (reason <> ''),
    -- The final point each network was read at.
    points       JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- Every term re-derived from the networks and reports, itemised; and the book's, as of those
    -- points.
    chain        JSONB NOT NULL DEFAULT '{}'::jsonb,
    book         JSONB NOT NULL DEFAULT '{}'::jsonb,
    differences  JSONB NOT NULL DEFAULT '[]'::jsonb,
    alerts       JSONB NOT NULL DEFAULT '[]'::jsonb,
    recorded_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reserve_evaluations_reason CHECK ((outcome = 'incomplete') = (reason IS NOT NULL)),
    CONSTRAINT reserve_evaluations_differences
        CHECK ((outcome = 'diverges') = (jsonb_array_length(differences) > 0))
);

CREATE INDEX reserve_evaluations_latest ON reserve_evaluations (asset, evaluated_at DESC, id DESC);

COMMENT ON TABLE reserve_evaluations IS
    'REQ-BK-1, NFR-5: each evaluation of the reserve equation per asset, at zero tolerance. A '
    'difference of any term, to the smallest unit, is a divergence.';

CREATE TRIGGER reserve_evaluations_append_only
    BEFORE UPDATE OR DELETE ON reserve_evaluations
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-BK-1, NFR-5');
CREATE TRIGGER reserve_evaluations_no_truncate
    BEFORE TRUNCATE ON reserve_evaluations
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-BK-1, NFR-5');

-- ---------------------------------------------------------------------------------------------
-- The pause a divergence makes (REQ-BK-1, REQ-IC-2, AC-E2.1). A pause row with no cause is the
-- operator's (REQ-GV-1). A login that does not own the table may insert only this one: the
-- asset's routes, on an evaluation of that asset that diverged.

ALTER TABLE pauses
    ADD COLUMN cause TEXT CHECK (cause IN ('REQ-BK-1')),
    ADD COLUMN evaluation_id BIGINT REFERENCES reserve_evaluations (id),
    ADD CONSTRAINT pauses_cause_names_its_evaluation
        CHECK ((cause IS NULL) = (evaluation_id IS NULL));

CREATE FUNCTION pauses_guard_insert() RETURNS trigger
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

CREATE TRIGGER pauses_guard_insert
    BEFORE INSERT ON pauses
    FOR EACH ROW EXECUTE FUNCTION pauses_guard_insert();

-- ---------------------------------------------------------------------------------------------
-- Statements (REQ-PR-1, REQ-PR-2, REQ-BE-3, REQ-BK-9, REQ-CV-4). Each names the SHA-256 of the
-- asset's previous one, so a revision of an earlier statement is detectable from the later ones
-- (AC-E1.2). The documents live in an append-only store (DEC-16: a bucket with object lock); this
-- table is the core's record of what it published.

CREATE TABLE reserve_statements (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    asset           TEXT NOT NULL REFERENCES assets (asset),
    sequence        BIGINT NOT NULL CHECK (sequence >= 1),
    evaluation_id   BIGINT NOT NULL REFERENCES reserve_evaluations (id),
    issued_at       TIMESTAMPTZ NOT NULL,
    name            TEXT NOT NULL UNIQUE CHECK (name <> ''),
    sha256          BYTEA NOT NULL CHECK (length(sha256) = 32),
    previous_sha256 BYTEA CHECK (length(previous_sha256) = 32),
    public_key      BYTEA NOT NULL CHECK (length(public_key) = 32),
    signature       BYTEA NOT NULL CHECK (length(signature) = 64),
    recorded_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reserve_statements_once UNIQUE (asset, sequence),
    CONSTRAINT reserve_statements_first_has_no_previous
        CHECK ((sequence = 1) = (previous_sha256 IS NULL))
);

-- REQ-PR-2: statement n names statement n-1's hash, so the chain has no gap and no fork.
CREATE FUNCTION reserve_statements_guard_insert() RETURNS trigger
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

CREATE TRIGGER reserve_statements_guard_insert
    BEFORE INSERT ON reserve_statements
    FOR EACH ROW EXECUTE FUNCTION reserve_statements_guard_insert();
CREATE TRIGGER reserve_statements_append_only
    BEFORE UPDATE OR DELETE ON reserve_statements
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');
CREATE TRIGGER reserve_statements_no_truncate
    BEFORE TRUNCATE ON reserve_statements
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');

-- ---------------------------------------------------------------------------------------------
-- Grants. No function here is PUBLIC's (F-003 verification F1); the trigger functions fire
-- without an EXECUTE grant.

REVOKE EXECUTE ON FUNCTION pauses_guard_insert(), reserve_statements_guard_insert() FROM PUBLIC;

REVOKE INSERT ON book_entries, book_postings FROM bezent_service;
GRANT INSERT ON book_entries, book_postings TO bezent_core;
GRANT SELECT ON book_entries, book_postings TO bezent_core;

GRANT SELECT, INSERT ON reserve_evaluations, reserve_statements TO bezent_core;
GRANT SELECT ON reserve_evaluations, reserve_statements TO bezent_service, bezent_operator;
GRANT SELECT ON pauses TO bezent_core;
GRANT INSERT (asset, cause, evaluation_id) ON pauses TO bezent_core;

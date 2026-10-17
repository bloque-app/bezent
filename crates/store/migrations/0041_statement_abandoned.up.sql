-- SPDX-License-Identifier: GPL-3.0-or-later
-- PR #50 re-check S-1 and S-3 (issue #42; REQ-PR-1, REQ-PR-2; PLAN DEC-16): a statement the
-- statement key signed that never reached the store, and that cannot be stored as it is (it no
-- longer re-derives, or it was signed under a key that is no longer the statement key), is
-- abandoned: its place is recorded here, with the hash of what was signed, why and who, and the
-- next place's statement names it. Nothing is ever stored twice for a place, nothing is re-signed
-- for it, and the chain goes on without a key rotation. Additive.
--
--   * reserve_statement_abandoned: append-only, one row per abandoned place. A place may be
--     abandoned only when it is the asset's next open place (after its last published or
--     abandoned one), holds no published statement, and the statement key signed a statement
--     intent for exactly that place and hash. The actor is the login that records it, never a
--     value it supplies.
--   * reserve_statements: a statement names the hash of the last published statement before it
--     (none for the first published one), and every place between them must be abandoned; a
--     statement is never published at an abandoned place.
--   * statement_place_refusal: what the signer reads, and nothing else, to sign a statement only
--     at a place that skips no place but abandoned ones, naming exactly those (SECURITY DEFINER,
--     the signer's to execute only; never PUBLIC's).

CREATE TABLE reserve_statement_abandoned (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    asset        TEXT NOT NULL REFERENCES assets (asset),
    sequence     BIGINT NOT NULL CHECK (sequence >= 1),
    -- The SHA-256 of the statement signed for the place and never published.
    sha256       BYTEA NOT NULL CHECK (length(sha256) = 32),
    -- The key that signed it, and the intent it was signed through.
    public_key   BYTEA NOT NULL CHECK (length(public_key) = 32),
    intent_id    BIGINT NOT NULL REFERENCES intents (id),
    reason       TEXT NOT NULL CHECK (reason <> ''),
    actor        TEXT NOT NULL,
    abandoned_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reserve_statement_abandoned_once UNIQUE (asset, sequence)
);

COMMENT ON TABLE reserve_statement_abandoned IS
    'REQ-PR-2 (PR #50 re-check S-1, S-3): a place in an asset''s statement chain whose signed '
    'statement never reached the store and could not be stored as it was. The next published '
    'statement names it; nothing is published or signed for it again. Append-only.';

CREATE FUNCTION reserve_statement_abandoned_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    NEW.actor := current_user;
    NEW.abandoned_at := now();
    IF EXISTS (SELECT 1 FROM reserve_statements s
               WHERE s.asset = NEW.asset AND s.sequence = NEW.sequence)
       OR NEW.sequence <> 1 + greatest(
              coalesce((SELECT max(s.sequence) FROM reserve_statements s WHERE s.asset = NEW.asset), 0),
              coalesce((SELECT max(x.sequence) FROM reserve_statement_abandoned x
                        WHERE x.asset = NEW.asset), 0))
       OR NOT EXISTS (
           SELECT 1 FROM intents i JOIN signatures g ON g.intent_id = i.id
           WHERE i.id = NEW.intent_id AND i.purpose = 'statement'
             AND i.payload -> 'statement' ->> 'asset' = NEW.asset
             AND i.payload -> 'statement' ->> 'sequence' = NEW.sequence::text
             AND i.payload -> 'statement' ->> 'sha256' = '0x' || encode(NEW.sha256, 'hex')
             AND g.scheme = 'ed25519' AND g.public_key = NEW.public_key) THEN
        RAISE EXCEPTION 'abandonment of statement % of % refused: only the next open place, with no published statement, and a statement the statement key signed for it is abandoned (REQ-PR-2)',
            NEW.sequence, NEW.asset
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER reserve_statement_abandoned_guard_insert
    BEFORE INSERT ON reserve_statement_abandoned
    FOR EACH ROW EXECUTE FUNCTION reserve_statement_abandoned_guard_insert();
CREATE TRIGGER reserve_statement_abandoned_append_only
    BEFORE UPDATE OR DELETE ON reserve_statement_abandoned
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');
CREATE TRIGGER reserve_statement_abandoned_no_truncate
    BEFORE TRUNCATE ON reserve_statement_abandoned
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');

-- The first statement published names no predecessor; a later one may name none only when every
-- place before it was abandoned, which the guard checks.
ALTER TABLE reserve_statements
    DROP CONSTRAINT reserve_statements_first_has_no_previous,
    ADD CONSTRAINT reserve_statements_first_has_no_previous
        CHECK (sequence > 1 OR previous_sha256 IS NULL);

-- REQ-PR-2: statement n names the hash of the last statement published before it, and every place
-- between them is abandoned, so the chain has no unexplained gap and no fork; an abandoned place
-- never gets a statement.
CREATE OR REPLACE FUNCTION reserve_statements_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    before_sequence BIGINT;
    before_sha256   BYTEA;
BEGIN
    IF EXISTS (SELECT 1 FROM reserve_statement_abandoned x
               WHERE x.asset = NEW.asset AND x.sequence = NEW.sequence) THEN
        RAISE EXCEPTION 'statement % of % refused: the place is abandoned (REQ-PR-2)',
            NEW.sequence, NEW.asset
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.sequence > 1 THEN
        SELECT s.sequence, s.sha256 INTO before_sequence, before_sha256
        FROM reserve_statements s
        WHERE s.asset = NEW.asset AND s.sequence < NEW.sequence
        ORDER BY s.sequence DESC LIMIT 1;
        IF before_sha256 IS DISTINCT FROM NEW.previous_sha256
           OR (SELECT count(*) FROM reserve_statement_abandoned x
               WHERE x.asset = NEW.asset AND x.sequence > coalesce(before_sequence, 0)
                 AND x.sequence < NEW.sequence)
              <> NEW.sequence - coalesce(before_sequence, 0) - 1 THEN
            RAISE EXCEPTION 'statement % of % refused: it must name the hash of the last statement before it, and every place between them must be abandoned (REQ-PR-2)',
                NEW.sequence, NEW.asset
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

-- Why the statement of `asset_name` at `place`, naming `previous` (the SHA-256 of the statement
-- before it, or NULL) and `abandoned` (the list of {sequence, sha256, ...} it says were
-- abandoned), may not be signed; NULL when it may. It may be signed only at a place that is
-- neither published nor abandoned, after a published statement it names (or none), with every
-- place between abandoned and named, exactly, in order. SECURITY DEFINER: the signer's role reads
-- neither table; it is EXECUTE-able by the signer only, never by PUBLIC.
CREATE FUNCTION statement_place_refusal(asset_name TEXT, place BIGINT, previous BYTEA,
                                        abandoned JSONB) RETURNS TEXT
    LANGUAGE plpgsql STABLE
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    last_published BIGINT;
    recorded       JSONB;
    named          JSONB;
BEGIN
    IF place IS NULL OR place < 1 THEN
        RETURN 'it names no place in the chain';
    END IF;
    IF EXISTS (SELECT 1 FROM reserve_statement_abandoned x
               WHERE x.asset = asset_name AND x.sequence = place) THEN
        RETURN format('place %s of %s is abandoned; nothing is signed for it', place, asset_name);
    END IF;
    IF EXISTS (SELECT 1 FROM reserve_statements s
               WHERE s.asset = asset_name AND s.sequence = place) THEN
        RETURN format('place %s of %s is published', place, asset_name);
    END IF;
    IF previous IS NULL THEN
        last_published := 0;
    ELSE
        SELECT s.sequence INTO last_published FROM reserve_statements s
        WHERE s.asset = asset_name AND s.sha256 = previous;
        IF last_published IS NULL THEN
            RETURN format('its previous_sha256 names no published statement of %s', asset_name);
        END IF;
    END IF;
    IF last_published >= place THEN
        RETURN 'the statement it names as previous is not before it';
    END IF;
    SELECT coalesce(jsonb_agg(jsonb_build_object('sequence', x.sequence,
                                                 'sha256', '0x' || encode(x.sha256, 'hex'))
                              ORDER BY x.sequence), '[]'::jsonb)
    INTO recorded
    FROM reserve_statement_abandoned x
    WHERE x.asset = asset_name AND x.sequence > last_published AND x.sequence < place;
    IF jsonb_array_length(recorded) <> place - last_published - 1 THEN
        RETURN format('it skips places %s to %s of %s, and not all of them are abandoned',
                      last_published + 1, place - 1, asset_name);
    END IF;
    named := coalesce(abandoned, '[]'::jsonb);
    IF jsonb_typeof(named) <> 'array' THEN
        RETURN 'its abandoned places are not a list';
    END IF;
    SELECT coalesce(jsonb_agg(jsonb_build_object('sequence', e -> 'sequence',
                                                 'sha256', e -> 'sha256') ORDER BY o), '[]'::jsonb)
    INTO named
    FROM jsonb_array_elements(named) WITH ORDINALITY AS t (e, o);
    IF named <> recorded THEN
        RETURN 'it does not name exactly the abandoned places it skips, with their hashes, in order';
    END IF;
    RETURN NULL;
END
$$;

-- Grants. No function here is PUBLIC's; the trigger functions fire without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION reserve_statement_abandoned_guard_insert() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION statement_place_refusal(TEXT, BIGINT, BYTEA, JSONB) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION statement_place_refusal(TEXT, BIGINT, BYTEA, JSONB) TO bezent_signer;

GRANT SELECT, INSERT ON reserve_statement_abandoned TO bezent_core;
GRANT SELECT ON reserve_statement_abandoned TO bezent_service, bezent_operator;

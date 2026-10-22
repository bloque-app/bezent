-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #58 (REQ-PR-1, REQ-PR-2; PLAN DEC-16): the core writes each daily statement to the
-- object-locked bucket itself, and records where. One row per object the statement sink wrote (the
-- statement and its signature file): the key it was written under, the ETag and the version id the
-- bucket answered with, and the SHA-256 of the object's bytes. The core inserts the rows, in the
-- same transaction as the statement's own row, only after every object was written; a statement
-- with no row here was not published to a store that answers with a receipt. Additive.
--
--   * reserve_statement_objects: append-only. The actor and the time are stamped by the store,
--     never supplied. A row names a statement that is already recorded (a guard, not a foreign
--     key: a foreign key would turn the refusal of TRUNCATE on reserve_statements into another
--     error). A directory sink (tests) records its file name and no ETag or version.

CREATE TABLE reserve_statement_objects (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    asset        TEXT NOT NULL,
    sequence     BIGINT NOT NULL CHECK (sequence >= 1),
    -- The object's logical name in the statement store (the statement's name, or that and `.sig`).
    name         TEXT NOT NULL CHECK (name <> ''),
    -- Where the sink wrote it: the bucket key, or the file name for a directory.
    location     TEXT NOT NULL CHECK (location <> ''),
    -- The SHA-256 of the object's bytes.
    sha256       BYTEA NOT NULL CHECK (length(sha256) = 32),
    etag         TEXT CHECK (etag <> ''),
    version_id   TEXT CHECK (version_id <> ''),
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor        TEXT NOT NULL,
    CONSTRAINT reserve_statement_objects_once UNIQUE (asset, sequence, name)
);

COMMENT ON TABLE reserve_statement_objects IS
    'REQ-PR-2, DEC-16 (issue #58): where the statement sink wrote each object of a published '
    'statement, and the receipt it answered with. Written only after the write succeeded. Append-only.';

CREATE FUNCTION reserve_statement_objects_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    NEW.actor := current_user;
    NEW.published_at := now();
    IF NOT EXISTS (SELECT 1 FROM reserve_statements s
                   WHERE s.asset = NEW.asset AND s.sequence = NEW.sequence) THEN
        RAISE EXCEPTION 'objects of statement % of % refused: the statement is not recorded (REQ-PR-2)',
            NEW.sequence, NEW.asset
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER reserve_statement_objects_guard_insert
    BEFORE INSERT ON reserve_statement_objects
    FOR EACH ROW EXECUTE FUNCTION reserve_statement_objects_guard_insert();
CREATE TRIGGER reserve_statement_objects_append_only
    BEFORE UPDATE OR DELETE ON reserve_statement_objects
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');
CREATE TRIGGER reserve_statement_objects_no_truncate
    BEFORE TRUNCATE ON reserve_statement_objects
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-PR-2');

-- Grants. The trigger function is no one's to execute; the core appends, the operators read.
REVOKE EXECUTE ON FUNCTION reserve_statement_objects_guard_insert() FROM PUBLIC;

GRANT SELECT, INSERT ON reserve_statement_objects TO bezent_core;
GRANT SELECT ON reserve_statement_objects TO bezent_operator;

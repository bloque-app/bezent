-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-03/04: the reserve book (SPEC §5.3 REQ-BK-7; PLAN DEC-4).
--
-- Double entry, append-only. An entry is one movement, with its reference (REQ-BK-4, REQ-BK-5);
-- its postings move value between the positions REQ-BK-7 names. Every entry balances per asset:
-- for each asset it touches, debits equal credits, checked when the transaction commits.
-- A correction is a new entry that names the one it corrects; no entry or posting is ever changed
-- or deleted (INV-17).
--
-- An entry is sealed when the transaction that records it commits: a posting is accepted only in
-- the transaction that inserted its entry, so a committed entry never gains a posting (INV-17).
-- The entry records that transaction's top-level id and start time, both set by the store, never
-- by the caller. The id alone is not enough after a restore into another cluster, whose
-- transaction ids may run through the restored ones again; the start time, which moves forward,
-- tells them apart. Both are top-level values, so savepoints do not change them.

CREATE TABLE book_entries (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    reference   TEXT NOT NULL CHECK (reference <> ''),
    memo        TEXT NOT NULL DEFAULT '',
    corrects    BIGINT REFERENCES book_entries (id),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The recording transaction (txid_current()); with recorded_at (now(), the transaction's start
    -- time) it seals the entry at commit (INV-17).
    recorded_tx BIGINT NOT NULL DEFAULT txid_current()
);

CREATE TABLE book_postings (
    id       BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    entry_id BIGINT NOT NULL REFERENCES book_entries (id),
    asset    TEXT NOT NULL CHECK (asset <> ''),
    -- The positions of REQ-BK-7 and the terms of the reserve equation (§5.3).
    account  TEXT NOT NULL CHECK (account IN (
                 'reserve', 'in-transit', 'owed-mint', 'owed-payout', 'surplus', 'supply')),
    -- Reserve is per (asset, origin network).
    network  TEXT CHECK (network <> ''),
    side     TEXT NOT NULL CHECK (side IN ('debit', 'credit')),
    amount   minor_units NOT NULL CHECK (amount > 0),
    CONSTRAINT book_postings_reserve_has_network CHECK (account <> 'reserve' OR network IS NOT NULL)
);

COMMENT ON COLUMN book_postings.amount IS
    'INV-11, REQ-AS-4: exact integer count of the asset''s book unit, the minor unit of its origin '
    'token (the finer side when the origin token has more precision than the bridged asset), so a '
    'REQ-AS-9 remainder is exact. Issuing-network values (supply, mints, redemptions) are posted '
    'multiplied by the asset''s declared ratio. Every posting of one asset is in the same unit, so '
    'an entry balances per asset.';

CREATE INDEX book_postings_entry ON book_postings (entry_id);

-- INV-17: the store, not the caller, records which transaction an entry belongs to.
CREATE FUNCTION book_entries_stamp() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.recorded_tx := txid_current();
    NEW.recorded_at := now();
    RETURN NEW;
END
$$;

CREATE TRIGGER book_entries_stamp
    BEFORE INSERT ON book_entries
    FOR EACH ROW EXECUTE FUNCTION book_entries_stamp();

-- INV-17: a posting joins only an entry recorded in its own transaction; a committed entry is
-- sealed and never gains one.
CREATE FUNCTION book_postings_same_transaction() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    e book_entries%ROWTYPE;
BEGIN
    SELECT * INTO e FROM book_entries WHERE id = NEW.entry_id;
    IF NOT FOUND THEN
        RETURN NEW;  -- the foreign key refuses it
    END IF;
    IF e.recorded_tx <> txid_current() OR e.recorded_at <> now() THEN
        RAISE EXCEPTION 'book entry % was committed by an earlier transaction; a committed entry never gains a posting (INV-17)',
            NEW.entry_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER book_postings_same_transaction
    BEFORE INSERT ON book_postings
    FOR EACH ROW EXECUTE FUNCTION book_postings_same_transaction();

-- At commit: the entry has at least two postings, and for every asset its debits equal its credits.
CREATE FUNCTION book_entry_balanced() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    entry       BIGINT;
    n           BIGINT;
    unbalanced  TEXT;
BEGIN
    IF TG_TABLE_NAME = 'book_entries' THEN
        entry := NEW.id;
    ELSE
        entry := NEW.entry_id;
    END IF;

    SELECT count(*) INTO n FROM book_postings WHERE entry_id = entry;
    IF n < 2 THEN
        RAISE EXCEPTION 'book entry % has % posting(s); a double entry needs at least two (REQ-BK-7)', entry, n
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT string_agg(asset, ', ' ORDER BY asset) INTO unbalanced
    FROM (
        SELECT asset
        FROM book_postings
        WHERE entry_id = entry
        GROUP BY asset
        HAVING sum(CASE side WHEN 'debit' THEN amount ELSE 0 END)
            <> sum(CASE side WHEN 'credit' THEN amount ELSE 0 END)
    ) s;
    IF unbalanced IS NOT NULL THEN
        RAISE EXCEPTION 'book entry % does not balance for asset %: debits must equal credits per asset (INV-17, REQ-BK-7)',
            entry, unbalanced
            USING ERRCODE = 'BZ001';
    END IF;

    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER book_entries_balanced
    AFTER INSERT ON book_entries
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION book_entry_balanced();
CREATE CONSTRAINT TRIGGER book_postings_balanced
    AFTER INSERT ON book_postings
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION book_entry_balanced();

CREATE TRIGGER book_entries_append_only
    BEFORE UPDATE OR DELETE ON book_entries
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-17, REQ-BK-7');
CREATE TRIGGER book_entries_no_truncate
    BEFORE TRUNCATE ON book_entries
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-17, REQ-BK-7');
CREATE TRIGGER book_postings_append_only
    BEFORE UPDATE OR DELETE ON book_postings
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-17, REQ-BK-7');
CREATE TRIGGER book_postings_no_truncate
    BEFORE TRUNCATE ON book_postings
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-17, REQ-BK-7');

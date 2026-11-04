-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-011 T-011-02: an attested backend's reports (SPEC §4.4(b), REQ-BE-4, REQ-BE-5, REQ-PN-2,
-- REQ-RQ-4; PLAN DEC-14). Additive. Numbered 0105 (0106 and 0107 follow): a store that
-- has applied 0095 refuses a lower pending version (bezent-migrate), so these come after the highest
-- version on `develop`, and 0100 to 0104 are another open PR's (F-011 NOTES 12).
--
--   * attesting_parties, attesting_keys: declarations. Each attested network has one attesting
--     party, the requester that speaks for it, and the keys it signs its reports with. The attesting
--     grant is per kind (REQ-PN-2); this names "that backend's attesting grant" (REQ-RQ-4), so a
--     requester of one backend, or Bloque Test on the one instance (DEC-15), cannot report for
--     another.
--   * attested_reports: what the party said, as it said it, append-only: the exact bytes signed, the
--     signature, the party's own identifier (REQ-BE-4). The store numbers them per network, under a
--     lock held to commit, so their order is their commit order.
--   * bezent_record_attested_observation(): the report becomes the observation (REQ-BE-5), with its
--     outcome and a final head, at a coordinate of its own: block number, the report's number on
--     its network; block hash, the digest of the signed bytes; position, log 0. Every field comes
--     from the report row. The core calls it once it has checked the signature; it holds no write
--     on the chain tables (review S6 of PR #56 stands), and the ingress cannot call it. So neither
--     makes an observation alone (F-011 NOTES 2).
--   * No order on an attested network: bezent signs nothing for a fiat payout (REQ-RQ-9; review S2).

-- ---------------------------------------------------------------------------------------------
-- A report is a request of its own kind (§7.A, proposed REQ-RQ-10 of amendment 0016): the party's
-- identifier is its request reference (REQ-ID-1).

ALTER TABLE intake DROP CONSTRAINT intake_kind_check;
ALTER TABLE intake ADD CONSTRAINT intake_kind_check
    CHECK (kind IN ('deposit', 'withdrawal', 'refund', 'report'));

-- ---------------------------------------------------------------------------------------------
-- Declarations (the migration owner's, as every declaration is).

CREATE TABLE attesting_parties (
    network   TEXT PRIMARY KEY REFERENCES networks (network),
    requester TEXT NOT NULL REFERENCES requesters (requester),
    -- The name the statement gives the party (REQ-BE-2, REQ-BE-4).
    party     TEXT NOT NULL CHECK (party <> '')
);

COMMENT ON TABLE attesting_parties IS
    'REQ-BE-2, REQ-BE-5, REQ-RQ-4, DEC-14: the named party of each attested network, and the requester '
    'that speaks for it. No row means nobody may report for that network.';

CREATE TABLE attesting_keys (
    network    TEXT NOT NULL REFERENCES attesting_parties (network),
    public_key BYTEA NOT NULL CHECK (length(public_key) = 32),
    PRIMARY KEY (network, public_key)
);

COMMENT ON TABLE attesting_keys IS
    'DEC-14, REQ-BE-4: the ed25519 keys a party signs its reports with. A new key is declared beside '
    'the old one, so a report in flight survives a rotation. Only the public half is ever here.';

-- An attesting party is a party of an attested backend (REQ-BE-2).
CREATE FUNCTION attesting_parties_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM networks n
                   WHERE n.network = NEW.network AND n.backend LIKE 'attested:%') THEN
        RAISE EXCEPTION '% has no attested backend; only an attested backend has an attesting party (REQ-BE-2)',
            NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER attesting_parties_guard
    BEFORE INSERT OR UPDATE ON attesting_parties
    FOR EACH ROW EXECUTE FUNCTION attesting_parties_guard();

-- ---------------------------------------------------------------------------------------------
-- The reports.

CREATE TABLE attested_reports (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network      TEXT NOT NULL REFERENCES attesting_parties (network),
    -- Its number on its network, from 1, set by the store. The observation's block number.
    seq          BIGINT NOT NULL CHECK (seq >= 1),
    intake_id    BIGINT NOT NULL UNIQUE REFERENCES intake (id),
    requester    TEXT NOT NULL,
    party        TEXT NOT NULL,
    -- The party's own identifier (REQ-BE-4).
    report_id    TEXT NOT NULL CHECK (report_id <> ''),
    kind         TEXT NOT NULL CHECK (kind IN ('arrival', 'payout', 'positions', 'reversal',
                                               'restoration')),
    asset        TEXT NOT NULL REFERENCES assets (asset),
    -- The document, parsed from `signed_bytes`, which remain what was signed.
    body         JSONB NOT NULL,
    signed_bytes BYTEA NOT NULL CHECK (length(signed_bytes) > 0),
    signature    BYTEA NOT NULL CHECK (length(signature) = 64),
    public_key   BYTEA NOT NULL CHECK (length(public_key) = 32),
    -- BLAKE2b-256 of what was signed. The observation's block hash.
    digest       BYTEA NOT NULL CHECK (length(digest) = 32),
    -- Set by the store: the observation's block time. The party's own clock stays in the body.
    received_at  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT attested_reports_number_once UNIQUE (network, seq),
    CONSTRAINT attested_reports_party_id_once UNIQUE (network, report_id)
);

COMMENT ON TABLE attested_reports IS
    'REQ-BE-4, REQ-BE-5, DEC-14: each report of an attesting party, as signed, under its own identifier. '
    'Append-only. It becomes an observation through bezent_record_attested_observation().';

-- REQ-BE-4, REQ-RQ-4: what a row says about itself holds, whoever writes it; and the number is the
-- store's, taken under a lock held to commit, so numbers commit in order and none is skipped.
CREATE FUNCTION attested_reports_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM attesting_parties p
                   WHERE p.network = NEW.network AND p.requester = NEW.requester
                     AND p.party = NEW.party) THEN
        RAISE EXCEPTION 'report % refused: % is not the attesting party of % (REQ-BE-5, REQ-RQ-4)',
            NEW.report_id, NEW.requester, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM attesting_keys k
                   WHERE k.network = NEW.network AND k.public_key = NEW.public_key) THEN
        RAISE EXCEPTION 'report % refused: its key is not declared for % (DEC-14, REQ-BE-4)',
            NEW.report_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.body ->> 'report_id' IS DISTINCT FROM NEW.report_id
       OR NEW.body ->> 'kind' IS DISTINCT FROM NEW.kind
       OR NEW.body ->> 'network' IS DISTINCT FROM NEW.network
       OR NEW.body ->> 'asset' IS DISTINCT FROM NEW.asset THEN
        RAISE EXCEPTION 'report % refused: its columns are not what its document says (REQ-BE-4)',
            NEW.report_id
            USING ERRCODE = 'BZ001';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('bezent/attested-report/' || NEW.network, 0));
    SELECT COALESCE(max(r.seq), 0) + 1 INTO NEW.seq
    FROM attested_reports r WHERE r.network = NEW.network;
    NEW.received_at := clock_timestamp();
    RETURN NEW;
END
$$;

CREATE TRIGGER attested_reports_guard
    BEFORE INSERT ON attested_reports
    FOR EACH ROW EXECUTE FUNCTION attested_reports_guard();

CREATE TRIGGER attested_reports_append_only
    BEFORE UPDATE OR DELETE ON attested_reports
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-BE-4');
CREATE TRIGGER attested_reports_no_truncate
    BEFORE TRUNCATE ON attested_reports
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-BE-4');

-- ---------------------------------------------------------------------------------------------
-- The report is the observation (REQ-BE-5).
--
-- By kind, on the attested network, at (seq, digest, log 0), final at once (REQ-OB-7: the party
-- reports a payment only once its rail guarantees it):
--
--   arrival      kind `arrival`, event Arrival: from the funder, if named, to the reserve address
--   payout       kind `payout_leg`, event Payout: from the reserve address to the transfer's
--                destination, with the reference the book finds its transfer by
--   positions    kind `other`, event Positions: no movement; the reserve check reads it
--   reversal     kind `other`, event Reversal: the reserve falls; the book posts it (REQ-BE-6)
--   restoration  kind `other`, event Restoration: the reserve is restored; the book posts it
--
-- In number order, none skipped, each once. Returns the observation; NULL when it is already one.
-- SECURITY DEFINER: the core holds no write on the chain tables. It runs as the owner, which
-- `observation_writer_covers()` admits; `endpoint_guard_scope()` looks at the session's login too, and
-- the core's holds no endpoint role.
CREATE FUNCTION bezent_record_attested_observation(p_report BIGINT) RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    r         attested_reports%ROWTYPE;
    t         transfers%ROWTYPE;
    reserve   TEXT;
    o_kind    TEXT;
    o_event   TEXT;
    o_amount  NUMERIC;
    o_sender  TEXT;
    o_to      TEXT;
    o_ref     BYTEA;
    o_detail  JSONB;
    recorded  BIGINT;
BEGIN
    SELECT * INTO r FROM attested_reports WHERE id = p_report;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'no report %', p_report USING ERRCODE = 'BZ001';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('bezent/attested-observation/' || r.network, 0));
    IF EXISTS (SELECT 1 FROM observations o
               WHERE o.network = r.network AND o.block_number = r.seq) THEN
        RETURN NULL;
    END IF;
    IF r.seq > 1 AND NOT EXISTS (SELECT 1 FROM observations o
                                 WHERE o.network = r.network AND o.block_number = r.seq - 1) THEN
        RAISE EXCEPTION 'report % of % waits for report % (REQ-OB-3)', r.seq, r.network, r.seq - 1
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT a.address INTO reserve FROM reserves a
    WHERE a.asset = r.asset AND a.network = r.network;
    IF reserve IS NULL OR NOT EXISTS (SELECT 1 FROM admitted_tokens k
                                      WHERE k.network = r.network AND k.asset = r.asset
                                        AND k.token = r.body ->> 'token') THEN
        RAISE EXCEPTION 'report % of %: no reserve of % with that token there (REQ-AS-2, REQ-BE-1)',
            r.report_id, r.network, r.asset
            USING ERRCODE = 'BZ001';
    END IF;
    o_amount := (r.body ->> 'amount')::numeric;
    o_detail := jsonb_strip_nulls(jsonb_build_object(
        'party', r.party, 'report_id', r.report_id, 'report_kind', r.kind,
        'ids', r.body -> 'ids', 'position', r.body -> 'position',
        'occurred_at', r.body -> 'occurred_at'));
    CASE r.kind
        WHEN 'arrival' THEN
            o_kind := 'arrival';
            o_event := 'Arrival';
            o_sender := r.body ->> 'funder';
            o_to := reserve;
        WHEN 'payout' THEN
            SELECT * INTO t FROM transfers x
            WHERE x.bridge_reference = r.body ->> 'bridge_reference'
              AND x.kind = 'withdrawal' AND x.destination_network = r.network
              AND x.asset = r.asset;
            IF NOT FOUND THEN
                RAISE EXCEPTION 'report % of %: its payout is of no withdrawal of % there (REQ-OT-2)',
                    r.report_id, r.network, r.asset
                    USING ERRCODE = 'BZ001';
            END IF;
            o_kind := 'payout_leg';
            o_event := 'Payout';
            o_sender := reserve;
            o_to := t.destination;
            o_ref := t.destination_reference;
        WHEN 'positions' THEN
            o_kind := 'other';
            o_event := 'Positions';
            o_amount := NULL;
            o_detail := o_detail || jsonb_build_object('positions', r.body -> 'positions',
                                                       'through', r.body -> 'through');
        WHEN 'reversal' THEN
            o_kind := 'other';
            o_event := 'Reversal';
            o_sender := reserve;
            o_detail := o_detail || jsonb_build_object('reverses', r.body -> 'reverses');
        WHEN 'restoration' THEN
            o_kind := 'other';
            o_event := 'Restoration';
            o_detail := o_detail || jsonb_build_object('restores', r.body -> 'restores');
    END CASE;
    o_detail := o_detail || jsonb_build_object('event', o_event);
    INSERT INTO observations (network, block_number, block_hash, block_time, position_kind,
                              position_index, token, amount, sender, recipient,
                              on_chain_reference, executed, detail, kind)
    VALUES (r.network, r.seq, r.digest, r.received_at, 'log', 0, r.body ->> 'token', o_amount,
            o_sender, o_to, o_ref, TRUE, o_detail, o_kind)
    RETURNING id INTO recorded;
    INSERT INTO final_heads (network, block_number, block_hash, block_time)
    VALUES (r.network, r.seq, r.digest, r.received_at);
    INSERT INTO observation_outcomes (observation_id, outcome, final_block_number, final_block_hash)
    VALUES (recorded, 'final', r.seq, r.digest);
    RETURN recorded;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- No order on an attested network (REQ-RQ-9; review S2): whoever writes it.

CREATE FUNCTION intents_guard_attested() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM networks n
               WHERE n.network = NEW.network AND n.backend LIKE 'attested:%') THEN
        RAISE EXCEPTION 'intent on % refused: bezent signs nothing on an attested backend; its payout is the party''s (REQ-RQ-9, DEC-14)',
            NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_guard_attested
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_attested();

-- ---------------------------------------------------------------------------------------------
-- Grants (crates/store/src/grants.rs states them). No function here is PUBLIC's (0017's F1).

REVOKE EXECUTE ON FUNCTION
    attesting_parties_guard(),
    attested_reports_guard(),
    bezent_record_attested_observation(BIGINT),
    intents_guard_attested()
FROM PUBLIC;

GRANT SELECT ON attesting_parties, attesting_keys TO bezent_core, bezent_ingress;
GRANT SELECT ON attested_reports TO bezent_core, bezent_operator;
GRANT SELECT, INSERT ON attested_reports TO bezent_ingress;
GRANT EXECUTE ON FUNCTION bezent_record_attested_observation(BIGINT) TO bezent_core;

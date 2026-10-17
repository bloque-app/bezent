-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #42 (F-004 follow-up): the reserve check and the daily statement in the running core.
-- Additive.
--
--   * reserve_readings: what an endpoint read at a final head it recorded, for the reserve check
--     (REQ-BK-8, NFR-5, INV-1): the issuing network's supply of each bridged asset (the on-chain
--     component's own count and the asset's issuance, both recorded, a mismatch flagged), and an
--     origin network's balance of each admitted token at the reserve, from both providers
--     (DEC-12: providers that disagree make the reading unknown). A convertible's balance at the
--     reserve is recorded the same way, outside the equation (REQ-CV-4). Each reading rests on a
--     final head of its network (REQ-OB-5), so the check reads every term at one point per network.
--     Only that network's endpoint records it (0034's scopes; 0035's EVM guard).
--   * reserve_evaluations.book_snapshot: the snapshot an evaluation read the book under. The book
--     and the statement's movements are cut by commit order (an entry is in the cut when its
--     transaction had committed when the evaluation read), not by id: an entry with a lower id
--     that commits after the evaluation is no longer in its statement (PR #40 second round, (1)).
--   * Statement intents (PLAN DEC-16, 2026-10-14): the core records an intent of purpose
--     `statement` carrying the statement's exact bytes; the signer signs it with the statement key
--     only. The store holds its shape, and records an ed25519 signature over a statement intent
--     only, and none other over one.
--   * The lag bound is also re-checked against the route budgets when a network's role changes
--     (a network that becomes the issuing one serves every route; PR #40 second round, (2), whose
--     budget case 0033 closed).

-- ---------------------------------------------------------------------------------------------
-- Readings at final heads.

CREATE TABLE reserve_readings (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network        TEXT NOT NULL CHECK (network <> ''),
    block_number   BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash     BYTEA NOT NULL CHECK (length(block_hash) > 0),
    -- supply: an issuing asset's total on the issuing network; reserve: an admitted token's
    -- balance at a reserve address; convertible: a convertible's balance there (REQ-CV-4).
    kind           TEXT NOT NULL CHECK (kind IN ('supply', 'reserve', 'convertible')),
    -- The issuing asset (supply), or the token as the store spells it.
    subject        TEXT NOT NULL CHECK (subject <> ''),
    -- The reserve address; empty for supply.
    holder         TEXT NOT NULL DEFAULT '',
    -- In the network's minor units. NULL is unknown, with its reason: never a guess (REQ-NX-3).
    amount         minor_units,
    unknown_reason TEXT CHECK (unknown_reason <> ''),
    -- Supply only: the on-chain component's count disagrees with the asset's issuance.
    mismatch       BOOLEAN NOT NULL DEFAULT false,
    -- What each source answered.
    detail         JSONB NOT NULL DEFAULT '{}',
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT reserve_readings_once UNIQUE (network, block_number, kind, subject, holder),
    CONSTRAINT reserve_readings_known_or_why CHECK ((amount IS NULL) = (unknown_reason IS NOT NULL)),
    CONSTRAINT reserve_readings_holder CHECK ((kind = 'supply') = (holder = '')),
    CONSTRAINT reserve_readings_mismatch_is_supply CHECK (NOT mismatch OR kind = 'supply')
);

COMMENT ON TABLE reserve_readings IS
    'REQ-BK-8, NFR-5, INV-1, REQ-OB-5, DEC-12: what an endpoint read at a final head it recorded: '
    'supply on the issuing network, reserve and convertible balances on an origin network. NULL '
    'amount is unknown, with its reason. Append-only; only the network''s endpoint records it.';

CREATE TRIGGER reserve_readings_append_only
    BEFORE UPDATE OR DELETE ON reserve_readings
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-BK-8, INV-16');
CREATE TRIGGER reserve_readings_no_truncate
    BEFORE TRUNCATE ON reserve_readings
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-BK-8, INV-16');

CREATE FUNCTION reserve_readings_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    network_role TEXT;
BEGIN
    IF NOT observation_writer_covers(NEW.network, TG_RELID) THEN
        RAISE EXCEPTION 'reading on % refused: only that network''s endpoint records its readings (REQ-OB-6, INV-8)',
            NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT role INTO network_role FROM networks WHERE network = NEW.network;
    IF NOT ((NEW.kind = 'supply' AND network_role = 'issuing')
            OR (NEW.kind IN ('reserve', 'convertible') AND network_role = 'origin')) THEN
        RAISE EXCEPTION 'reading on % refused: a % network records no % reading (REQ-BK-8)',
            NEW.network, coalesce(network_role, 'undeclared'), NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM final_heads h
                   WHERE h.network = NEW.network AND h.block_number = NEW.block_number
                     AND h.block_hash = NEW.block_hash) THEN
        RAISE EXCEPTION 'reading on % refused: block % with that hash is not a recorded final head (REQ-OB-5, REQ-BK-8)',
            NEW.network, NEW.block_number
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER reserve_readings_guard_insert
    BEFORE INSERT ON reserve_readings
    FOR EACH ROW EXECUTE FUNCTION reserve_readings_guard_insert();
-- 0035: a login holding the EVM endpoint's role writes EVM networks' rows only.
CREATE TRIGGER reserve_readings_evm_endpoint_guard
    BEFORE INSERT ON reserve_readings
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();

REVOKE ALL ON reserve_readings FROM PUBLIC;
-- The core reads them through the service role its login holds (as every endpoint record).
GRANT SELECT ON reserve_readings TO bezent_service, bezent_operator;
GRANT INSERT ON reserve_readings TO bezent_endpoint;

-- ---------------------------------------------------------------------------------------------
-- The book an evaluation read, by commit order.

ALTER TABLE reserve_evaluations ADD COLUMN book_snapshot pg_snapshot;

COMMENT ON COLUMN reserve_evaluations.book_snapshot IS
    'REQ-BK-1, REQ-PR-1: the snapshot the evaluation read the book under. An entry is in its cut '
    'when its recording transaction (book_entries.recorded_tx) is visible in it: committed before '
    'the read, whatever its id. With book_entry_cut, the last entry id visible then.';

-- ---------------------------------------------------------------------------------------------
-- Statement intents (DEC-16, 2026-10-14).
--
-- An intent of purpose `statement` carries the statement's exact bytes as `message`, `amount` "0"
-- (it moves nothing), and `statement`: {asset, sequence, sha256} with sha256 the SHA-256 of the
-- bytes. It names no transfer, no on-chain reference and no prepared transaction, is attempt 1, and
-- is recorded on the issuing network (the signer holds the statement key's environment to it,
-- REQ-KY-6). No other intent carries `statement`.

CREATE OR REPLACE FUNCTION intents_guard_prepared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    -- A statement intent signs a statement, not a prepared transaction: its own guard holds it.
    IF NEW.purpose = 'statement' THEN
        RETURN NEW;
    END IF;
    IF NOT (NEW.payload ? 'message' OR NEW.payload ? 'prepared') THEN
        RETURN NEW;
    END IF;
    IF jsonb_typeof(NEW.payload -> 'prepared') IS DISTINCT FROM 'number'
       OR jsonb_typeof(NEW.payload -> 'message') IS DISTINCT FROM 'string'
       OR jsonb_typeof(NEW.payload -> 'amount') IS DISTINCT FROM 'string' THEN
        RAISE EXCEPTION 'signing intent refused: it must name its prepared transaction, and carry its message and amount (INV-9, issue #38)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM prepared_transactions p
        JOIN intents o ON o.id = p.order_intent_id
        WHERE p.id::text = NEW.payload ->> 'prepared'
          AND o.payload ? 'order' AND NOT o.payload ? 'prepared' AND NOT o.payload ? 'message'
          AND p.network = NEW.network AND o.network = NEW.network
          AND o.transfer_id IS NOT DISTINCT FROM NEW.transfer_id
          AND o.attempt = NEW.attempt
          AND o.purpose = NEW.purpose
          AND o.on_chain_reference IS NOT DISTINCT FROM NEW.on_chain_reference
          AND o.reference_scope IS NOT DISTINCT FROM NEW.reference_scope
          AND p.dry_run_passed
          AND lower(NEW.payload ->> 'message') = '0x' || encode(p.message, 'hex')
          AND jsonb_typeof(o.payload -> 'amount') = 'string'
          AND NEW.payload -> 'amount' = o.payload -> 'amount') THEN
        RAISE EXCEPTION 'signing intent refused: its bytes and amount are not those of prepared transaction % of its own order on % (INV-9, issue #38)',
            NEW.payload ->> 'prepared', NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- SECURITY DEFINER, as intents_guard_prepared: the core's role alone does not read `assets`.
CREATE FUNCTION intents_guard_statement() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    message TEXT;
    s JSONB;
BEGIN
    IF NEW.purpose <> 'statement' AND NOT NEW.payload ? 'statement' THEN
        RETURN NEW;
    END IF;
    message := NEW.payload ->> 'message';
    s := NEW.payload -> 'statement';
    IF NEW.purpose <> 'statement'
       OR NEW.transfer_id IS NOT NULL OR NEW.on_chain_reference IS NOT NULL
       OR NEW.reference_scope IS NOT NULL OR NEW.attempt <> 1
       OR NEW.payload ? 'prepared' OR NEW.payload ? 'order'
       OR NEW.payload -> 'amount' IS DISTINCT FROM '"0"'::jsonb
       OR jsonb_typeof(NEW.payload -> 'message') IS DISTINCT FROM 'string'
       OR message !~ '^0x([0-9a-f]{2})+$'
       OR jsonb_typeof(s) IS DISTINCT FROM 'object'
       OR jsonb_typeof(s -> 'asset') IS DISTINCT FROM 'string'
       OR jsonb_typeof(s -> 'sequence') IS DISTINCT FROM 'number'
       OR (s ->> 'sequence') !~ '^[1-9][0-9]{0,17}$'
       OR jsonb_typeof(s -> 'sha256') IS DISTINCT FROM 'string'
       OR s ->> 'sha256' <> '0x' || encode(sha256(decode(substr(message, 3), 'hex')), 'hex')
       OR NOT EXISTS (SELECT 1 FROM assets a WHERE a.asset = s ->> 'asset')
       OR NOT EXISTS (SELECT 1 FROM networks n WHERE n.network = NEW.network AND n.role = 'issuing') THEN
        RAISE EXCEPTION 'statement intent refused: it carries a statement''s exact bytes, amount 0 and {asset, sequence, sha256} of those bytes, on the issuing network, and nothing of a transfer (REQ-PR-1, DEC-16, INV-9)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_guard_statement
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_statement();

CREATE INDEX intents_statement ON intents ((payload -> 'statement' ->> 'sha256'))
    WHERE purpose = 'statement';

-- The statement key's signatures are recorded like every other (ed25519, REQ-PR-1), and only over
-- a statement intent.
ALTER TABLE signatures DROP CONSTRAINT signatures_scheme_check;
ALTER TABLE signatures ADD CONSTRAINT signatures_scheme_check
    CHECK (scheme IN ('sr25519', 'ecdsa', 'secp256k1-evm', 'ed25519'));

CREATE FUNCTION signatures_ed25519_statements_only() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF (NEW.scheme = 'ed25519')
       IS DISTINCT FROM EXISTS (SELECT 1 FROM intents i
                                WHERE i.id = NEW.intent_id AND i.purpose = 'statement') THEN
        RAISE EXCEPTION 'signature for intent % refused: the ed25519 statement key signs statement intents, and only it does (REQ-PR-1, DEC-16)',
            NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER signatures_ed25519_statements_only
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_ed25519_statements_only();

-- ---------------------------------------------------------------------------------------------
-- The lag bound against the route budgets when a network's role changes.

DROP TRIGGER networks_reserve_point_lag_within_budget ON networks;
CREATE TRIGGER networks_reserve_point_lag_within_budget
    BEFORE INSERT OR UPDATE OF reserve_point_lag_seconds, finality_seconds, role ON networks
    FOR EACH ROW EXECUTE FUNCTION reserve_point_lag_within_budget();

-- ---------------------------------------------------------------------------------------------
-- No function here is PUBLIC's (0017's F1). The trigger functions fire without an EXECUTE grant.

REVOKE EXECUTE ON FUNCTION
    reserve_readings_guard_insert(),
    intents_guard_statement(),
    intents_guard_prepared(),
    signatures_ed25519_statements_only()
FROM PUBLIC;

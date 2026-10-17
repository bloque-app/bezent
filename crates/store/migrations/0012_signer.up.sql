-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-005 T-005-01: the signer's own role and its signature table (SPEC §4.1 REQ-AR-3, §9 INV-9;
-- PLAN DEC-3).
--
-- The signer is a process apart from the one that holds bezent's state, and it holds no state of
-- its own. It logs in as a role that is a member of `bezent_signer` and of nothing else. That role
-- may:
--   * SELECT on intents: the signer reads the intent the core recorded, and its exact bytes;
--   * SELECT and INSERT on signatures: it returns a signature it already made (AC-D2.1), and
--     records a new one, append-only.
-- Nothing else: it cannot write an intent, a transfer, the book or the log, and it owns nothing,
-- so it cannot ALTER, DROP or disable a trigger.
--
-- The store backs the signer's guards (DEC-3: a second line; the on-chain component stays the
-- first, REQ-AR-5):
--   * one signature per intent, forever: a restart returns the recorded one (REQ-EO-1, AC-D2.1);
--   * one signature per on-chain reference: the reference is copied from the intent, never taken
--     from the signer (INV-2);
--   * the amount is copied from the intent as well, so the per-window cap sums what the core
--     recorded (INV-11: exact minor units).
-- The service role reads signatures (it builds and submits the transaction, and matches signatures
-- seen on chain against them, AC-I1.2) and never writes one.
--
-- "Recorded" in INV-9 means recorded by the coordinator (review S5). Every service logs in as a
-- member of `bezent_service`: ingress and the endpoints as well as the core. So `bezent_service`
-- may no longer insert an intent; only `bezent_core`, the role the core's login holds besides
-- `bezent_service`, may. A compromised endpoint or ingress cannot record an intent for the signer.

DO $$
DECLARE
    role_name TEXT;
BEGIN
    FOREACH role_name IN ARRAY ARRAY['bezent_signer', 'bezent_core'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = role_name) THEN
            BEGIN
                EXECUTE format('CREATE ROLE %I NOLOGIN', role_name);
            EXCEPTION WHEN duplicate_object OR unique_violation THEN
                NULL;  -- created concurrently by another database's migration on the same server
            END;
        END IF;

        IF current_user = role_name
           OR EXISTS (SELECT 1 FROM pg_auth_members m
                      JOIN pg_roles g ON g.oid = m.roleid
                      JOIN pg_roles u ON u.oid = m.member
                      WHERE g.rolname = role_name AND u.rolname = current_user) THEN
            RAISE EXCEPTION 'migrations run as the migration owner, never as %, (%)', role_name, current_user
                USING ERRCODE = 'BZ001';
        END IF;
        IF EXISTS (SELECT 1 FROM pg_roles
                   WHERE rolname = role_name AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
            RAISE EXCEPTION '% must not be a superuser, bypass row security or create roles', role_name
                USING ERRCODE = 'BZ001';
        END IF;
    END LOOP;
    -- The signer is never also the coordinator (REQ-AR-3).
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE (g.rolname IN ('bezent_service', 'bezent_core') AND u.rolname = 'bezent_signer')
                  OR (g.rolname = 'bezent_signer' AND u.rolname IN ('bezent_service', 'bezent_core'))) THEN
        RAISE EXCEPTION 'bezent_signer and the coordinator''s roles must not be members of each other (REQ-AR-3)'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

-- Only the coordinator records an intent (INV-9; review S5).
REVOKE INSERT ON intents FROM bezent_service;
GRANT USAGE ON SCHEMA public TO bezent_core;
GRANT SELECT, INSERT ON intents TO bezent_core;

CREATE TABLE signatures (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    intent_id          BIGINT NOT NULL REFERENCES intents (id),
    -- The signer's name for the key, and what a verifier needs: never key material.
    key_id             TEXT NOT NULL CHECK (key_id <> ''),
    scheme             TEXT NOT NULL CHECK (scheme IN ('sr25519', 'ecdsa', 'secp256k1-evm')),
    public_key         BYTEA NOT NULL CHECK (length(public_key) > 0),
    signature          BYTEA NOT NULL CHECK (length(signature) > 0),
    -- Copied from the intent by the trigger below; the signer cannot set them.
    on_chain_reference BYTEA,
    amount             minor_units NOT NULL,
    signed_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT signatures_once_per_intent UNIQUE (intent_id)
);

-- A one-time reference is signed for once (INV-2; DEC-3's second line).
CREATE UNIQUE INDEX signatures_once_per_reference
    ON signatures (on_chain_reference) WHERE on_chain_reference IS NOT NULL;

-- The per-window cap sums a key's recent signatures.
CREATE INDEX signatures_by_key_and_time ON signatures (key_id, signed_at);

-- Copies the on-chain reference and the amount from the recorded intent, over anything the insert
-- named (INV-9: the signature answers an intent, and says nothing the intent does not). The
-- amount is the intent payload's field `amount`, a decimal string of minor units (INV-11); an
-- intent without one is refused.
-- It runs as the inserting role: the signer may read intents.
CREATE FUNCTION signatures_from_intent() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    rec intents%ROWTYPE;
    declared JSONB;
BEGIN
    SELECT * INTO rec FROM intents WHERE id = NEW.intent_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'signature for intent % refused: no such intent (INV-9)', NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    declared := rec.payload -> 'amount';
    IF declared IS NULL OR jsonb_typeof(declared) <> 'string' OR NOT (declared #>> '{}') ~ '^[0-9]+$' THEN
        RAISE EXCEPTION 'signature for intent % refused: the intent has no amount in minor units (INV-11)', NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    NEW.on_chain_reference := rec.on_chain_reference;
    NEW.amount := (declared #>> '{}')::minor_units;
    RETURN NEW;
END
$$;

CREATE TRIGGER signatures_from_intent
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_from_intent();
CREATE TRIGGER signatures_append_only
    BEFORE UPDATE OR DELETE ON signatures
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');
CREATE TRIGGER signatures_no_truncate
    BEFORE TRUNCATE ON signatures
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');

REVOKE ALL ON signatures FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO bezent_signer;
GRANT SELECT ON intents TO bezent_signer;
GRANT SELECT, INSERT ON signatures TO bezent_signer;
GRANT SELECT ON signatures TO bezent_service;

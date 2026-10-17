-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-007 review S1, S2, M1: who may write what the endpoint proves (SPEC §8.2 REQ-NX-1/2, §8.3
-- REQ-EO-1/6, §9 INV-8; PLAN DEC-3).
--
-- A proof of non-execution is what lets the core refund on the origin network; a false one is a
-- refund on top of an executed mint (INV-8). Every service logs in as a member of
-- `bezent_service`, the internet-facing ingress included, so `bezent_service` may no longer insert
-- the endpoint's records: proofs, prepared transactions and read ranges. Only `bezent_endpoint`
-- may, the role an endpoint's login is a member of. It is a member of `bezent_service`, so an
-- endpoint keeps what every service may do (observations, signed bytes, the event log), and
-- neither the ingress nor the core can become it.
--
-- A proof must also describe the transaction it proves (S2): its signer, nonce and expiry are its
-- prepared transaction's, whose bytes the endpoint decoded and checked before asking for a
-- signature (M2).
--
-- One signing intent per prepared transaction (M1): two intents naming the same bytes would get
-- two signatures over one payload, two transactions that are one mint.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_endpoint') THEN
        BEGIN
            CREATE ROLE bezent_endpoint NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;

    IF current_user = 'bezent_endpoint'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_endpoint' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_endpoint (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_endpoint' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_endpoint must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    -- Neither the signer nor the ingress's role may hold the endpoint's (REQ-AR-3).
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname = 'bezent_endpoint'
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_signer')) THEN
        RAISE EXCEPTION 'bezent_service, bezent_core and bezent_signer must not be members of bezent_endpoint'
            USING ERRCODE = 'BZ001';
    END IF;

    IF NOT EXISTS (SELECT 1 FROM pg_auth_members m
                   JOIN pg_roles g ON g.oid = m.roleid
                   JOIN pg_roles u ON u.oid = m.member
                   WHERE g.rolname = 'bezent_service' AND u.rolname = 'bezent_endpoint') THEN
        BEGIN
            GRANT bezent_service TO bezent_endpoint;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- granted concurrently by another database's migration on the same server
        END;
    END IF;
END
$$;

REVOKE INSERT ON non_execution_proofs, prepared_transactions, observation_ranges FROM bezent_service;
GRANT USAGE ON SCHEMA public TO bezent_endpoint;
GRANT INSERT ON non_execution_proofs, prepared_transactions, observation_ranges TO bezent_endpoint;

-- S2: a proof answers a signed transaction of its own network, and names that transaction's
-- signer, nonce and expiry, as its prepared transaction recorded them (REQ-NX-2).
CREATE OR REPLACE FUNCTION non_execution_proofs_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM signed_transactions st JOIN intents i ON i.id = st.intent_id
                   WHERE st.id = NEW.signed_transaction_id AND i.network = NEW.network) THEN
        RAISE EXCEPTION 'proof for signed transaction % refused: no such signed transaction on % (REQ-NX-2)',
            NEW.signed_transaction_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM signed_transactions st
                   JOIN intents i ON i.id = st.intent_id
                   JOIN prepared_transactions p ON p.id::text = i.payload ->> 'prepared'
                   WHERE st.id = NEW.signed_transaction_id AND p.network = NEW.network
                     AND p.lane = NEW.signer AND p.nonce = NEW.nonce
                     AND p.valid_from_block = NEW.valid_from_block
                     AND p.valid_until_block = NEW.valid_until_block) THEN
        RAISE EXCEPTION 'proof for signed transaction % refused: its signer, nonce or expiry is not the transaction''s (REQ-NX-2)',
            NEW.signed_transaction_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- M1: one signing intent per prepared transaction.
CREATE UNIQUE INDEX intents_one_signing_intent_per_prepared
    ON intents ((payload ->> 'prepared')) WHERE payload ? 'prepared';

-- SPDX-License-Identifier: AGPL-3.0-or-later
-- PR #39 third review MF-A and SF-A (issue #38; SPEC §9 INV-9, AC-I1.2; PLAN DEC-3).
--
-- 0036 checked a signing intent only when it named a prepared transaction, and never compared
-- what it carries with what was prepared. So a core (buggy, or compromised: the case #38 is about)
-- could record, for order A, an intent carrying order B's bytes, or an intent that names no
-- preparation at all; the signer signs `message` as it is, and its one-time-reference guard and
-- window cap read order A's reference and the amount the intent claims (MF-A).
--
-- From here, whoever inserts, an intent that carries bytes to sign (`message`) or names a
-- preparation (`prepared`) is a signing intent of a delivery, and it is recorded only when:
--
--   * it names (`prepared`) a prepared transaction whose dry run passed;
--   * that prepared transaction answers a delivery order of this intent's own: the same network
--     (the intent's, the order's and the prepared row's), transfer, attempt, purpose, on-chain
--     reference and reference scope;
--   * its `message` is exactly the prepared transaction's `message` (the bytes the key signs), as
--     `0x` hex;
--   * its `amount` is exactly its order's `amount`.
--
-- SF-A: no session is exempt, the owner's and a superuser's included (0036 exempted them). A
-- restore (NFR-7) does not need it: plain `pg_dump` writes every CREATE TRIGGER in its post-data
-- section, after the data, so a `pg_dump` + `psql` restore loads `intents` before this trigger
-- exists (the restore drill writes such an intent and passes).
--
-- No row already in a table is touched or checked: the tables are append-only, and the rule
-- holds from here on.

CREATE OR REPLACE FUNCTION intents_guard_prepared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
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

REVOKE EXECUTE ON FUNCTION intents_guard_prepared() FROM PUBLIC;

-- MF-A, the signer's side: before it signs, the signer reads the prepared transaction the intent
-- names and compares bytes, amount, order and network itself (services/signer, `INV-9`). It reads
-- and never writes.
GRANT SELECT ON prepared_transactions TO bezent_signer;

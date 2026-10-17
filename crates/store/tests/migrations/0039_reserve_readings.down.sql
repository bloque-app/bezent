-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #42's readings, book snapshot and statement intents.
DROP TRIGGER networks_reserve_point_lag_within_budget ON networks;
CREATE TRIGGER networks_reserve_point_lag_within_budget
    BEFORE INSERT OR UPDATE OF reserve_point_lag_seconds, finality_seconds ON networks
    FOR EACH ROW EXECUTE FUNCTION reserve_point_lag_within_budget();
DROP TRIGGER signatures_ed25519_statements_only ON signatures;
DROP FUNCTION signatures_ed25519_statements_only();
ALTER TABLE signatures DROP CONSTRAINT signatures_scheme_check;
ALTER TABLE signatures ADD CONSTRAINT signatures_scheme_check
    CHECK (scheme IN ('sr25519', 'ecdsa', 'secp256k1-evm'));
DROP INDEX intents_statement;
DROP TRIGGER intents_guard_statement ON intents;
DROP FUNCTION intents_guard_statement();
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
ALTER TABLE reserve_evaluations DROP COLUMN book_snapshot;
DROP TABLE reserve_readings;
DROP FUNCTION reserve_readings_guard_insert();

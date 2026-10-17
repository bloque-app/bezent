-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): the check as 0036 left it.
REVOKE SELECT ON prepared_transactions FROM bezent_signer;
CREATE OR REPLACE FUNCTION intents_guard_prepared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NEW.payload ? 'prepared'
       AND NOT EXISTS (SELECT 1 FROM pg_roles u
                       WHERE u.rolname = session_user
                         AND (u.rolsuper
                              OR pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')))
       AND NOT EXISTS (
        SELECT 1 FROM prepared_transactions p
        JOIN intents o ON o.id = p.order_intent_id
        WHERE p.id::text = NEW.payload ->> 'prepared'
          AND o.payload ? 'order'
          AND p.network = NEW.network AND o.network = NEW.network
          AND o.transfer_id IS NOT DISTINCT FROM NEW.transfer_id
          AND o.attempt = NEW.attempt
          AND o.purpose = NEW.purpose
          AND o.on_chain_reference IS NOT DISTINCT FROM NEW.on_chain_reference
          AND o.reference_scope IS NOT DISTINCT FROM NEW.reference_scope) THEN
        RAISE EXCEPTION 'signing intent refused: prepared transaction % is not one of its own order on % (INV-9, issue #38)',
            NEW.payload ->> 'prepared', NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

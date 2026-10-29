-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): the ed25519 delivery keys' guard, back to 0039's.
CREATE OR REPLACE FUNCTION signatures_ed25519_statements_only() RETURNS trigger
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
DROP INDEX signatures_ed25519_by_key;

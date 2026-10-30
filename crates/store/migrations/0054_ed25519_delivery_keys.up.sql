-- SPDX-License-Identifier: GPL-3.0-or-later
-- Owner ruling d-delivery-keys (2026-10-29): a Kreivo delivery account may be a native ed25519
-- account, whose key is a Vault Transit key. 0039 let an ed25519 signature be recorded over a
-- statement intent only, which made the scheme the statement key's alone; the key, not the scheme,
-- is what REQ-PR-1 reserves ("a key used for nothing else"). The rule is now stated on keys:
--
--   * a statement intent is signed with ed25519, and only with it;
--   * an ed25519 public key signs statement intents or other intents, never both, so the statement
--     key signs nothing else and no delivery key ever signs a statement.
--
-- The signer's configuration names the statement key and refuses to bind it, or any key with its
-- public key, to a network (REQ-PR-1); this is the store's second line. The function is replaced
-- in place, so its trigger and its revoked EXECUTE stay as 0039 made them. Additive otherwise.

CREATE INDEX signatures_ed25519_by_key ON signatures (public_key) WHERE scheme = 'ed25519';

CREATE OR REPLACE FUNCTION signatures_ed25519_statements_only() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    is_statement BOOLEAN;
BEGIN
    is_statement := EXISTS (SELECT 1 FROM intents i
                         WHERE i.id = NEW.intent_id AND i.purpose = 'statement');
    IF is_statement AND NEW.scheme <> 'ed25519' THEN
        RAISE EXCEPTION 'signature for intent % refused: a statement intent is signed by the ed25519 statement key, and only by it (REQ-PR-1, DEC-16)',
            NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.scheme = 'ed25519' AND EXISTS (
           SELECT 1 FROM signatures g JOIN intents i ON i.id = g.intent_id
           WHERE g.scheme = 'ed25519' AND g.public_key = NEW.public_key
             AND (i.purpose = 'statement') IS DISTINCT FROM is_statement) THEN
        RAISE EXCEPTION 'signature for intent % refused: an ed25519 key signs statements or other intents, never both (REQ-PR-1, DEC-16)',
            NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

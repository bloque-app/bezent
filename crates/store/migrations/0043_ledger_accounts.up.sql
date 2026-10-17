-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #49 (F-002 follow-up): the declared list of ledger contract accounts (REQ-IN-5; PLAN
-- DEC-15, 2026-10-15).
--
-- Delivering into a ledger is the requester's, from its own account. Value that reaches a ledger's
-- contract account outside the contract's own calls credits no one, so the ingress refuses a
-- destination (or a refund destination) on this list with ERR-InvalidDestination, and the core
-- checks again before it orders a mint (defence in depth).
--
-- A declaration like the others: per network, with its actor and reason, append-only. It is written
-- by the migration owner or by the operator's role; the services only read it. An entry is never
-- edited and never deleted. It is withdrawn by a later 'unlisted' entry for the same account, with
-- its own actor and reason; the latest entry per (network, account) decides.
--
-- One canonical form: every entry is parsed at insert, by its network's address format, into the
-- account's canonical bytes (`account`): an EVM address in 20 bytes whatever its case; an SS58
-- address, or 0x and 64 hex digits, in the 32-byte public key whatever the prefix; an opaque
-- address byte for byte. An entry that does not parse is refused, so nothing malformed is on the
-- list. The services compare bytes. (The SS58 checksum and EIP-55 case are not checked here, as the
-- database has no BLAKE2b or Keccak; the services read an entry back through the same check and
-- refuse closed when it does not agree with its bytes.)
--
-- Additive: one table, one view, one function, grants. No existing row is touched, and no function
-- is executable by PUBLIC.

CREATE TABLE ledger_accounts (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network     TEXT NOT NULL REFERENCES networks (network),
    -- As declared. The canonical form is `account`, set by the trigger from this and the network.
    address     TEXT NOT NULL CHECK (address <> ''),
    -- A declaration lists the account; an 'unlisted' entry withdraws it (the latest decides).
    kind        TEXT NOT NULL DEFAULT 'declared' CHECK (kind IN ('declared', 'unlisted')),
    account     BYTEA NOT NULL,
    reason      TEXT NOT NULL CHECK (reason <> ''),
    actor       TEXT NOT NULL DEFAULT session_user CHECK (actor <> ''),
    declared_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ledger_accounts_by_account ON ledger_accounts (network, account, id DESC);

-- Base58 to bytes (Bitcoin alphabet); NULL when a character is outside it.
CREATE FUNCTION ledger_accounts_base58(p_text TEXT) RETURNS BYTEA
    LANGUAGE plpgsql IMMUTABLE
    SET search_path = pg_catalog, pg_temp
    AS $$
DECLARE
    alphabet CONSTANT TEXT := '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
    n NUMERIC := 0;
    digit INT;
    i INT;
    zeros INT := 0;
    out BYTEA := ''::bytea;
BEGIN
    FOR i IN 1..length(p_text) LOOP
        digit := position(substr(p_text, i, 1) IN alphabet);
        IF digit = 0 THEN
            RETURN NULL;
        END IF;
        n := n * 58 + (digit - 1);
    END LOOP;
    WHILE n > 0 LOOP
        out := set_byte('\x00'::bytea, 0, (n % 256)::int) || out;
        n := div(n, 256);
    END LOOP;
    i := 1;
    WHILE i <= length(p_text) AND substr(p_text, i, 1) = '1' LOOP
        zeros := zeros + 1;
        i := i + 1;
    END LOOP;
    RETURN decode(repeat('00', zeros), 'hex') || out;
END
$$;

-- Sets `account` from `address` by the network's address format; refuses what does not parse.
-- SECURITY DEFINER: it reads the network's address format, which the operator's role does not see.
CREATE FUNCTION ledger_accounts_canonical() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_catalog, pg_temp
    AS $$
DECLARE
    format TEXT;
    raw BYTEA;
BEGIN
    SELECT address_format INTO format FROM networks WHERE network = NEW.network;
    IF format = 'evm' THEN
        IF NEW.address !~ '^0x[0-9a-fA-F]{40}$' THEN
            RAISE EXCEPTION 'ledger account %: not an EVM address (0x and 40 hex digits) on % (REQ-IN-5)',
                NEW.address, NEW.network USING ERRCODE = 'BZ001';
        END IF;
        NEW.account := decode(lower(substr(NEW.address, 3)), 'hex');
    ELSIF format = 'ss58' THEN
        IF NEW.address ~ '^0x[0-9a-fA-F]{64}$' THEN
            NEW.account := decode(lower(substr(NEW.address, 3)), 'hex');
        ELSE
            raw := ledger_accounts_base58(NEW.address);
            -- 1-byte prefix: 35 bytes; 2-byte prefix: 36 bytes; the last two are the checksum.
            IF raw IS NULL OR NOT (
                   (length(raw) = 35 AND get_byte(raw, 0) < 64)
                OR (length(raw) = 36 AND get_byte(raw, 0) BETWEEN 64 AND 127)) THEN
                RAISE EXCEPTION 'ledger account %: not an SS58 address or 0x and 64 hex digits on % (REQ-IN-5)',
                    NEW.address, NEW.network USING ERRCODE = 'BZ001';
            END IF;
            NEW.account := substring(raw FROM length(raw) - 33 FOR 32);
        END IF;
    ELSIF format = 'opaque' THEN
        NEW.account := convert_to(NEW.address, 'UTF8');
    ELSE
        RAISE EXCEPTION 'ledger account %: network % has no address format (REQ-IN-5)',
            NEW.address, NEW.network USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER ledger_accounts_canonical
    BEFORE INSERT ON ledger_accounts
    FOR EACH ROW EXECUTE FUNCTION ledger_accounts_canonical();

COMMENT ON TABLE ledger_accounts IS
    'REQ-IN-5, DEC-15: contract accounts of a ledger on a network, as entries: declared, or unlisted '
    'again; the latest per (network, account) decides. A destination on the list is refused '
    '(ERR-InvalidDestination). Append-only; written by the migration owner or bezent_operator.';
COMMENT ON COLUMN ledger_accounts.account IS
    'The canonical bytes of the address, set at insert by the network''s address format; entries '
    'that do not parse are refused.';

CREATE TRIGGER ledger_accounts_append_only
    BEFORE UPDATE OR DELETE ON ledger_accounts
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-IN-5, DEC-15');
CREATE TRIGGER ledger_accounts_no_truncate
    BEFORE TRUNCATE ON ledger_accounts
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-IN-5, DEC-15');

-- What the services read: the accounts whose latest entry is a declaration.
CREATE VIEW ledger_accounts_listed AS
SELECT network, address, account, reason, actor, declared_at
FROM (
    SELECT DISTINCT ON (network, account) *
    FROM ledger_accounts
    ORDER BY network, account, id DESC
) latest
WHERE kind = 'declared';

REVOKE ALL ON ledger_accounts FROM PUBLIC;
REVOKE ALL ON ledger_accounts_listed FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ledger_accounts_base58(TEXT) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION ledger_accounts_canonical() FROM PUBLIC;
GRANT SELECT ON ledger_accounts, ledger_accounts_listed
    TO bezent_service, bezent_core, bezent_operator;
GRANT INSERT (network, address, kind, reason) ON ledger_accounts TO bezent_operator;

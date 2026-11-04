-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #45 (follow-up to the review of PR #42): a deposit's destination on the issuing network
-- decodes to a 32-byte account when the deposit is bound to its arrival or accepted, not after the
-- arrival is final. Additive: one trigger function and two `BEFORE INSERT` triggers (on `transfers`
-- and on `transitions`); no table changes, no row is read or touched, no grant changes.
--
-- 0050 reads the account a mint is to with `issuing_account(destination)` and refuses a mint whose
-- `to` is not that account. A destination for which the function is NULL (`0x` and a wrong number
-- of hex digits, an EVM address, an SS58 address with a trailing byte, a prefix of the wrong length)
-- was nevertheless taken in, and its deposit could never be minted: refused late, after the arrival
-- is final, with only the owner to resolve it. From here the same decode is a condition of the
-- binding, as 0049's destination-network rules are.
--
--   * When. Like 0049's `transfers_guard_destination`, which this follows: on the insert of a
--     deposit that names a source (a bound deposit, `Received` or `Source-Final`), and on the
--     transition `Received` to `Accepted` of a deposit (a deposit unbound at insert is bound only
--     before it is accepted). A refused request's transfer binds no source and is never accepted
--     (INV-2): the ingress records it, with the destination it was refused for, in `Refused`, and
--     that stays as it is, so a request whose destination is malformed is answered with
--     ERR-InvalidDestination exactly as before.
--   * Which rows. A deposit (`kind = 'deposit'`) whose `destination_network` is the issuing network
--     and that network's `address_format` is `ss58`: the networks `issuing_account` is defined for
--     (its decode is the one 0043 gives the ledger accounts of an `ss58` network). Withdrawals and
--     refunds are never read (a withdrawal's destination is on its own network, a refund's is the
--     redeemer's or the arrival's network, and none of them is the target of a mint). A deposit on
--     a network that is not the issuing network (0049 refuses it when bound), and one on a network
--     whose format is `evm` or `opaque`, is not read either: there is no 32-byte account to decode.
--   * Which writers. Every writer but the migration owner, as 0047's, 0049's and 0050's guards: the
--     owner's drills and restores (NFR-7) may carry a row the store has since learned to refuse.
--   * What it checks. Only the decode, never the SS58 checksum: the store has no BLAKE2, and the
--     ingress verifies the checksum when it takes the request (ERR-InvalidDestination), where it
--     stays. A destination the ingress accepts (a one- or two-byte prefix, 32 bytes, a checksum)
--     always decodes, so what a well-behaved request is answered does not change; the refusal
--     here is the answer to a writer the ingress did not check first.
--
-- Rows that already exist. Both triggers fire on INSERT only, and no row is validated, changed or
-- made invalid by the migration, which cannot fail on one. A deposit past `Received` is never read
-- again. A deposit still in `Received` that holds an undecodable destination is refused when it is
-- next accepted (it could not be minted either, as 0050 left it); it stays `Received` until it
-- expires or the owner resolves it. A later UPDATE of a transfer cannot change its destination
-- (0003, REQ-ST-7), so no row becomes undecodable after the insert.

CREATE FUNCTION transfers_guard_writer_destination_decodes() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    t         transfers%ROWTYPE;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF TG_TABLE_NAME = 'transfers' THEN
        IF NEW.source_network IS NULL THEN
            RETURN NEW;
        END IF;
        t := NEW;
    ELSE
        IF NOT (NEW.from_state = 'Received' AND NEW.to_state = 'Accepted') THEN
            RETURN NEW;
        END IF;
        SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
    END IF;
    IF t.kind = 'deposit'
       AND EXISTS (SELECT 1 FROM networks n
                   WHERE n.network = t.destination_network
                     AND n.role = 'issuing' AND n.address_format = 'ss58')
       AND issuing_account(t.destination) IS NULL THEN
        RAISE EXCEPTION 'deposit % refused: its destination must decode to a 32-byte account on the issuing network (0x and 64 hex digits, or an SS58 address of a one- or two-byte prefix, the account and a two-byte checksum), or no mint can ever be ordered to it (INV-6, REQ-IN-4, ERR-InvalidDestination)',
            t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- Triggers of one event fire by name: these are named to fire after `transfers_guard_destination`
-- (0049: the destination's network, REQ-RQ-7) and `transfers_guard_writer` (0047: who may create a
-- transfer), so a row either refuses first is answered with its refusal, as before.
CREATE TRIGGER transfers_guard_writer_destination_decodes
    BEFORE INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_writer_destination_decodes();
CREATE TRIGGER transitions_guard_writer_destination_decodes
    BEFORE INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_writer_destination_decodes();

REVOKE EXECUTE ON FUNCTION transfers_guard_writer_destination_decodes() FROM PUBLIC;

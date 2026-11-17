-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, review M3 of PR #139 (SPEC REQ-AS-11, a MUST; REQ-IN-10, REQ-IN-11): directing code is
-- declared by an explicit, recorded declaration (`directing_code`, 0147), and only declared code
-- directs. 0147 recorded the declaration for the statement; nothing read it before a directed arrival
-- was minted or returned, so an arrival the EVM endpoint's `[[directing]]` named, and the store did
-- not, would have been acted on all the same. Now the store holds the rule, whoever writes:
--
--   * directing_declared(): whether the directing contract an observation recorded
--     (`detail.directing`) is declared on its network for its token, from a block at or before the
--     observation's. Addresses compare as the network spells them: without case on an `evm` network,
--     exactly otherwise (as address_matches(), 0144, does).
--   * transfers_guard_directing_declared(): the deposit bezent makes from a directed arrival
--     (directed_deposit(), 0144) and its return (the refund of an unassociated arrival bound to one,
--     0144/0145) are refused when the arrival's directing contract is not declared so. It holds on
--     the insert, and on any update that binds a transfer to a directed arrival; the owner's drills
--     and restores included. A refund of an existing directed deposit (0145's review edge) is not
--     checked again: its deposit was.
--   * directed_return() (0145), the lifecycle's question whether a refund is the core's return of a
--     directed arrival (accepted with no second approver, Open 3), is true only for a declared one.
--
-- An undeclared directing contract's arrival is therefore neither minted nor returned: it waits,
-- final and unbound, an unassociated arrival an operator sees (`unassociated_arrivals`), until the
-- declaration is made (a declaration is never retroactive below its `from_block`) or a recorded
-- release returns it as any other unassociated arrival (REQ-IN-6). The core says so in its log
-- (`services/core/src/directed.rs`, `directing-undeclared`) and does not try.

CREATE FUNCTION directing_declared(
    p_network TEXT, p_token TEXT, p_directing TEXT, p_block BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = public, pg_temp
    AS $$
    SELECT p_directing IS NOT NULL AND p_directing <> '' AND p_token IS NOT NULL
       AND EXISTS (
        SELECT 1 FROM directing_code d JOIN networks n ON n.network = d.network
        WHERE d.network = p_network AND d.from_block <= p_block
          AND (d.address = p_directing
               OR (n.address_format = 'evm' AND lower(d.address) = lower(p_directing)))
          AND (d.token = p_token
               OR (n.address_format = 'evm' AND lower(d.token) = lower(p_token))))
$$;

COMMENT ON FUNCTION directing_declared(TEXT, TEXT, TEXT, BIGINT) IS
    'REQ-AS-11: whether directing contract p_directing is declared (directing_code) on p_network for '
    'token p_token from a block at or before p_block; without case on an evm network.';

CREATE FUNCTION transfers_guard_directing_declared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    o RECORD;
BEGIN
    IF NEW.source_network IS NULL OR NEW.refund_of IS NOT NULL
       OR NEW.kind NOT IN ('deposit', 'refund') THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE'
       AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index, NEW.kind)
           IS NOT DISTINCT FROM
           ROW(OLD.source_network, OLD.source_block_number, OLD.source_block_hash,
               OLD.source_position_kind, OLD.source_position_index, OLD.kind)
       AND OLD.refund_of IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT ob.network, ob.token, ob.block_number, ob.detail ->> 'directing' AS directing INTO o
    FROM observations ob
    WHERE ob.network = NEW.source_network AND ob.block_number = NEW.source_block_number
      AND ob.block_hash = NEW.source_block_hash
      AND ob.position_kind = NEW.source_position_kind
      AND ob.position_index = NEW.source_position_index
      AND ob.kind = 'directed_arrival';
    IF NOT FOUND THEN
        RETURN NEW;
    END IF;
    IF NOT directing_declared(o.network, o.token, o.directing, o.block_number) THEN
        RAISE EXCEPTION '% % refused: the directed arrival''s directing contract % is not declared on % for token % from block % or before; only declared directing code directs, and its arrival is neither minted nor returned (REQ-AS-11, REQ-IN-10, REQ-IN-11)',
            NEW.kind, NEW.bridge_reference, coalesce(o.directing, '(none)'), o.network,
            coalesce(o.token, '(none)'), o.block_number
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_guard_directing_declared
    BEFORE INSERT OR UPDATE OF source_network, source_block_number, source_block_hash,
        source_position_kind, source_position_index, kind, refund_of
    ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_directing_declared();

-- 0145's, and the arrival's directing contract declared (above).
CREATE OR REPLACE FUNCTION directed_return(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN observations o
          ON o.network = t.source_network AND o.block_number = t.source_block_number
         AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
         AND o.position_index = t.source_position_index
        WHERE t.id = p_transfer AND t.kind = 'refund' AND t.refund_of IS NULL
          AND o.kind = 'directed_arrival'
          AND directing_declared(o.network, o.token, o.detail ->> 'directing', o.block_number))
$$;

-- Grants. The core asks the question before it decides an arrival (it reads directing_code,
-- networks and observations already); the rest run as a trigger's or a definer's own.
REVOKE EXECUTE ON FUNCTION
    directing_declared(TEXT, TEXT, TEXT, BIGINT),
    transfers_guard_directing_declared()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION directing_declared(TEXT, TEXT, TEXT, BIGINT) TO bezent_core;

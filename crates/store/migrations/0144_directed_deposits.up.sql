-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-06 (amendment 0029, proposed; Pablo approved building F-018 on 2026-11-08): the
-- store's side of a directed deposit. The two kinds are 0143's; what binds them is here.
--
-- A directed arrival names its own destination and its own refund address in the transaction that
-- moved its value (A1), so no request chooses either (REQ-IN-10, REQ-RF-2; `d-refund-dest-swaps`).
-- bezent makes its deposit, and its return, itself. The store holds the whole rule, whoever writes:
--
--   * source_bindable() (0034, 0120) gets two purposes, `directed_deposit` and `directed_return`,
--     each true for a directed arrival and for nothing else. 'deposit' and 'refund' stay as they
--     were, so the ingress's own lookup (`intake::source`, which asks for those two) never finds a
--     directed arrival: a requester's deposit or refund request naming one is ERR-SourceNotFound and
--     binds nothing (AC-A6.2). `unpaired_direction` is bindable by nothing (AC-A6.6).
--   * transfers_source_bindable() (0120) asks for the directed purposes when the observation at the
--     coordinate is a directed arrival.
--   * transfers_guard_directed(): a transfer bound to a directed arrival is the core's deposit or the
--     core's return (the migration owner's drills and restores may carry the same row), and nothing
--     else; a deposit's destination is the observed recipient and its refund destination the observed
--     `refund`; a return's destination is the observed `refund`; a refund of the zero address, of the
--     vault or of the directing contract is refused (the contract's own checks, repeated here). It
--     holds for every writer, the owner included.
--   * intake_answers_guard_directed(): no request is ever answered with a transfer bound to a
--     directed arrival, whoever writes (A3).
--   * transfers_guard_writer() (0047, 0048): the core may create those two rows, and the ingress may
--     not bind a directed arrival.
--   * directed_deposit(): the core's one way to make the deposit. It creates it at `Source-Final`
--     from the final observation: the bridge reference `bz-d-…` derived from the coordinate as
--     `refs::deposit` derives it (deposit_reference()), the destination and the refund destination
--     from the observation's detail, the amount and the asset from the observation, and no
--     intake row. A second call for the coordinate does nothing (INV-2).
--
-- A directed deposit has no request, so its `request_reference` (not null, and distinct from the
-- bridge reference, REQ-ID-3) is a label made from its coordinate, `directed:<network>:<block>:
-- <hash>:<kind>:<index>`, as a withdrawal's is `redemption:…` (the coordinate stands in for the
-- request reference, amendment 0029, A2). Nothing looks a transfer up by it but a requester's
-- visibility, which is by `intake_answers`, and a directed deposit has none.
--
-- Readers of a deposit's answer. Audited (F-018 T-018-06): every one of them reaches a transfer
-- through `intake_answers` (`by-request`, `by-bridge`, `by-coordinate`, the log, the webhook
-- registration and the dispatcher's fan-out), so a transfer with no answer is a transfer none of
-- their requesters may read, and none fails on it. `unassociated_arrivals` (0014, 0120) still lists a
-- final directed arrival that no transfer binds yet, which is what an operator should see if the
-- core is stuck; the deposit consumes it.

-- ---------------------------------------------------------------------------------------------
-- The derivation of a bridge reference (services/ingress/src/refs.rs), byte for byte: SHA-256 over
-- the tag and each field, every one length-prefixed with a big-endian u64; the coordinate's digest is
-- the one field of a deposit's; the reference is the prefix and the first 20 bytes in hex.

CREATE FUNCTION reference_tagged(p_tag TEXT, p_fields BYTEA[]) RETURNS BYTEA
    LANGUAGE sql IMMUTABLE
    SET search_path = pg_catalog
    AS $$
    SELECT sha256(
        int8send(octet_length(convert_to(p_tag, 'UTF8'))::bigint) || convert_to(p_tag, 'UTF8')
        || COALESCE((SELECT string_agg(int8send(octet_length(f)::bigint) || f, ''::bytea ORDER BY ord)
                     FROM unnest(p_fields) WITH ORDINALITY AS u(f, ord)), ''::bytea))
$$;

CREATE FUNCTION deposit_reference(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS TEXT
    LANGUAGE sql IMMUTABLE
    SET search_path = pg_catalog, public
    AS $$
    SELECT 'bz-d-' || encode(substring(reference_tagged('bezent/deposit/v0', ARRAY[
        reference_tagged('bezent/coordinate/v0', ARRAY[
            convert_to(p_network, 'UTF8'),
            int8send(p_block),
            p_hash,
            convert_to(p_kind::text, 'UTF8'),
            substring(int8send(p_index) from 5 for 4)])
    ]) from 1 for 20), 'hex')
$$;

COMMENT ON FUNCTION deposit_reference(TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-IN-3, REQ-IN-10: the bridge reference of the deposit that binds the arrival at a coordinate, '
    'derived as services/ingress/src/refs.rs::deposit derives it. A test pins one vector in both.';

-- An address, compared as its network spells it: without case on an `evm` network, exactly otherwise.
CREATE FUNCTION address_matches(p_format TEXT, p_a TEXT, p_b TEXT) RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT p_a IS NOT NULL AND p_b IS NOT NULL
           AND (p_a = p_b OR (p_format = 'evm' AND lower(p_a) = lower(p_b)))
$$;

-- ---------------------------------------------------------------------------------------------
-- What a deposit or a refund of an unassociated arrival may name (0034, 0120): the two purposes
-- of a directed arrival, and no others.

CREATE OR REPLACE FUNCTION source_bindable(
    p_purpose TEXT, p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind,
    p_index BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT COALESCE((
        SELECT o.executed
           AND o.token IS NOT NULL AND o.amount IS NOT NULL AND o.recipient IS NOT NULL
           AND o.on_chain_reference IS NULL
           AND EXISTS (SELECT 1 FROM networks n WHERE n.network = o.network AND n.role = 'origin')
           AND EXISTS (SELECT 1 FROM reserves r
                       WHERE r.network = o.network AND r.address = o.recipient)
           AND CASE p_purpose
               WHEN 'deposit' THEN
                   o.kind = 'conversion_output'
                   OR (o.kind IN ('arrival', 'convertible_arrival')
                       AND NOT (o.sender IS NOT NULL
                                AND EXISTS (SELECT 1 FROM reserves r WHERE r.address = o.sender))
                       AND NOT (o.sender IS NOT NULL AND EXISTS (
                            SELECT 1 FROM routes v WHERE v.venue_addresses @> to_jsonb(o.sender))))
               WHEN 'refund' THEN
                   o.kind IN ('arrival', 'other_arrival', 'conversion_output')
               -- F-018 (amendment 0029, A2, A5): the two carve-outs, and no others. The ingress asks
               -- for 'deposit' and 'refund' only, so a requester never sees a directed arrival as
               -- bindable (ERR-SourceNotFound, REQ-IN-10); only transfers_source_bindable() asks for
               -- these two, for a row the core writes (transfers_guard_directed() then checks who).
               WHEN 'directed_deposit' THEN o.kind = 'directed_arrival'
               WHEN 'directed_return' THEN o.kind = 'directed_arrival'
               ELSE FALSE
           END
        FROM observations o
        WHERE o.network = p_network AND o.block_number = p_block AND o.block_hash = p_hash
          AND o.position_kind = p_kind AND o.position_index = p_index
    ), FALSE)
$$;
COMMENT ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-IN-10, REQ-CV-1, REQ-CV-2, INV-2: whether the '
    'observation at a coordinate is one a deposit (p_purpose = deposit: an arrival, a convertible '
    'arrival or a conversion output) or the refund of an unassociated arrival (p_purpose = refund) '
    'may name, and, for the core alone, a directed arrival''s deposit (directed_deposit) and its '
    'return (directed_return). Its finality, and whether it is consumed, are checked apart.';

-- The binding rule (0034, 0120), choosing the directed purposes for a directed arrival.
-- SECURITY DEFINER now (it ran as the inserting login, which 0034 and 0120 granted what it calls): the
-- core binds a directed arrival too, and the coordinate's lock and the conversion record are the
-- ingress's to call. It reads and refuses and writes nothing, so it needs no more than the owner's reads
-- to say what it says, and no role is granted more.
CREATE OR REPLACE FUNCTION transfers_source_bindable() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    purpose TEXT;
    binding BOOLEAN;
    moving BOOLEAN;
    finalising BOOLEAN;
    observed BOOLEAN;
    observed_kind TEXT;
BEGIN
    IF NEW.kind = 'deposit' THEN
        purpose := 'deposit';
    ELSIF NEW.kind = 'refund' AND NEW.refund_of IS NULL THEN
        purpose := 'refund';
    ELSE
        RETURN NEW;
    END IF;
    IF NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    binding := TG_OP = 'INSERT' OR OLD.source_network IS NULL;
    moving := TG_OP = 'UPDATE' AND OLD.source_network IS NOT NULL
              AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                      NEW.source_position_kind, NEW.source_position_index)
                  IS DISTINCT FROM
                  ROW(OLD.source_network, OLD.source_block_number, OLD.source_block_hash,
                      OLD.source_position_kind, OLD.source_position_index);
    finalising := NEW.source_final_network IS NOT NULL
                  AND (TG_OP = 'INSERT' OR OLD.source_final_network IS NULL);
    IF NOT (binding OR moving OR finalising) THEN
        RETURN NEW;
    END IF;
    IF binding OR moving THEN
        PERFORM coordinate_lock(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                                NEW.source_position_kind, NEW.source_position_index);
        IF coordinate_recorded_converted(NEW.source_network, NEW.source_block_number,
                                         NEW.source_block_hash, NEW.source_position_kind,
                                         NEW.source_position_index) THEN
            RAISE EXCEPTION '% refused: a deposit recorded the arrival at its source coordinate as the convertible arrival its conversion replaced; it is consumed (INV-2, REQ-CV-2)',
                NEW.kind
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    SELECT o.kind::text INTO observed_kind
    FROM observations o
    WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
      AND o.block_hash = NEW.source_block_hash
      AND o.position_kind = NEW.source_position_kind
      AND o.position_index = NEW.source_position_index;
    observed := FOUND;
    -- F-018 (amendment 0029, A2, A5): a directed arrival is bound by a deposit the core makes from
    -- it or by its return, under the purposes that name those two and no other; who may write
    -- either, and the destination and the refund destination they take, are
    -- transfers_guard_directed()'s.
    IF observed_kind = 'directed_arrival' THEN
        purpose := CASE purpose WHEN 'deposit' THEN 'directed_deposit' ELSE 'directed_return' END;
    END IF;
    IF finalising AND NOT observed
       AND EXISTS (SELECT 1 FROM networks WHERE network = NEW.source_network AND role = 'origin') THEN
        RAISE EXCEPTION '% refused: its source on % becomes final with no observation at its coordinate (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, INV-2)',
            NEW.kind, NEW.source_network
            USING ERRCODE = 'BZ001';
    END IF;
    IF moving AND NOT EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index
          AND o.kind = 'conversion_output') THEN
        RAISE EXCEPTION '% refused: a deposit''s source moves only to a conversion output (REQ-CV-2, INV-2)',
            NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    IF observed
       AND NOT source_bindable(purpose, NEW.source_network, NEW.source_block_number,
                               NEW.source_block_hash, NEW.source_position_kind,
                               NEW.source_position_index) THEN
        RAISE EXCEPTION '% refused: the observation at its source coordinate is not of a kind a % may name (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, INV-2)',
            NEW.kind, NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
-- The writer guard (0047, 0048): the core's two rows, and the ingress binding no directed arrival.
CREATE OR REPLACE FUNCTION transfers_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    o         RECORD;
    unproven  INTEGER[];
    issued    BIGINT;
    reviewed  transfer_state;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(writer, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;

    -- S8: the id is the one the store's sequence gave this session, never a writer's choice.
    BEGIN
        issued := currval(pg_get_serial_sequence('public.transfers', 'id'));
    EXCEPTION WHEN object_not_in_prerequisite_state THEN
        issued := NULL;
    END;
    IF issued IS DISTINCT FROM NEW.id THEN
        RAISE EXCEPTION 'transfer % refused for %: id % was not drawn from the store''s sequence; no writer chooses a transfer''s identifier, nor claims the one a withdrawal is about to take (REQ-RQ-4, REQ-RQ-7, REQ-ID-4)',
            NEW.bridge_reference, writer, NEW.id
            USING ERRCODE = 'BZ001';
    END IF;

    IF pg_has_role(writer, 'bezent_core', 'USAGE') THEN
        -- F-018 (amendment 0029, A2, A5): the core makes a directed arrival's deposit (at
        -- Source-Final, from its final observation) and its return (the refund of an
        -- unassociated arrival, in Received), from the arrival's own observation and nothing else.
        -- What they carry (the destination, the refund destination) is transfers_guard_directed()'s.
        IF NEW.refund_of IS NULL AND NEW.redeemer IS NULL AND NEW.on_chain_reference IS NULL
           AND NEW.destination_reference IS NULL
           AND ((NEW.kind = 'deposit' AND NEW.state = 'Source-Final'
                 AND NEW.bridge_reference ~ '^bz-d-')
                OR (NEW.kind = 'refund' AND NEW.state = 'Received'
                    AND NEW.bridge_reference ~ '^bz-u-'))
           AND EXISTS (SELECT 1 FROM observations ob
                       WHERE ob.network = NEW.source_network
                         AND ob.block_number = NEW.source_block_number
                         AND ob.block_hash = NEW.source_block_hash
                         AND ob.position_kind = NEW.source_position_kind
                         AND ob.position_index = NEW.source_position_index
                         AND ob.kind = 'directed_arrival') THEN
            RETURN NEW;
        END IF;
        IF NEW.state = 'Received' AND NEW.kind = 'withdrawal' AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NOT NULL AND NEW.on_chain_reference IS NOT NULL
           AND EXISTS (SELECT 1 FROM networks n
                       WHERE n.network = NEW.source_network AND n.role = 'issuing') THEN
            RETURN NEW;
        END IF;
        IF NEW.state = 'Received' AND NEW.kind = 'refund' AND NEW.refund_of IS NOT NULL THEN
            SELECT t.kind, t.state, t.redeemer, t.on_chain_reference, t.asset, t.amount,
                   t.source_network, t.source_block_number, t.source_block_hash,
                   t.source_position_kind, t.source_position_index
            INTO o
            FROM transfers t WHERE t.id = NEW.refund_of;
            IF FOUND AND o.kind = 'withdrawal'
               AND o.state IN ('Delivering', 'Delivered', 'Needs-Review')
               AND o.redeemer IS NOT NULL AND NEW.redeemer = o.redeemer
               AND NEW.destination = o.redeemer
               AND NEW.on_chain_reference IS NOT DISTINCT FROM o.on_chain_reference
               AND NEW.token IS NULL AND NEW.asset IS NOT DISTINCT FROM o.asset
               AND NEW.amount = o.amount
               AND NEW.source_network IS NOT DISTINCT FROM o.source_network
               AND NEW.source_block_number IS NOT DISTINCT FROM o.source_block_number
               AND NEW.source_block_hash IS NOT DISTINCT FROM o.source_block_hash
               AND NEW.source_position_kind IS NOT DISTINCT FROM o.source_position_kind
               AND NEW.source_position_index IS NOT DISTINCT FROM o.source_position_index THEN
                -- S7: a review the withdrawal entered while delivering, not before its first
                -- payout was ordered.
                IF o.state = 'Needs-Review' THEN
                    SELECT x.from_state INTO reviewed
                    FROM transitions x
                    WHERE x.transfer_id = NEW.refund_of AND x.to_state = 'Needs-Review'
                      AND x.from_state <> 'Needs-Review'
                    ORDER BY x.id DESC LIMIT 1;
                    IF reviewed IS NULL OR reviewed NOT IN ('Delivering', 'Delivered') THEN
                        RAISE EXCEPTION 'refund % refused: the withdrawal it replaces entered review from %, not while delivering; nothing was paid out to prove not executed (INV-8, REQ-RF-1)',
                            NEW.bridge_reference, coalesce(reviewed::text, 'nowhere')
                            USING ERRCODE = 'BZ001';
                    END IF;
                END IF;
                -- S7: at least one payout attempt was ordered ...
                IF NOT EXISTS (SELECT 1 FROM intents i
                               WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order') THEN
                    RAISE EXCEPTION 'refund % refused: the withdrawal it replaces never ordered a payout; a refund replaces a leg proven not executed (INV-8, REQ-RF-1)',
                        NEW.bridge_reference
                        USING ERRCODE = 'BZ001';
                END IF;
                -- ... and every attempt is proven not executed at finality.
                unproven := attempts_unproven(NEW.refund_of);
                IF cardinality(unproven) > 0 THEN
                    RAISE EXCEPTION 'refund % refused: attempt % of the withdrawal it replaces is not proven not executed at finality (INV-8, REQ-RF-1)',
                        NEW.bridge_reference, unproven[1]
                        USING ERRCODE = 'BZ001';
                END IF;
                RETURN NEW;
            END IF;
            RAISE EXCEPTION 'refund % refused: a refund of a transfer is of a withdrawal being delivered or in review, to its redeemer, with its on-chain reference, source, asset and amount (REQ-RF-3, REQ-RF-4, INV-8)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        RAISE EXCEPTION 'transfer % (%, %) refused for %: the core creates withdrawals from redemptions on the issuing network and refunds of withdrawals, in Received; a request''s transfer is the ingress''s (REQ-RQ-4, REQ-RF-3, REQ-TR-2)',
            NEW.bridge_reference, NEW.kind, NEW.state, writer
            USING ERRCODE = 'BZ001';
    END IF;

    IF pg_has_role(writer, 'bezent_ingress', 'USAGE') THEN
        IF NEW.state = 'Received'
           AND (NEW.kind = 'deposit' OR NEW.kind = 'refund')
           AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NULL AND NEW.on_chain_reference IS NULL
           AND NEW.destination_reference IS NULL
           AND NEW.bridge_reference ~ '^bz-[dur]-'
           AND (NEW.source_network IS NULL
                OR EXISTS (SELECT 1 FROM networks n
                           WHERE n.network = NEW.source_network AND n.role = 'origin'))
           -- F-018 (amendment 0029, A3): no request binds a directed arrival, nor what it left unpaired.
           AND NOT EXISTS (SELECT 1 FROM observations ob
                           WHERE ob.network = NEW.source_network
                             AND ob.block_number = NEW.source_block_number
                             AND ob.block_hash = NEW.source_block_hash
                             AND ob.position_kind = NEW.source_position_kind
                             AND ob.position_index = NEW.source_position_index
                             AND ob.kind IN ('directed_arrival', 'unpaired_direction')) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'transfer % (%, %) refused for %: the ingress creates only what a request makes, a deposit or the refund of an unassociated arrival, in Received, bound to an origin network, with none of the core''s keys; a withdrawal, a refund of one and anything at Source-Final or later rest on chain data and are the core''s (INV-6, INV-8, REQ-RQ-7, REQ-RF-3, REQ-RF-4)',
            NEW.bridge_reference, NEW.kind, NEW.state, writer
            USING ERRCODE = 'BZ001';
    END IF;

    RAISE EXCEPTION 'transfer % refused for %: only the ingress (a request''s) and the core (a withdrawal, a refund of one) create transfers (REQ-RQ-7, REQ-AR-3)',
        NEW.bridge_reference, writer
        USING ERRCODE = 'BZ001';
END
$$;
REVOKE EXECUTE ON FUNCTION transfers_guard_writer() FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- The destination guard, whoever writes (amendment 0029, A2, A3, A5): a transfer bound to a
-- directed arrival is the core's deposit or the core's return, with the observed destination and
-- refund. SECURITY DEFINER: it reads the observation's detail and the reserves, which the writer
-- may not. The writer is `session_user` (0047): a service login holds its role without SET.

CREATE FUNCTION transfers_guard_directed() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    o         RECORD;
    origin_fmt  TEXT;
    issuing_fmt TEXT;
    refund    TEXT;
    recipient TEXT;
BEGIN
    IF NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT ob.network, ob.kind::text AS kind, ob.detail INTO o
    FROM observations ob
    WHERE ob.network = NEW.source_network AND ob.block_number = NEW.source_block_number
      AND ob.block_hash = NEW.source_block_hash
      AND ob.position_kind = NEW.source_position_kind
      AND ob.position_index = NEW.source_position_index;
    IF NOT FOUND OR o.kind <> 'directed_arrival' THEN
        RETURN NEW;
    END IF;

    -- Who: the core (its deposit, its return) and the migration owner (drills and restores, NFR-7).
    -- A requester's transfer is the ingress's, and no request binds a directed arrival (REQ-IN-10).
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
    IF NOT (pg_has_role(writer, owner_oid, 'USAGE') OR pg_has_role(writer, 'bezent_core', 'USAGE')) THEN
        RAISE EXCEPTION '% refused for %: the source is a directed arrival, which no request binds; only bezent''s own deposit and return do (REQ-IN-10, REQ-RQ-3, REQ-RQ-6, ERR-SourceNotFound, INV-2)',
            NEW.kind, writer
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT n.address_format INTO origin_fmt FROM networks n WHERE n.network = o.network;
    refund := o.detail ->> 'refund';
    recipient := o.detail ->> 'recipient';

    -- The refund address the caller named (REQ-RF-2, d-refund-dest-swaps), and the three the
    -- directing contract refuses too: zero, the vault, the directing contract itself (REQ-IN-10).
    IF refund IS NULL OR refund = '' THEN
        RAISE EXCEPTION '% % refused: the directed arrival it is bound to records no refund address (REQ-IN-10, REQ-RF-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF lower(refund) ~ '^(0x)?0+$' THEN
        RAISE EXCEPTION '% % refused: the directed arrival''s refund address is the zero address (REQ-IN-10, REQ-RF-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM reserves r
               WHERE r.network = o.network AND address_matches(origin_fmt, r.address, refund)) THEN
        RAISE EXCEPTION '% % refused: the directed arrival''s refund address is the reserve itself (REQ-IN-10, REQ-RF-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF address_matches(origin_fmt, o.detail ->> 'directing', refund) THEN
        RAISE EXCEPTION '% % refused: the directed arrival''s refund address is its directing contract (REQ-IN-10, REQ-RF-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF NEW.kind = 'deposit' AND NEW.refund_of IS NULL THEN
        SELECT n.address_format INTO issuing_fmt FROM networks n
        WHERE n.network = NEW.destination_network AND n.role = 'issuing';
        IF NOT FOUND OR NOT address_matches(issuing_fmt, NEW.destination, recipient) THEN
            RAISE EXCEPTION 'deposit % refused: a directed deposit''s destination is the recipient its arrival recorded, on the issuing network, and nothing else (REQ-IN-10, INV-2)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.refund_destination_network IS DISTINCT FROM o.network
           OR NOT address_matches(origin_fmt, NEW.refund_destination, refund) THEN
            RAISE EXCEPTION 'deposit % refused: a directed deposit''s refund destination is the refund address its arrival recorded, on the arrival''s network, and nothing else (REQ-IN-10, REQ-RF-2, INV-2)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.bridge_reference IS DISTINCT FROM deposit_reference(
               NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index) THEN
            RAISE EXCEPTION 'deposit % refused: a directed deposit''s bridge reference is derived from its arrival''s coordinate (REQ-IN-10, REQ-IN-3)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.kind = 'refund' AND NEW.refund_of IS NULL THEN
        IF NEW.destination_network IS DISTINCT FROM o.network
           OR NOT address_matches(origin_fmt, NEW.destination, refund)
           OR num_nonnulls(NEW.refund_destination_network, NEW.refund_destination) > 0 THEN
            RAISE EXCEPTION 'refund % refused: the return of a directed arrival is paid on its network to the refund address its arrival recorded, and nowhere else (REQ-IN-11, REQ-RF-2, AC-A6.4)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSE
        RAISE EXCEPTION '% % refused: a directed arrival is bound only by the deposit bezent makes from it or by its return (REQ-IN-10, REQ-IN-11, INV-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_guard_directed
    BEFORE INSERT OR UPDATE OF source_network, source_block_number, source_block_hash,
        source_position_kind, source_position_index, kind, refund_of, destination_network,
        destination, refund_destination_network, refund_destination
    ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_directed();

-- No request is answered with a transfer bound to a directed arrival, whoever writes it, the owner
-- included (AC-A6.2): the answer is what makes a transfer a requester's.
CREATE FUNCTION intake_answers_guard_directed() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    bound TEXT;
BEGIN
    IF NEW.transfer_id IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT t.bridge_reference INTO bound
    FROM transfers t JOIN observations o
      ON o.network = t.source_network AND o.block_number = t.source_block_number
     AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
     AND o.position_index = t.source_position_index
    WHERE t.id = NEW.transfer_id AND o.kind IN ('directed_arrival', 'unpaired_direction');
    IF FOUND THEN
        RAISE EXCEPTION 'answer refused: transfer % is bound to a directed arrival, which no request binds or reaches (REQ-IN-10, REQ-RQ-3, REQ-RQ-6, ERR-SourceNotFound, INV-2)',
            bound
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intake_answers_guard_directed
    BEFORE INSERT ON intake_answers
    FOR EACH ROW EXECUTE FUNCTION intake_answers_guard_directed();

-- ---------------------------------------------------------------------------------------------
-- directed_deposit(): the core's deposit for a final directed arrival (REQ-IN-10, AC-A6.1).
--
-- Begins at Source-Final, as a requester-bound deposit does (REQ-IN-3), with the milestone of the
-- final head its observation was decided at. Returns the deposit's id: the new one, or, when the
-- coordinate already has its deposit, that one (INV-2); NULL when something else binds the
-- coordinate (its return). The triggers above and 0046, 0048, 0049, 0080 check what it writes, as
-- they check any writer's.

CREATE FUNCTION directed_deposit(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    VOLATILE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    o         RECORD;
    asset_of  TEXT;
    issuing   TEXT;
    final_at  TIMESTAMPTZ;
    reference TEXT;
    made      BIGINT;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
    IF NOT (pg_has_role(writer, owner_oid, 'USAGE') OR pg_has_role(writer, 'bezent_core', 'USAGE')) THEN
        RAISE EXCEPTION 'directed_deposit refused for %: only the core makes the deposit of a directed arrival (REQ-IN-10)',
            writer
            USING ERRCODE = 'BZ001';
    END IF;

    PERFORM coordinate_lock(p_network, p_block, p_hash, p_kind, p_index);

    SELECT ob.network, ob.token, ob.amount, ob.executed, ob.detail, x.outcome,
           x.final_block_number, x.final_block_hash
    INTO o
    FROM observations ob
    LEFT JOIN observation_outcomes x ON x.observation_id = ob.id
    WHERE ob.network = p_network AND ob.block_number = p_block AND ob.block_hash = p_hash
      AND ob.position_kind = p_kind AND ob.position_index = p_index
      AND ob.kind = 'directed_arrival';
    IF NOT FOUND OR NOT source_bindable('directed_deposit', p_network, p_block, p_hash, p_kind,
                                        p_index) THEN
        RAISE EXCEPTION 'directed_deposit refused: no executed directed arrival of a token at a reserve is observed at that coordinate (REQ-IN-10)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF o.outcome IS DISTINCT FROM 'final' THEN
        RAISE EXCEPTION 'directed_deposit refused: the directed arrival is %, not final; a deposit begins at Source-Final (REQ-IN-10, REQ-IN-2, INV-6)',
            coalesce(o.outcome, 'undecided')
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT k.asset INTO asset_of FROM admitted_tokens k WHERE k.network = o.network AND k.token = o.token;
    IF asset_of IS NULL THEN
        RAISE EXCEPTION 'directed_deposit refused: token % on % is not admitted for any bridged asset; its arrival is returned, not minted (REQ-IN-4, REQ-IN-11)',
            o.token, o.network
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT n.network INTO issuing FROM networks n WHERE n.role = 'issuing';
    SELECT h.block_time INTO final_at
    FROM final_heads h
    WHERE h.network = o.network AND h.block_number = o.final_block_number
      AND h.block_hash = o.final_block_hash;
    IF issuing IS NULL OR final_at IS NULL THEN
        RAISE EXCEPTION 'directed_deposit refused: no issuing network is declared, or the final head its observation was decided at is not recorded (REQ-IN-10, INV-6)'
            USING ERRCODE = 'BZ001';
    END IF;

    reference := deposit_reference(p_network, p_block, p_hash, p_kind, p_index);
    INSERT INTO transfers (
        bridge_reference, request_reference, kind, asset, amount,
        destination_network, destination, refund_destination_network, refund_destination,
        source_network, source_block_number, source_block_hash, source_position_kind,
        source_position_index, state,
        source_final_network, source_final_block_number, source_final_block_hash,
        source_final_block_time, source_final_seen_at)
    VALUES (
        reference,
        'directed:' || p_network || ':' || p_block || ':0x' || encode(p_hash, 'hex') || ':'
            || p_kind || ':' || p_index,
        'deposit', asset_of, o.amount,
        issuing, o.detail ->> 'recipient', p_network, o.detail ->> 'refund',
        p_network, p_block, p_hash, p_kind, p_index, 'Source-Final',
        o.network, o.final_block_number, o.final_block_hash, final_at, now())
    ON CONFLICT DO NOTHING
    RETURNING id INTO made;
    IF made IS NOT NULL THEN
        RETURN made;
    END IF;
    -- Nothing was made: the coordinate has its deposit already (INV-2), or something else binds it.
    SELECT t.id INTO made FROM transfers t WHERE t.bridge_reference = reference AND t.kind = 'deposit';
    RETURN made;
END
$$;

COMMENT ON FUNCTION directed_deposit(TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-IN-10, AC-A6.1, INV-2: the core''s deposit for the final directed arrival at a coordinate, at '
    'Source-Final, with the observed recipient as destination and the observed refund address as '
    'refund destination, and no request. A second call for the coordinate returns the same deposit.';

-- ---------------------------------------------------------------------------------------------
-- Grants. Nothing here is PUBLIC's. The core calls directed_deposit(); the rest run as a trigger's
-- or a definer's own.

REVOKE EXECUTE ON FUNCTION
    reference_tagged(TEXT, BYTEA[]),
    deposit_reference(TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    address_matches(TEXT, TEXT, TEXT),
    transfers_guard_directed(),
    intake_answers_guard_directed(),
    directed_deposit(TEXT, BIGINT, BYTEA, position_kind, BIGINT)
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION directed_deposit(TEXT, BIGINT, BYTEA, position_kind, BIGINT)
    TO bezent_core;

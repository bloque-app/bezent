-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): review M3 and L4 of PR #139, second fix round, the release of
-- an undeclared directed arrival and the statement's hold on the declarations, back to 0148's.
DROP FUNCTION operator_release_directed_arrival(TEXT, BIGINT, BYTEA, position_kind, BIGINT, TEXT);
DROP FUNCTION directing_code_hold();

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

CREATE OR REPLACE FUNCTION transfers_guard_directing_declared() RETURNS trigger
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
        -- F-018 (amendment 0029, A5, Open 2; T-018-09): the refund of a directed deposit that an
        -- approved, unrun resolution of its review moves into Refunding. It is paid as the return of
        -- its arrival is: its asset and amount, on the arrival's network, to the refund destination
        -- the deposit took from the arrival, with the return's bridge reference and payout reference
        -- (transfers_guard_directed checks the destination against the observation again).
        IF NEW.state = 'Received' AND NEW.kind = 'refund' AND NEW.refund_of IS NOT NULL
           AND EXISTS (SELECT 1 FROM transfers d WHERE d.id = NEW.refund_of AND d.kind = 'deposit') THEN
            SELECT d.asset, d.amount, d.refund_destination_network, d.refund_destination,
                   d.source_network, d.source_block_number, d.source_block_hash,
                   d.source_position_kind, d.source_position_index
            INTO o
            FROM transfers d WHERE d.id = NEW.refund_of;
            IF directed_review_refund_open(NEW.refund_of)
               AND EXISTS (SELECT 1 FROM manual_actions a
                           JOIN manual_approvals p ON p.action_id = a.id
                           JOIN reviews r ON r.transfer_id = a.transfer_id
                           WHERE a.transfer_id = NEW.refund_of AND a.kind = 'review-resolution'
                             AND a.to_state = 'Refunding' AND a.recorded_at >= r.entered_at
                             AND NOT EXISTS (SELECT 1 FROM transitions x
                                             WHERE x.manual_action_id = a.id))
               AND NEW.redeemer IS NULL AND NEW.destination_reference IS NULL
               AND NEW.token IS NULL AND NEW.asset IS NOT DISTINCT FROM o.asset
               AND NEW.amount = o.amount
               AND o.refund_destination IS NOT NULL
               AND NEW.destination_network IS NOT DISTINCT FROM o.refund_destination_network
               AND NEW.destination IS NOT DISTINCT FROM o.refund_destination
               AND num_nonnulls(NEW.refund_destination_network, NEW.refund_destination) = 0
               AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                       NEW.source_position_kind, NEW.source_position_index)
                   IS NOT DISTINCT FROM
                   ROW(o.source_network, o.source_block_number, o.source_block_hash,
                       o.source_position_kind, o.source_position_index)
               AND NEW.bridge_reference IS NOT DISTINCT FROM refund_reference(
                       o.source_network, o.source_block_number, o.source_block_hash,
                       o.source_position_kind, o.source_position_index)
               AND NEW.on_chain_reference IS NOT DISTINCT FROM directed_return_reference(
                       o.source_network, o.source_block_number, o.source_block_hash,
                       o.source_position_kind, o.source_position_index) THEN
                RETURN NEW;
            END IF;
            RAISE EXCEPTION 'refund % refused: the refund of a directed deposit is made only under an approved, unrun resolution of its review into Refunding, entered from Source-Final for CND-DestinationRefused with no mint attempt signed, of its asset and amount, to its refund destination, with the return''s references (REQ-IN-11, REQ-RF-2, REQ-RV-2, REQ-GV-5, INV-8)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
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
            -- Review H1 of PR #125 (REQ-RQ-3, REQ-RQ-6): a request names the coordinate of an
            -- arrival the store has observed (the ingress's `intake::source` reads it before it
            -- binds, and answers ERR-SourceNotFound otherwise), so a binding to a coordinate with no
            -- observation is no request's. Refused, so nothing observed there later (a directed
            -- arrival) is bound by it.
            IF NEW.source_network IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM observations ob
                               WHERE ob.network = NEW.source_network
                                 AND ob.block_number = NEW.source_block_number
                                 AND ob.block_hash = NEW.source_block_hash
                                 AND ob.position_kind = NEW.source_position_kind
                                 AND ob.position_index = NEW.source_position_index) THEN
                RAISE EXCEPTION 'transfer % (%) refused for %: its source names a coordinate on % with no observation; a request binds only an arrival the store has observed (REQ-RQ-3, REQ-RQ-6, ERR-SourceNotFound, INV-2)',
                    NEW.bridge_reference, NEW.kind, writer, NEW.source_network
                    USING ERRCODE = 'BZ001';
            END IF;
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

CREATE OR REPLACE FUNCTION transfers_guard_directed() RETURNS trigger
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

    -- No request answered with this transfer may rebind it onto a directed arrival: the answer
    -- guard below sees inserts only, so an update of an answered transfer is refused here (A3).
    IF TG_OP = 'UPDATE' AND EXISTS (SELECT 1 FROM intake_answers a WHERE a.transfer_id = NEW.id) THEN
        RAISE EXCEPTION '% % refused: a request already answers this transfer, and no request binds or reaches a directed arrival (REQ-IN-10, AC-A6.2, INV-2)',
            NEW.kind, NEW.bridge_reference
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
    -- A detail with no recipient or no directing contract is no directed arrival's: the directing
    -- check below compares nothing when the key is missing, so it is refused here, not passed.
    IF recipient IS NULL OR recipient = '' THEN
        RAISE EXCEPTION '% % refused: the directed arrival it is bound to records no recipient (REQ-IN-10, INV-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF o.detail ->> 'directing' IS NULL OR o.detail ->> 'directing' = '' THEN
        RAISE EXCEPTION '% % refused: the directed arrival it is bound to records no directing contract (REQ-IN-10, REQ-RF-2)',
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
        IF NEW.bridge_reference IS DISTINCT FROM refund_reference(
               NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index) THEN
            RAISE EXCEPTION 'refund % refused: the return of a directed arrival has the bridge reference derived from its arrival''s coordinate (REQ-IN-11, REQ-RQ-6)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.kind = 'refund' AND NEW.refund_of IS NOT NULL THEN
        -- F-018 (Open 2; T-018-09): the refund of the arrival's own directed deposit, which a review
        -- resolved into Refunding. Paid as its return is, to the observed refund address.
        IF NOT EXISTS (SELECT 1 FROM transfers d
                       WHERE d.id = NEW.refund_of AND d.kind = 'deposit'
                         AND ROW(d.source_network, d.source_block_number, d.source_block_hash,
                                 d.source_position_kind, d.source_position_index)
                             IS NOT DISTINCT FROM
                             ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                                 NEW.source_position_kind, NEW.source_position_index))
           OR NEW.destination_network IS DISTINCT FROM o.network
           OR NOT address_matches(origin_fmt, NEW.destination, refund)
           OR num_nonnulls(NEW.refund_destination_network, NEW.refund_destination) > 0 THEN
            RAISE EXCEPTION 'refund % refused: the refund of a directed deposit refunds the deposit bound to the same arrival, on its network, to the refund address the arrival recorded, and nowhere else (REQ-IN-11, REQ-RF-2, AC-A6.4)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.bridge_reference IS DISTINCT FROM refund_reference(
               NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index) THEN
            RAISE EXCEPTION 'refund % refused: the refund of a directed deposit has the bridge reference of its arrival''s return (REQ-IN-11, REQ-RQ-6, INV-2)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSE
        RAISE EXCEPTION '% % refused: a directed arrival is bound only by the deposit bezent makes from it, by its return, or by the refund of that deposit out of review (REQ-IN-10, REQ-IN-11, INV-2)',
            NEW.kind, NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

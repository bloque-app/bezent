-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, review M3 of PR #139, its second fix round (SPEC REQ-IN-6, REQ-GV-5, REQ-AS-11, REQ-IN-11):
-- the way out for a directed arrival whose directing contract is not declared, and the statement's
-- read of the declarations held still.
--
-- 0148 refuses the core's deposit and return of such an arrival, whoever writes, so it waits, final
-- and unbound. A declaration is append-only and keyed by (network, address), so one made from a
-- block later than the arrival's never covers it: an endpoint whose `[[directing]]` `from_block` is
-- lower than the declaration's observes arrivals in that gap that could never be minted or
-- returned. REQ-IN-6 says how an arrival that cannot become a deposit leaves: a refund, released by a
-- recorded decision with a second approver. Here it is, for such an arrival:
--
--   * operator_release_directed_arrival(coordinate, justification): one operator, as their own
--     login, releases a final, executed directed arrival whose directing contract is not declared,
--     and that nothing binds. In one transaction it makes the arrival's return, in Received, exactly
--     as the core makes a return (0144, 0145: the coordinate's `bz-u-` reference and `directed:`
--     label, the observed amount and token, paid on its network to the refund address the arrival
--     recorded and nowhere else), and records the release (`manual_actions`, kind
--     `release-unassociated`, 0014) on it. Returns the release's id. It refuses a declared one (the
--     core decides that one, a deposit or its return), one not final, and one already bound.
--   * A different operator login approves the release (`manual_approvals`, as for every release;
--     the store refuses the action's own login). Only then does the core accept the return,
--     Received -> Accepted, under that release (transitions_apply_lifecycle(), 0145: directed_return()
--     is false for an undeclared arrival, so the core's own acceptance with no approver is refused);
--     it is paid out as every refund of an unassociated arrival is, and the book posts it as one.
--   * The store's allowance for it, and nothing wider: transfers_guard_directing_declared() (0148)
--     lets the refund of an undeclared arrival be inserted in Received by an operator's session that
--     is neither the owner's nor the core's (only this function writes a transfer as one: an
--     operator holds no INSERT on transfers), and
--     still refuses its deposit to everyone and any other refund of it; transfers_guard_writer() and
--     transfers_guard_directed() (0145) admit that one row from an operator. The arrival is never
--     minted: nothing here makes a deposit.
--   * directing_code_hold() (review L4 of PR #139): the statement's publisher takes the declarations
--     in SHARE mode in the transaction it reads them in, so a declaration whose transaction began
--     before the evaluation's time and commits after the read is waited for, not missed; a statement
--     re-derived from the same evaluation lists what the first listed. Writes to `directing_code`
--     are the migration owner's and rare; they wait for the read, which is short.
--
-- Replaced, as their latest migration made them plus these lines: transfers_guard_writer() and
-- transfers_guard_directed() (0145), transfers_guard_directing_declared() (0148).

CREATE FUNCTION operator_release_directed_arrival(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT,
    p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    VOLATILE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    o        RECORD;
    asset_of TEXT;
    bound    TEXT;
    made     BIGINT;
    act      BIGINT;
BEGIN
    PERFORM operator_require();
    IF p_justification IS NULL OR btrim(p_justification) = '' THEN
        RAISE EXCEPTION 'release refused: a manual action states its justification (REQ-GV-5)'
            USING ERRCODE = 'BZ001';
    END IF;
    PERFORM coordinate_lock(p_network, p_block, p_hash, p_kind, p_index);

    SELECT ob.network, ob.token, ob.amount, ob.executed, ob.block_number, ob.detail, x.outcome
    INTO o
    FROM observations ob
    LEFT JOIN observation_outcomes x ON x.observation_id = ob.id
    WHERE ob.network = p_network AND ob.block_number = p_block AND ob.block_hash = p_hash
      AND ob.position_kind = p_kind AND ob.position_index = p_index
      AND ob.kind = 'directed_arrival';
    IF NOT FOUND OR NOT o.executed THEN
        RAISE EXCEPTION 'release refused: no executed directed arrival is observed at that coordinate (REQ-IN-6, REQ-IN-10)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF o.outcome IS DISTINCT FROM 'final' THEN
        RAISE EXCEPTION 'release refused: the directed arrival is %, not final; nothing is returned before it is final (REQ-IN-6, REQ-RF-1)',
            coalesce(o.outcome, 'undecided')
            USING ERRCODE = 'BZ001';
    END IF;
    IF directing_declared(o.network, o.token, o.detail ->> 'directing', o.block_number) THEN
        RAISE EXCEPTION 'release refused: the directed arrival''s directing contract % is declared on % for its token from its block; the core decides it, a deposit or its return, with no release (REQ-IN-10, REQ-IN-11)',
            o.detail ->> 'directing', o.network
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT t.bridge_reference INTO bound
    FROM transfers t
    WHERE t.refund_of IS NULL AND t.source_network = p_network
      AND t.source_block_number = p_block AND t.source_block_hash = p_hash
      AND t.source_position_kind = p_kind AND t.source_position_index = p_index
    ORDER BY t.id LIMIT 1;
    IF bound IS NOT NULL THEN
        RAISE EXCEPTION 'release refused: transfer % binds the directed arrival already; it is released once (INV-2, REQ-IN-6)',
            bound
            USING ERRCODE = 'BZ001';
    END IF;

    -- As the core's return (services/core/src/directed.rs, give_back()): the asset its token is
    -- admitted for, or, for a token admitted for none, the token itself (REQ-IN-6).
    SELECT k.asset INTO asset_of FROM admitted_tokens k
    WHERE k.network = o.network AND k.token = o.token;
    INSERT INTO transfers (
        bridge_reference, request_reference, kind, asset, token, amount,
        destination_network, destination,
        source_network, source_block_number, source_block_hash, source_position_kind,
        source_position_index, state)
    VALUES (
        refund_reference(p_network, p_block, p_hash, p_kind, p_index),
        'directed:' || p_network || ':' || p_block || ':0x' || encode(p_hash, 'hex') || ':'
            || p_kind || ':' || p_index || ':return',
        'refund', asset_of, CASE WHEN asset_of IS NULL THEN o.token END, o.amount,
        p_network, o.detail ->> 'refund',
        p_network, p_block, p_hash, p_kind, p_index, 'Received')
    RETURNING id INTO made;
    -- The release, as the operator's own (manual_actions_by_session() stamps the login and time).
    INSERT INTO manual_actions (kind, transfer_id, justification)
    VALUES ('release-unassociated', made, p_justification)
    RETURNING id INTO act;
    RETURN act;
END
$$;

COMMENT ON FUNCTION operator_release_directed_arrival(TEXT, BIGINT, BYTEA, position_kind, BIGINT, TEXT) IS
    'REQ-IN-6, REQ-GV-5, REQ-AS-11: an operator releases a final directed arrival whose directing '
    'contract is not declared, and that nothing binds, to the refund address it recorded: its return '
    'in Received and the release (release-unassociated) recorded as the caller''s; the return leaves '
    'Received only once another login approves the release. Returns the release''s id. Never mints.';

CREATE FUNCTION directing_code_hold() RETURNS void
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    LOCK TABLE directing_code IN SHARE MODE;
END
$$;

COMMENT ON FUNCTION directing_code_hold() IS
    'Review L4 of PR #139 (REQ-BK-5): holds directing_code in SHARE mode to the end of the calling '
    'transaction, so a declaration being written is waited for before the statement reads them.';

-- ---------------------------------------------------------------------------------------------
-- The replaced guards.

CREATE OR REPLACE FUNCTION transfers_guard_directing_declared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    o         RECORD;
    owner_oid OID;
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
        -- The release (0149): an operator's return of the arrival, in Received, which
        -- operator_release_directed_arrival() makes with its recorded release; it leaves Received
        -- only once another login approves that release (REQ-IN-6, REQ-GV-5). Never a deposit.
        SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
        IF TG_OP = 'INSERT' AND NEW.kind = 'refund' AND NEW.state = 'Received'
           AND pg_has_role(session_user, 'bezent_operator', 'MEMBER')
           AND NOT pg_has_role(session_user, owner_oid, 'USAGE')
           AND NOT pg_has_role(session_user, 'bezent_core', 'USAGE') THEN
            RETURN NEW;
        END IF;
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

    -- Review M3 of PR #139, its second fix round (REQ-IN-6, REQ-GV-5, REQ-AS-11): an operator's
    -- release of a directed arrival whose directing contract is not declared, made only through
    -- operator_release_directed_arrival() (an operator holds no INSERT on transfers): the arrival's
    -- return, in Received, with the coordinate's bz-u- reference and none of the core's keys. It
    -- leaves Received only by that release, approved by another login (transitions_apply_lifecycle;
    -- directed_return() is false for it). Where it pays is transfers_guard_directed()'s.
    IF pg_has_role(writer, 'bezent_operator', 'MEMBER') THEN
        IF NEW.kind = 'refund' AND NEW.state = 'Received' AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NULL AND NEW.on_chain_reference IS NULL
           AND NEW.destination_reference IS NULL
           AND NEW.bridge_reference ~ '^bz-u-'
           AND EXISTS (SELECT 1 FROM observations ob
                       WHERE ob.network = NEW.source_network
                         AND ob.block_number = NEW.source_block_number
                         AND ob.block_hash = NEW.source_block_hash
                         AND ob.position_kind = NEW.source_position_kind
                         AND ob.position_index = NEW.source_position_index
                         AND ob.kind = 'directed_arrival'
                         AND NOT directing_declared(ob.network, ob.token,
                                                    ob.detail ->> 'directing', ob.block_number)) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'transfer % (%, %) refused for %: an operator creates only the release of a directed arrival whose directing contract is not declared, its return in Received, through operator_release_directed_arrival() (REQ-IN-6, REQ-GV-5, REQ-AS-11)',
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

    -- Who: the core (its deposit, its return) and the migration owner (drills and restores, NFR-7),
    -- and an operator's release of an arrival of undeclared directing code, its return in Received
    -- (operator_release_directed_arrival(), 0149; transfers_guard_writer() holds the rest of it). A
    -- requester's transfer is the ingress's, and no request binds a directed arrival (REQ-IN-10).
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
    IF NOT (pg_has_role(writer, owner_oid, 'USAGE') OR pg_has_role(writer, 'bezent_core', 'USAGE')
            OR (pg_has_role(writer, 'bezent_operator', 'MEMBER') AND TG_OP = 'INSERT'
                AND NEW.kind = 'refund' AND NEW.refund_of IS NULL AND NEW.state = 'Received')) THEN
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

-- ---------------------------------------------------------------------------------------------
-- Grants. The operator releases; the core holds the declarations still for its statement. The
-- replaced functions keep theirs (a trigger's, run as the definer's own).

REVOKE EXECUTE ON FUNCTION
    operator_release_directed_arrival(TEXT, BIGINT, BYTEA, position_kind, BIGINT, TEXT),
    directing_code_hold()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION
    operator_release_directed_arrival(TEXT, BIGINT, BYTEA, position_kind, BIGINT, TEXT)
    TO bezent_operator;
GRANT EXECUTE ON FUNCTION directing_code_hold() TO bezent_core;

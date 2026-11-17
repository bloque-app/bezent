-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-07 to T-018-09 (amendment 0029, A2, A5, Open 1 to 3): what the core's directed intake,
-- its return of a never-mintable directed arrival and the one review edge of a directed deposit need
-- from the store. 0144 binds a directed arrival only to the core's deposit and the core's return; this
-- lets them move.
--
--   * CND-DepositWindow (Open 1, ruled: wait): a directed deposit over what its route's window has
--     left holds it at Source-Final, and leaves it, by itself, when it fits. A deposit takes its share
--     of the window when it is made, or, if it waited, when it leaves the condition; while it waits it
--     is a claim on the window ahead of anything that comes later, so first come first served by
--     coordinate holds for requesters' deposits too (Open 5: no separate share). route_usage_locked()
--     counts the window that way; directed_window_fits() is the core's question for one deposit.
--   * The return (Open 3, ruled: no second approver): the refund of an unassociated arrival that the
--     core makes for a directed arrival (0144 fixes its destination to the observed refund address and
--     its bridge reference to the coordinate's `bz-u-`) is accepted, Received -> Accepted, by the core
--     with no recorded release. Every other refund of an unassociated arrival still needs one
--     (REQ-IN-6, REQ-GV-5).
--   * The review edge (Open 2, ruled: yes): a directed deposit in Needs-Review, entered from
--     Source-Final holding CND-DestinationRefused, with no mint attempt of it ever signed, may be
--     resolved into Refunding by a recorded resolution with its second approver. Its attempt 1, ordered
--     and never signed, is cancelled under that resolution (0051, now also for a first attempt of
--     such a deposit), which closes it to minting; its refund is a transfer of its own (`refund_of` the
--     deposit), paid out as the return of its arrival is: the same bridge reference (`bz-u-`, so the
--     arrival is returned once whichever way), the same payout reference, the observed refund address.
--     No other transfer gains the edge: transfer_edge() is unchanged.
--
-- Replaced, each as its latest migration made it plus the F-018 lines: transitions_apply_lifecycle()
-- (0014), unsigned_attempt_cancellations_guard() (0051), transfers_guard_writer() and
-- transfers_guard_directed() (0144), intents_guard_order() (0056), route_usage_locked() (0011).

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff', 'CND-PayoutWindow',
        'CND-DepositWindow'
    ));

-- The deposits that ever waited for the window: few, and what the window's count looks up.
CREATE INDEX transitions_deposit_window ON transitions (transfer_id)
    WHERE condition = 'CND-DepositWindow';

ALTER TABLE unsigned_attempt_cancellations
    DROP CONSTRAINT unsigned_attempt_cancellations_attempt_check;
ALTER TABLE unsigned_attempt_cancellations
    ADD CONSTRAINT unsigned_attempt_cancellations_attempt_check CHECK (attempt >= 1);

-- ---------------------------------------------------------------------------------------------
-- Helpers.

-- The 32 bytes the payout of the return of the arrival at a coordinate writes on its network:
-- services/core/src/refs.rs::unassociated_refund_reference, byte for byte (a test pins it).
CREATE FUNCTION directed_return_reference(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS BYTEA
    LANGUAGE sql IMMUTABLE
    SET search_path = pg_catalog, public
    AS $$
    SELECT reference_tagged('bezent/unassociated-refund-reference/v0', ARRAY[
        coordinate_digest(p_network, p_block, p_hash, p_kind, p_index)])
$$;

-- Whether transfer `p_transfer` is the core's return of a directed arrival: the refund of an
-- unassociated arrival bound to one (0144 holds who wrote it and where it pays).
CREATE FUNCTION directed_return(p_transfer BIGINT) RETURNS BOOLEAN
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
          AND o.kind = 'directed_arrival')
$$;

-- Whether directed deposit `p_transfer` may be resolved from its review into Refunding (amendment
-- 0029, Open 2): a deposit bound to a directed arrival, in Needs-Review, entered from Source-Final
-- holding CND-DestinationRefused, with no signature and no signed bytes recorded for any intent of
-- it. The core asks it before it runs a resolution; the store asks it again of every row it writes.
CREATE FUNCTION directed_review_refund_open(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN observations o
          ON o.network = t.source_network AND o.block_number = t.source_block_number
         AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
         AND o.position_index = t.source_position_index
        JOIN LATERAL (
            SELECT x.from_state, x.condition FROM transitions x
            WHERE x.transfer_id = t.id AND x.to_state = 'Needs-Review'
              AND x.from_state <> 'Needs-Review'
            ORDER BY x.id DESC LIMIT 1) e ON TRUE
        WHERE t.id = p_transfer AND t.kind = 'deposit' AND t.refund_of IS NULL
          AND t.state = 'Needs-Review' AND o.kind = 'directed_arrival'
          AND e.from_state = 'Source-Final' AND e.condition = 'CND-DestinationRefused'
          AND NOT EXISTS (SELECT 1 FROM intents i JOIN signatures s ON s.intent_id = i.id
                          WHERE i.transfer_id = t.id)
          AND NOT EXISTS (SELECT 1 FROM intents i JOIN signed_transactions st ON st.intent_id = i.id
                          WHERE i.transfer_id = t.id))
$$;

COMMENT ON FUNCTION directed_review_refund_open(BIGINT) IS
    'REQ-IN-11, REQ-RV-2, INV-8 (amendment 0029, Open 2): whether a directed deposit, in review '
    'entered from Source-Final for CND-DestinationRefused with no mint attempt ever signed, may be '
    'resolved into Refunding (with a second approver, REQ-GV-5). No other transfer may.';

-- ---------------------------------------------------------------------------------------------
-- The lifecycle (0014), with the return accepted by the core and the one review edge.

CREATE OR REPLACE FUNCTION transitions_apply_lifecycle() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t               transfers%ROWTYPE;
    refund          transfers%ROWTYPE;
    act             manual_actions%ROWTYPE;
    inc             attempt_milestones%ROWTYPE;
    proof           RECORD;
    edge_from       transfer_state;
    entered_at      TIMESTAMPTZ;
    needs_coord     BOOLEAN;
    review_exit     BOOLEAN;
    attempt_edge    BOOLEAN;
    current_attempt INTEGER;
    directed_refund BOOLEAN := FALSE;
    owner_oid       OID;
BEGIN
    SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id FOR UPDATE;
    IF is_terminal_state(t.state) THEN
        RAISE EXCEPTION 'transfer % is % (terminal); a terminal state never changes (INV-5)',
            t.bridge_reference, t.state
            USING ERRCODE = 'BZ001';
    END IF;
    IF t.state IS DISTINCT FROM NEW.from_state THEN
        RAISE EXCEPTION 'transition of transfer % starts at % but the transfer is % (REQ-TR-2)',
            t.bridge_reference, NEW.from_state, t.state
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT COALESCE(max(attempt), 0) INTO current_attempt
    FROM transitions WHERE transfer_id = t.id AND id <> NEW.id AND attempt IS NOT NULL;

    -- The state the edge leaves: the transition's own, or, out of review, the state held before it.
    review_exit := NEW.from_state = 'Needs-Review' AND NEW.to_state <> 'Needs-Review';
    IF review_exit THEN
        SELECT from_state, seen_at INTO edge_from, entered_at FROM transitions
        WHERE transfer_id = t.id AND id <> NEW.id
          AND to_state = 'Needs-Review' AND from_state <> 'Needs-Review'
        ORDER BY id DESC LIMIT 1;
    ELSE
        edge_from := NEW.from_state;
    END IF;

    -- F-018 (amendment 0029, A5, Open 2): the one edge a directed deposit gains, out of a review it
    -- entered from Source-Final for CND-DestinationRefused, no mint attempt of it ever signed. No
    -- other transfer gains it: transfer_edge() still says Source-Final -> Refunding is no edge.
    directed_refund := review_exit AND edge_from = 'Source-Final' AND NEW.to_state = 'Refunding'
        AND directed_review_refund_open(t.id);

    -- A delivery attempt: from Source-Final (attempt 1), or a new attempt from Delivering or
    -- Delivered. Delivering -> Delivering without an attempt number is a condition only.
    attempt_edge := NEW.to_state = 'Delivering'
        AND (edge_from = 'Source-Final' OR edge_from = 'Delivered'
             OR (edge_from = 'Delivering' AND NEW.attempt IS NOT NULL));

    -- The edge (REQ-TR-6).
    IF review_exit THEN
        needs_coord := CASE WHEN directed_refund THEN TRUE
                            ELSE transfer_edge(edge_from, NEW.to_state) END;
        IF edge_from IS NULL OR needs_coord IS NULL
           OR (NEW.to_state = edge_from AND NOT attempt_edge) THEN
            RAISE EXCEPTION 'transfer %: Needs-Review -> % is not one ordinary edge from %, the state held before review (§5.4, REQ-RV-2, REQ-TR-6)',
                t.bridge_reference, NEW.to_state, edge_from
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.network IS NULL THEN
            RAISE EXCEPTION 'transfer %: a review ends only by evidence, the coordinate of the edge it runs (REQ-RV-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        SELECT * INTO act FROM manual_actions WHERE id = NEW.manual_action_id;
        IF NOT FOUND OR act.kind <> 'review-resolution' OR act.transfer_id <> t.id
           OR act.to_state <> NEW.to_state OR act.recorded_at < entered_at
           OR ROW(act.network, act.block_number, act.block_hash, act.block_time)
              IS DISTINCT FROM ROW(NEW.network, NEW.block_number, NEW.block_hash, NEW.block_time) THEN
            RAISE EXCEPTION 'transfer %: a review resolution runs a recorded manual action of this review, with its actor, justification and evidence (REQ-GV-5, REQ-RV-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        IF resolution_moves_value(NEW.to_state)
           AND NOT EXISTS (SELECT 1 FROM manual_approvals WHERE action_id = act.id) THEN
            RAISE EXCEPTION 'transfer %: a resolution that moves value needs a second approver (REQ-GV-5, REQ-RV-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSE
        needs_coord := CASE WHEN attempt_edge AND edge_from = 'Delivering' THEN TRUE
                            ELSE transfer_edge(NEW.from_state, NEW.to_state) END;
        IF needs_coord IS NULL THEN
            RAISE EXCEPTION 'transfer %: % -> % is not an edge of the state graph (§5.4, REQ-TR-2, REQ-TR-6)',
                t.bridge_reference, NEW.from_state, NEW.to_state
                USING ERRCODE = 'BZ001';
        END IF;
        IF needs_coord AND NEW.network IS NULL THEN
            RAISE EXCEPTION 'transfer %: % -> % rests on chain data and must record its coordinate (REQ-TR-2)',
                t.bridge_reference, NEW.from_state, NEW.to_state
                USING ERRCODE = 'BZ001';
        END IF;
        SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
        IF t.kind = 'refund' AND t.refund_of IS NULL
           AND NEW.from_state = 'Received' AND NEW.to_state = 'Accepted'
           AND NEW.manual_action_id IS NULL AND directed_return(t.id)
           AND (pg_has_role(session_user, 'bezent_core', 'USAGE')
                OR pg_has_role(session_user, owner_oid, 'USAGE')) THEN
            -- F-018 (amendment 0029, A5, Open 3; REQ-IN-11): the return of a directed arrival is
            -- bezent's own, to the refund address the arrival recorded (transfers_guard_directed),
            -- and needs no second approver. Only the core accepts it.
            NULL;
        ELSIF t.kind = 'refund' AND t.refund_of IS NULL
           AND NEW.from_state = 'Received' AND NEW.to_state = 'Accepted' THEN
            -- REQ-IN-6, REQ-RQ-6: an unassociated arrival is released only by a recorded decision
            -- with a second approver.
            SELECT * INTO act FROM manual_actions WHERE id = NEW.manual_action_id;
            IF NOT FOUND OR act.kind <> 'release-unassociated' OR act.transfer_id <> t.id
               OR NOT EXISTS (SELECT 1 FROM manual_approvals WHERE action_id = act.id) THEN
                RAISE EXCEPTION 'transfer %: an unassociated arrival is released only by a recorded decision with a second approver (REQ-IN-6, REQ-GV-5)',
                    t.bridge_reference
                    USING ERRCODE = 'BZ001';
            END IF;
        ELSIF NEW.manual_action_id IS NOT NULL THEN
            RAISE EXCEPTION 'transfer %: a manual action runs only a review resolution or a release (REQ-GV-5)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;

    -- REQ-EO-6: each attempt numbered, the next one, signed and recorded; attempt n+1 only once
    -- attempt n is proven not executed at finality, resting on that proof's final block.
    IF attempt_edge THEN
        IF NEW.attempt IS DISTINCT FROM current_attempt + 1 THEN
            RAISE EXCEPTION 'transfer %: the delivery attempt into Delivering is %, not % (REQ-EO-6)',
                t.bridge_reference, current_attempt + 1, NEW.attempt
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.attempt > 1 THEN
            SELECT * INTO proof FROM attempt_proof(t.id, current_attempt);
            IF proof.network IS NULL
               OR ROW(proof.network, proof.final_block_number, proof.final_block_hash)
                  IS DISTINCT FROM ROW(NEW.network, NEW.block_number, NEW.block_hash) THEN
                RAISE EXCEPTION 'transfer %: attempt % only after attempt % is proven not executed at finality, resting on the proof''s final block (REQ-EO-6, REQ-RF-5)',
                    t.bridge_reference, NEW.attempt, current_attempt
                    USING ERRCODE = 'BZ001';
            END IF;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM intents i
                       JOIN signatures s ON s.intent_id = i.id
                       JOIN signed_transactions st ON st.intent_id = i.id
                       WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt) THEN
            RAISE EXCEPTION 'transfer %: Delivering needs a signing intent of attempt % with its signature and its signed bytes recorded (§5.4, INV-9, REQ-EO-1)',
                t.bridge_reference, NEW.attempt
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.attempt IS NOT NULL THEN
        RAISE EXCEPTION 'transfer %: an attempt number is carried only by a delivery attempt (REQ-EO-6)',
            t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF NEW.to_state = 'Source-Final' AND NEW.from_state <> 'Source-Final' THEN
        -- §5.4: the source leg is final and consumed for it (INV-2): bound, on its own network.
        IF t.source_network IS NULL OR NEW.network <> t.source_network THEN
            RAISE EXCEPTION 'transfer %: Source-Final needs its source binding, and the coordinate on the source network (INV-2, REQ-TR-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        UPDATE transfers SET
            source_final_network = NEW.network, source_final_block_number = NEW.block_number,
            source_final_block_hash = NEW.block_hash, source_final_block_time = NEW.block_time,
            source_final_seen_at = NEW.seen_at
        WHERE id = t.id;
    ELSIF NEW.to_state = 'Delivered' AND NEW.from_state <> 'Delivered' THEN
        IF NEW.network <> t.destination_network THEN
            RAISE EXCEPTION 'transfer %: Delivered rests on the destination network % (REQ-TR-2)',
                t.bridge_reference, t.destination_network
                USING ERRCODE = 'BZ001';
        END IF;
        -- The inclusion of this attempt (F-001 NOTES, S5): the transfer's own milestone is written
        -- by Completed, from the attempt that completed it.
        INSERT INTO attempt_milestones (transfer_id, attempt, milestone, network, block_number,
                                        block_hash, block_time, seen_at, transition_id)
        VALUES (t.id, current_attempt, 'included', NEW.network, NEW.block_number, NEW.block_hash,
                NEW.block_time, NEW.seen_at, NEW.id);
    ELSIF NEW.to_state = 'Completed' THEN
        IF NEW.network <> t.destination_network THEN
            RAISE EXCEPTION 'transfer %: Completed rests on the destination network % (REQ-TR-2, REQ-TR-3)',
                t.bridge_reference, t.destination_network
                USING ERRCODE = 'BZ001';
        END IF;
        SELECT * INTO inc FROM attempt_milestones
        WHERE transfer_id = t.id AND attempt = current_attempt AND milestone = 'included';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'transfer %: Completed needs the inclusion of attempt % (REQ-TR-4)',
                t.bridge_reference, current_attempt
                USING ERRCODE = 'BZ001';
        END IF;
        INSERT INTO attempt_milestones (transfer_id, attempt, milestone, network, block_number,
                                        block_hash, block_time, seen_at, transition_id)
        VALUES (t.id, current_attempt, 'final', NEW.network, NEW.block_number, NEW.block_hash,
                NEW.block_time, NEW.seen_at, NEW.id);
        UPDATE transfers SET
            destination_included_network = inc.network,
            destination_included_block_number = inc.block_number,
            destination_included_block_hash = inc.block_hash,
            destination_included_block_time = inc.block_time,
            destination_included_seen_at = inc.seen_at,
            destination_final_network = NEW.network,
            destination_final_block_number = NEW.block_number,
            destination_final_block_hash = NEW.block_hash,
            destination_final_block_time = NEW.block_time,
            destination_final_seen_at = NEW.seen_at
        WHERE id = t.id;
    ELSIF NEW.to_state = 'Expired' THEN
        IF t.expires_at IS NULL OR NEW.block_time < t.expires_at THEN
            RAISE EXCEPTION 'transfer %: Expired needs an expiry and a coordinate at or past it (REQ-TR-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.to_state = 'Refunding' AND NEW.from_state <> 'Refunding' THEN
        -- §5.4: the destination leg is proven not executed at finality (REQ-RF-1, INV-8), resting
        -- on the proof's final block, and a refund transfer exists (REQ-RF-3). A directed deposit
        -- out of its review (F-018, Open 2) never had a mint attempt signed, so there is nothing to
        -- prove: it rests on the resolution's evidence, and its attempt set is closed to minting
        -- (amendment 0014's reading; transitions_refund_every_attempt_proven checks it).
        IF NOT directed_refund THEN
            SELECT * INTO proof FROM attempt_proof(t.id, current_attempt);
            IF proof.network IS NULL
               OR ROW(proof.network, proof.final_block_number, proof.final_block_hash)
                  IS DISTINCT FROM ROW(NEW.network, NEW.block_number, NEW.block_hash) THEN
                RAISE EXCEPTION 'transfer %: Refunding only once attempt % is proven not executed at finality, resting on the proof''s final block (REQ-RF-1, INV-8)',
                    t.bridge_reference, current_attempt
                    USING ERRCODE = 'BZ001';
            END IF;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM transfers WHERE refund_of = t.id) THEN
            RAISE EXCEPTION 'transfer %: Refunding needs its refund transfer (REQ-RF-3)', t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.to_state = 'Refunded' THEN
        SELECT * INTO refund FROM transfers WHERE refund_of = t.id;
        IF NOT FOUND OR refund.state <> 'Completed'
           OR ROW(NEW.network, NEW.block_number, NEW.block_hash)
              IS DISTINCT FROM
              ROW(refund.destination_final_network, refund.destination_final_block_number,
                  refund.destination_final_block_hash) THEN
            RAISE EXCEPTION 'transfer %: Refunded needs its refund Completed, resting on the refund''s final coordinate (REQ-TR-2, REQ-RF-3)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;

    UPDATE transfers SET state = NEW.to_state, condition = NEW.condition WHERE id = t.id;

    INSERT INTO event_log (kind, transfer_id, payload)
    VALUES ('transition', t.id, jsonb_build_object(
        'transition_id', NEW.id,
        'bridge_reference', t.bridge_reference,
        'from_state', NEW.from_state,
        'to_state', NEW.to_state,
        'condition', NEW.condition,
        'error', NEW.error,
        'attempt', NEW.attempt,
        'manual_action_id', NEW.manual_action_id,
        'network', NEW.network,
        'block_number', NEW.block_number,
        'block_hash', encode(NEW.block_hash, 'hex'),
        'block_time', NEW.block_time,
        'seen_at', NEW.seen_at));
    RETURN NULL;
END
$$;


CREATE OR REPLACE FUNCTION unsigned_attempt_cancellations_guard() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    t       transfers%ROWTYPE;
    act     manual_actions%ROWTYPE;
    reached INTEGER;
BEGIN
    -- The refund decision's lock: the signer's inserts take it FOR SHARE (0015), so a signature
    -- either lands before this row (and is seen below) or waits for it (and is then refused).
    SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id FOR UPDATE;
    IF NOT FOUND OR t.state <> 'Needs-Review' THEN
        RAISE EXCEPTION 'attempt % of transfer % is cancelled only in review, under an approved resolution into Refunding (INV-8, REQ-RF-1, REQ-RV-2)',
            NEW.attempt, NEW.transfer_id
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT * INTO act FROM manual_actions WHERE id = NEW.manual_action_id;
    IF NOT FOUND OR act.kind <> 'review-resolution' OR act.transfer_id <> t.id
       OR act.to_state IS DISTINCT FROM 'Refunding'
       OR NOT EXISTS (SELECT 1 FROM manual_approvals WHERE action_id = act.id)
       OR EXISTS (SELECT 1 FROM transitions x WHERE x.manual_action_id = act.id) THEN
        RAISE EXCEPTION 'attempt % of transfer % is cancelled only by an approved, unrun review resolution of this transfer into Refunding (REQ-GV-5, REQ-RV-2, INV-8)',
            NEW.attempt, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT COALESCE(max(attempt), 0) INTO reached FROM transitions WHERE transfer_id = t.id;
    IF NEW.attempt <> reached + 1
       OR NOT EXISTS (SELECT 1 FROM intents WHERE transfer_id = t.id AND attempt = NEW.attempt)
       -- F-018 (Open 2): a directed deposit's first attempt, which has no predecessor to prove.
       OR (NOT (NEW.attempt = 1 AND directed_review_refund_open(t.id))
           AND (SELECT p.network FROM attempt_proof(t.id, reached) p) IS NULL) THEN
        RAISE EXCEPTION 'attempt % of transfer % is not the next attempt after one proven not executed at finality, ordered and never entered (INV-8, REQ-EO-6)',
            NEW.attempt, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM signatures s JOIN intents i ON i.id = s.intent_id
               WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt)
       OR EXISTS (SELECT 1 FROM signed_transactions st JOIN intents i ON i.id = st.intent_id
                  WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt) THEN
        RAISE EXCEPTION 'attempt % of transfer % was signed, or has signed bytes recorded; it is proven not executed, never cancelled (INV-8, REQ-EO-6, REQ-NX-1)',
            NEW.attempt, t.bridge_reference
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


CREATE OR REPLACE FUNCTION intents_guard_order() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    t         transfers%ROWTYPE;
    src       RECORD;
    ordered   NUMERIC;
    expected  NUMERIC;
    issuing   TEXT;
    account   BYTEA;
    o         JSONB := NEW.payload -> 'order';
BEGIN
    IF NOT (NEW.payload ? 'order') OR NEW.payload ? 'message' OR NEW.payload ? 'prepared' THEN
        RETURN NEW;
    END IF;
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF jsonb_typeof(NEW.payload -> 'amount') IS DISTINCT FROM 'string'
       OR (NEW.payload ->> 'amount') !~ '^(0|[1-9][0-9]*)$' THEN
        RAISE EXCEPTION 'order refused: its amount is not a decimal string of minor units (INV-11)'
            USING ERRCODE = 'BZ001';
    END IF;
    ordered := (NEW.payload ->> 'amount')::numeric;

    IF NEW.purpose = 'cancel' THEN
        IF NEW.transfer_id IS NULL AND ordered = 0 AND jsonb_typeof(o -> 'cancel') = 'number' THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'cancel order refused: a cancel names its signed transaction, of no transfer, and moves nothing (REQ-NX-4)'
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION '% order refused: it names no transfer (INV-6, INV-7)', NEW.purpose
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT * INTO src FROM bezent_final_source(t.id);
    IF NOT FOUND THEN
        RAISE EXCEPTION '% order for % refused: its source observation is not final and executed at a recorded final head; nothing is minted before the arrival is final, nor paid out before the redemption is (INV-6, INV-7)',
            NEW.purpose, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT n.network INTO issuing FROM networks n WHERE n.role = 'issuing';

    IF NEW.purpose = 'mint' AND t.kind = 'deposit' THEN
        SELECT div(src.amount, power(10::numeric, k.precision - a.precision))
        INTO expected
        FROM assets a JOIN admitted_tokens k ON k.asset = a.asset
        WHERE a.asset = t.asset AND k.network = src.network AND k.token = src.token
          AND k.precision >= a.precision
          AND o ->> 'asset' IS NOT DISTINCT FROM a.issuing_asset;
        -- The recipient is the deposit's destination (issue #36): the account it decodes to.
        account := issuing_account(t.destination);
        IF expected IS NOT NULL AND NEW.network = issuing AND t.amount = src.amount
           AND ordered = expected
           AND account IS NOT NULL
           AND lower(o ->> 'to') IS NOT DISTINCT FROM '0x' || encode(account, 'hex') THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'mint order for % refused: a mint orders exactly the final arrival''s amount (%), scaled to the asset (%), of the asset''s issuing asset, to the account the deposit''s destination names, on the issuing network, for a deposit of that amount; it ordered % to % (INV-6, REQ-IN-4, REQ-AS-9)',
            t.bridge_reference, src.amount, expected, ordered, coalesce(o ->> 'to', 'no recipient')
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'refund-mint' AND t.kind = 'refund' AND t.refund_of IS NOT NULL THEN
        IF NEW.network = issuing AND src.network = issuing AND ordered = src.amount
           AND o ->> 'redeemer' IS NOT DISTINCT FROM t.redeemer
           AND o ->> 'asset' IS NOT DISTINCT FROM src.token THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'refund-mint order for % refused: it re-mints exactly the final redemption''s amount (%) to its redeemer, on the issuing network; it ordered % (INV-8, REQ-RF-4)',
            t.bridge_reference, src.amount, ordered
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'withdrawal' THEN
        SELECT src.amount * power(10::numeric, k.precision - a.precision)
        INTO expected
        FROM assets a JOIN admitted_tokens k ON k.asset = a.asset
        WHERE a.asset = t.asset AND k.network = t.destination_network
          AND a.issuing_asset = src.token AND k.precision >= a.precision;
        -- The asset is the transfer's, and a token, if the order names one, the asset's admitted
        -- token on the destination network (issue #36): the amount above is scaled for that one.
        IF expected IS NOT NULL AND src.network = issuing
           AND NEW.network = t.destination_network AND t.amount = expected AND ordered = expected
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination
           AND o ->> 'asset' IS NOT DISTINCT FROM t.asset
           AND (o ->> 'token' IS NULL
                OR EXISTS (SELECT 1 FROM admitted_tokens k
                           WHERE k.network = t.destination_network AND k.asset = t.asset
                             AND lower(k.token) = lower(o ->> 'token'))) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: a payout orders exactly the final redemption''s amount (%), scaled to the destination''s token (%), to the withdrawal''s destination on its network, of the withdrawal''s own asset (%) and the token admitted for it; it ordered % of asset %, token % (INV-7, REQ-OT-3, REQ-OT-4, REQ-AS-2, REQ-AS-3, REQ-TR-5)',
            t.bridge_reference, src.amount, expected, t.asset, ordered,
            coalesce(o ->> 'asset', 'none'), coalesce(o ->> 'token', 'none')
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'refund' AND t.refund_of IS NULL THEN
        -- Paid out in the arrival's own minor units: in the arrival's own token (issue #36). The
        -- refund names the arrival's asset (and the token admitted for it is the arrival's), or,
        -- its token not being admitted, that token and no asset (REQ-IN-6; 0048 holds the refund's
        -- own asset and token to the observation).
        IF NEW.network = src.network AND t.destination_network = src.network
           AND t.amount = src.amount AND ordered = src.amount
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination
           AND o ->> 'asset' IS NOT DISTINCT FROM t.asset
           AND (t.asset IS NOT NULL
                AND (o ->> 'token' IS NULL OR lower(o ->> 'token') = lower(src.token))
                OR t.asset IS NULL
                AND lower(o ->> 'token') = lower(t.token) AND lower(t.token) = lower(src.token)) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: the refund of an unassociated arrival pays out exactly the final arrival''s amount (%) on its network, to the refund''s destination, in the arrival''s own token (asset %, token %); it ordered % of asset %, token % (REQ-IN-6, REQ-RQ-7, REQ-AS-3)',
            t.bridge_reference, src.amount, coalesce(t.asset, 'none'), src.token, ordered,
            coalesce(o ->> 'asset', 'none'), coalesce(o ->> 'token', 'none')
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'refund' AND t.refund_of IS NOT NULL
          AND EXISTS (SELECT 1 FROM transfers d WHERE d.id = t.refund_of AND d.kind = 'deposit') THEN
        -- F-018 (Open 2; T-018-09): the refund of a directed deposit out of review is paid as the
        -- return of its arrival is: exactly the final arrival's amount, on its network, to the
        -- refund's destination, in the arrival's own token.
        IF NEW.network = src.network AND t.destination_network = src.network
           AND t.amount = src.amount AND ordered = src.amount
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination
           AND o ->> 'asset' IS NOT DISTINCT FROM t.asset
           AND (o ->> 'token' IS NULL OR lower(o ->> 'token') = lower(src.token)) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: the refund of a directed deposit pays out exactly the final arrival''s amount (%) on its network, to the refund''s destination, in the arrival''s own token; it ordered % (REQ-IN-11, REQ-RF-2, INV-8)',
            t.bridge_reference, src.amount, ordered
            USING ERRCODE = 'BZ001';
    END IF;
    RAISE EXCEPTION '% order for % (%) refused: no such delivery (INV-6, INV-7)',
        NEW.purpose, t.bridge_reference, t.kind
        USING ERRCODE = 'BZ001';
END
$$;


-- ---------------------------------------------------------------------------------------------
-- The route's window (REQ-LM-1; amendment 0029, Open 1 and 5).
--
-- What a route's trailing window has used: every transfer of the route that moved or may move
-- (not Refused, Expired or Withdrawn) and took its share within the window, plus every directed
-- deposit that waits for its share (CND-DepositWindow), which has a claim on it ahead of whatever
-- comes later. A transfer takes its share when it is made, or, if it waited, when it left the
-- condition. As 0011's otherwise: the same lock, the same in-flight count.

CREATE OR REPLACE FUNCTION route_usage_locked(
    p_asset TEXT, p_network TEXT, p_direction TEXT, p_window_seconds BIGINT,
    OUT used TEXT, OUT in_flight BIGINT
)
    LANGUAGE plpgsql
    VOLATILE
    SET search_path = public, pg_temp
    AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended(
        'bezent/route/' || p_asset || '/' || p_network || '/' || p_direction, 0));
    SELECT COALESCE(sum(t.amount), 0)::text INTO used
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn')
      AND CASE
          WHEN t.condition IS NOT DISTINCT FROM 'CND-DepositWindow' THEN TRUE
          WHEN t.id IN (SELECT w.transfer_id FROM transitions w
                        WHERE w.condition = 'CND-DepositWindow') THEN
              (SELECT min(x.seen_at) FROM transitions x
               WHERE x.transfer_id = t.id
                 AND x.condition IS DISTINCT FROM 'CND-DepositWindow'
                 AND x.id > (SELECT min(w.id) FROM transitions w
                             WHERE w.transfer_id = t.id AND w.condition = 'CND-DepositWindow'))
              > now() - make_interval(secs => p_window_seconds)
          ELSE t.created_at > now() - make_interval(secs => p_window_seconds)
      END;
    SELECT count(*) INTO in_flight
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND NOT is_terminal_state(t.state);
END
$$;

-- Whether deposit `p_transfer` fits in what its route's window has left (REQ-LM-1, amendment 0029
-- Open 1): what the other deposits of the route took within the window, plus what every deposit
-- waiting ahead of it by coordinate claims (first come first served: block, then position), plus its
-- own amount, is within the window's limit. FALSE for a transfer that is not a deposit with a
-- declared route. Takes the route's lock (route_usage_locked), held until the transaction ends, so
-- that the decision and the transition it leads to are serial with every other reading of the
-- window, the ingress's included. Runs with the caller's privileges; the core calls it.
CREATE FUNCTION directed_window_fits(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE plpgsql
    VOLATILE
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t      transfers%ROWTYPE;
    r      routes%ROWTYPE;
    taken  NUMERIC;
    ahead  NUMERIC;
BEGIN
    SELECT * INTO t FROM transfers WHERE id = p_transfer;
    IF NOT FOUND OR t.kind <> 'deposit' OR t.source_network IS NULL THEN
        RETURN FALSE;
    END IF;
    SELECT * INTO r FROM routes
    WHERE asset = t.asset AND network = t.source_network AND direction = 'deposit';
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    PERFORM route_usage_locked(t.asset, t.source_network, 'deposit', r.window_seconds);
    SELECT COALESCE(sum(d.amount), 0) INTO taken
    FROM transfers d
    WHERE d.asset = t.asset AND d.kind = 'deposit' AND d.source_network = t.source_network
      AND d.id <> t.id
      AND d.state NOT IN ('Refused', 'Expired', 'Withdrawn')
      AND d.condition IS DISTINCT FROM 'CND-DepositWindow'
      AND CASE
          WHEN d.id IN (SELECT w.transfer_id FROM transitions w
                        WHERE w.condition = 'CND-DepositWindow') THEN
              (SELECT min(x.seen_at) FROM transitions x
               WHERE x.transfer_id = d.id
                 AND x.condition IS DISTINCT FROM 'CND-DepositWindow'
                 AND x.id > (SELECT min(w.id) FROM transitions w
                             WHERE w.transfer_id = d.id AND w.condition = 'CND-DepositWindow'))
              > now() - make_interval(secs => r.window_seconds)
          ELSE d.created_at > now() - make_interval(secs => r.window_seconds)
      END;
    SELECT COALESCE(sum(d.amount), 0) INTO ahead
    FROM transfers d
    WHERE d.asset = t.asset AND d.kind = 'deposit' AND d.source_network = t.source_network
      AND d.id <> t.id
      AND d.condition = 'CND-DepositWindow'
      AND ROW(d.source_block_number, d.source_position_index, d.id)
          < ROW(t.source_block_number, t.source_position_index, t.id);
    RETURN taken + ahead + t.amount <= r.window_limit;
END
$$;

COMMENT ON FUNCTION directed_window_fits(BIGINT) IS
    'REQ-LM-1, REQ-IN-11 (amendment 0029, Open 1): whether a directed deposit fits in what its '
    'route''s window has left, behind every deposit waiting ahead of it by coordinate; takes the '
    'route''s lock until the transaction ends. The core holds CND-DepositWindow while it does not.';

-- ---------------------------------------------------------------------------------------------
-- Grants. Nothing new is PUBLIC's but what route_usage_locked() already was. The core asks the two
-- questions; the rest run as a trigger's or a definer's own.

REVOKE EXECUTE ON FUNCTION
    directed_return_reference(TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    directed_return(BIGINT),
    directed_review_refund_open(BIGINT),
    directed_window_fits(BIGINT)
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION directed_review_refund_open(BIGINT), directed_window_fits(BIGINT)
    TO bezent_core;

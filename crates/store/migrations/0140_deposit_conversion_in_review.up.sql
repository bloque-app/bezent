-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #10 (F-010), amendment 0018 Open 6, ruled by @pandres95 on the decision form (`d-cv-open-choices`,
-- "As recommended", 2026-11-06): a conversion output may be named for a deposit in `Needs-Review`, as
-- long as no refund of the deposit is recorded or started.
--
-- Nothing here moves the deposit out of review. The store records the conversion and moves the
-- deposit's source and amount to the output, as it does at Source-Final, and the deposit stays in
-- review holding `CND-AwaitingConversion`. The review then ends as every review does (REQ-RV-2): a
-- recorded resolution into `Delivering` (one ordinary edge from `Source-Final`, the state held before
-- review; `transitions_apply`, 0014, unchanged) with a second approver, which orders the mint at the
-- output's amount (REQ-GV-5, REQ-CV-5). No transition is added.
--
--   * review_entered_from(): the state a transfer held before its last review. deposit_refund_recorded():
--     whether a refund of a transfer is recorded or started.
--   * deposit_conversion_record() (0120): also takes a deposit in `Needs-Review` that was sent there from
--     `Source-Final` holding `CND-AwaitingConversion`, with its source still the convertible arrival,
--     and refuses one with a refund recorded (a transfer whose `refund_of` is the deposit) or started
--     (a review resolution into `Refunding` whose evidence was not refused): deposit_refund_recorded().
--   * transfers_guard_update() (0120): the one move of the source and the amount is also allowed on a
--     deposit in review, with the same record.
--
-- The route's limits on the output (REQ-LM-1) are the ingress's, like every route check (Open 3, ruled
-- the same day); the function takes none. Grants: unchanged (CREATE OR REPLACE keeps them).

CREATE FUNCTION review_entered_from(p_transfer BIGINT)
    RETURNS transfer_state
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT x.from_state FROM transitions x
    WHERE x.transfer_id = p_transfer AND x.to_state = 'Needs-Review' AND x.from_state <> 'Needs-Review'
    ORDER BY x.id DESC LIMIT 1
$$;

COMMENT ON FUNCTION review_entered_from(BIGINT) IS
    'REQ-RV-2: the state the transfer held before its last review (the edge a review resolution runs).';

-- Whether a refund of the deposit is recorded (a transfer whose refund_of is the deposit) or started (a
-- review resolution into Refunding whose evidence the core did not refuse). The ingress reads it to
-- refuse by name, and deposit_conversion_record() checks it under the deposit's lock.
CREATE FUNCTION deposit_refund_recorded(p_transfer BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (SELECT 1 FROM transfers r WHERE r.refund_of = p_transfer)
        OR EXISTS (SELECT 1 FROM manual_actions a
                   WHERE a.transfer_id = p_transfer AND a.kind = 'review-resolution'
                     AND a.to_state = 'Refunding'
                     AND NOT EXISTS (SELECT 1 FROM event_log e
                                     WHERE e.kind = 'review-evidence-refused'
                                       AND e.payload ->> 'manual_action_id' = a.id::text))
$$;

COMMENT ON FUNCTION deposit_refund_recorded(BIGINT) IS
    'REQ-RF-1, REQ-RV-2 (amendment 0018, Open 6): whether a refund of the transfer is recorded or started; '
    'a conversion output is not named for a deposit after it.';

REVOKE EXECUTE ON FUNCTION review_entered_from(BIGINT), deposit_refund_recorded(BIGINT) FROM PUBLIC;
-- The write-once guard of 0003 runs as the updating login: the ingress (the move) and the core (the
-- reference it writes in review) call review_entered_from(); only the ingress asks about a refund.
GRANT EXECUTE ON FUNCTION review_entered_from(BIGINT) TO bezent_ingress, bezent_core;
GRANT EXECUTE ON FUNCTION deposit_refund_recorded(BIGINT) TO bezent_ingress;

CREATE OR REPLACE FUNCTION transfers_guard_update() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    -- REQ-CV-2, INV-2 (amendment 0018, A1b; Open 6 ruled, `d-cv-open-choices`): a deposit at
    -- Source-Final holding CND-AwaitingConversion, or in review entered from there, moves its
    -- source and its amount once, to the output and the amount a record of deposit_conversions
    -- names for it (only deposit_conversion_record() writes one).
    converting BOOLEAN := COALESCE(OLD.kind = 'deposit'
        AND OLD.state IN ('Source-Final', 'Needs-Review')
        AND NEW.state = OLD.state
        AND (OLD.state = 'Source-Final' OR review_entered_from(OLD.id) = 'Source-Final')
        AND OLD.condition IS NOT DISTINCT FROM 'CND-AwaitingConversion'
        AND OLD.source_network IS NOT NULL
        AND EXISTS (
            SELECT 1 FROM deposit_conversions c
            WHERE c.transfer_id = OLD.id
              AND c.arrival_network = OLD.source_network
              AND c.arrival_block_number = OLD.source_block_number
              AND c.arrival_block_hash = OLD.source_block_hash
              AND c.arrival_position_kind = OLD.source_position_kind
              AND c.arrival_position_index = OLD.source_position_index
              AND c.output_network = NEW.source_network
              AND c.output_block_number = NEW.source_block_number
              AND c.output_block_hash = NEW.source_block_hash
              AND c.output_position_kind = NEW.source_position_kind
              AND c.output_position_index = NEW.source_position_index
              AND c.output_amount = NEW.amount), FALSE);
BEGIN
    IF is_terminal_state(OLD.state) THEN
        RAISE EXCEPTION 'transfer % is % (terminal); a terminal state never changes (INV-5)',
            OLD.bridge_reference, OLD.state
            USING ERRCODE = 'BZ001';
    END IF;

    IF ROW(NEW.id, NEW.bridge_reference, NEW.request_reference, NEW.kind, NEW.asset,
           NEW.destination_network, NEW.destination, NEW.refund_of, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.bridge_reference, OLD.request_reference, OLD.kind, OLD.asset,
           OLD.destination_network, OLD.destination, OLD.refund_of, OLD.created_at)
       OR (NEW.amount IS DISTINCT FROM OLD.amount AND NOT converting) THEN
        RAISE EXCEPTION 'transfer %: what it was asked never changes (REQ-ST-7)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF num_nonnulls(OLD.refund_destination_network, OLD.refund_destination) > 0
       AND ROW(NEW.refund_destination_network, NEW.refund_destination)
           IS DISTINCT FROM ROW(OLD.refund_destination_network, OLD.refund_destination) THEN
        RAISE EXCEPTION 'transfer %: the refund destination is write-once (REQ-RF-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.expires_at IS NOT NULL AND NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'transfer %: the expiry is write-once (REQ-RQ-4)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.source_network IS NOT NULL
       AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index)
           IS DISTINCT FROM
           ROW(OLD.source_network, OLD.source_block_number, OLD.source_block_hash,
               OLD.source_position_kind, OLD.source_position_index)
       AND NOT converting THEN
        RAISE EXCEPTION 'transfer %: the source binding is write-once, but for a deposit''s one move from its convertible arrival to its conversion''s output (INV-2, REQ-CV-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.on_chain_reference IS NOT NULL
       AND NEW.on_chain_reference IS DISTINCT FROM OLD.on_chain_reference THEN
        RAISE EXCEPTION 'transfer %: the on-chain reference is write-once (INV-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    -- The state, the condition and the milestones change only inside transitions_apply(), the
    -- trigger of an appended transition (nesting depth 2). The service role holds no UPDATE
    -- privilege on these columns either (0009_roles), so it cannot reach this point.
    IF ROW(NEW.state, NEW.condition,
           NEW.source_final_network, NEW.source_final_block_number, NEW.source_final_block_hash,
           NEW.source_final_block_time, NEW.source_final_seen_at,
           NEW.destination_included_network, NEW.destination_included_block_number,
           NEW.destination_included_block_hash, NEW.destination_included_block_time,
           NEW.destination_included_seen_at,
           NEW.destination_final_network, NEW.destination_final_block_number,
           NEW.destination_final_block_hash, NEW.destination_final_block_time,
           NEW.destination_final_seen_at)
       IS DISTINCT FROM
       ROW(OLD.state, OLD.condition,
           OLD.source_final_network, OLD.source_final_block_number, OLD.source_final_block_hash,
           OLD.source_final_block_time, OLD.source_final_seen_at,
           OLD.destination_included_network, OLD.destination_included_block_number,
           OLD.destination_included_block_hash, OLD.destination_included_block_time,
           OLD.destination_included_seen_at,
           OLD.destination_final_network, OLD.destination_final_block_number,
           OLD.destination_final_block_hash, OLD.destination_final_block_time,
           OLD.destination_final_seen_at)
       AND pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'transfer %: the state, the condition and the milestones change only by appending a transition (REQ-TR-2)',
            OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    -- A milestone, once written, never changes (REQ-TR-2: no recorded time is overwritten).
    IF (OLD.source_final_network IS NOT NULL
        AND ROW(NEW.source_final_network, NEW.source_final_block_number, NEW.source_final_block_hash,
                NEW.source_final_block_time, NEW.source_final_seen_at)
            IS DISTINCT FROM
            ROW(OLD.source_final_network, OLD.source_final_block_number, OLD.source_final_block_hash,
                OLD.source_final_block_time, OLD.source_final_seen_at))
       OR (OLD.destination_included_network IS NOT NULL
        AND ROW(NEW.destination_included_network, NEW.destination_included_block_number,
                NEW.destination_included_block_hash, NEW.destination_included_block_time,
                NEW.destination_included_seen_at)
            IS DISTINCT FROM
            ROW(OLD.destination_included_network, OLD.destination_included_block_number,
                OLD.destination_included_block_hash, OLD.destination_included_block_time,
                OLD.destination_included_seen_at))
       OR (OLD.destination_final_network IS NOT NULL
        AND ROW(NEW.destination_final_network, NEW.destination_final_block_number,
                NEW.destination_final_block_hash, NEW.destination_final_block_time,
                NEW.destination_final_seen_at)
            IS DISTINCT FROM
            ROW(OLD.destination_final_network, OLD.destination_final_block_number,
                OLD.destination_final_block_hash, OLD.destination_final_block_time,
                OLD.destination_final_seen_at)) THEN
        RAISE EXCEPTION 'transfer %: a milestone is write-once (REQ-TR-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION deposit_conversion_record(
    p_transfer BIGINT, p_intake BIGINT,
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS VOID
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    t         transfers%ROWTYPE;
    arrival   RECORD;
    output    RECORD;
    asker     TEXT;
    depositor TEXT;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'transfers'::regclass;
    IF NOT (pg_has_role(writer, owner_oid, 'USAGE') OR pg_has_role(writer, 'bezent_ingress', 'USAGE')) THEN
        RAISE EXCEPTION 'conversion refused for %: only a request names a deposit''s conversion output (REQ-RQ-4, REQ-RQ-7)',
            writer
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT * INTO t FROM transfers WHERE id = p_transfer FOR UPDATE;
    IF NOT FOUND OR t.kind <> 'deposit' OR t.refund_of IS NOT NULL THEN
        RAISE EXCEPTION 'conversion refused: transfer % is not a deposit (REQ-RQ-4, REQ-CV-2)', p_transfer
            USING ERRCODE = 'BZ001';
    END IF;
    -- Awaiting at Source-Final, or in review entered from there for the deadline, still holding
    -- the condition (Open 6, `d-cv-open-choices` (a)): a review is no bar to naming the output, a
    -- refund is.
    IF t.state = 'Needs-Review' THEN
        IF review_entered_from(t.id) IS DISTINCT FROM 'Source-Final'
           OR t.condition IS DISTINCT FROM 'CND-AwaitingConversion' THEN
            RAISE EXCEPTION 'conversion refused: deposit % is in review, but not from Source-Final holding CND-AwaitingConversion; it names its output only while it awaits it (REQ-CV-2, REQ-CV-6, REQ-RV-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        IF deposit_refund_recorded(t.id) THEN
            RAISE EXCEPTION 'conversion refused: a refund of deposit % is recorded or started; its output is not named after it (REQ-CV-2, REQ-RF-1, REQ-RV-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF t.state <> 'Source-Final' OR t.condition IS DISTINCT FROM 'CND-AwaitingConversion' THEN
        RAISE EXCEPTION 'conversion refused: deposit % is % holding %, not Source-Final holding CND-AwaitingConversion, nor in review from it; it names its output only while it awaits it (REQ-CV-2, REQ-CV-6, REQ-RV-2)',
            t.bridge_reference, t.state, coalesce(t.condition::text, 'no condition')
            USING ERRCODE = 'BZ001';
    END IF;

    -- REQ-RQ-4: by the deposit's requester only, in a request of its own not yet answered.
    SELECT i.requester INTO asker FROM intake i
    WHERE i.id = p_intake AND i.kind = 'deposit'
      AND NOT EXISTS (SELECT 1 FROM intake_answers a WHERE a.intake_id = i.id);
    SELECT i.requester INTO depositor
    FROM intake_answers a JOIN intake i ON i.id = a.intake_id
    WHERE a.transfer_id = t.id AND NOT a.conversion;
    IF asker IS NULL OR depositor IS NULL OR asker <> depositor THEN
        RAISE EXCEPTION 'conversion refused: deposit %''s output is named only by the requester that made it, in a request of its own (REQ-RQ-4, REQ-ID-4)',
            t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    PERFORM coordinate_lock(t.source_network, t.source_block_number, t.source_block_hash,
                            t.source_position_kind, t.source_position_index);
    PERFORM coordinate_lock(p_network, p_block, p_hash, p_kind, p_index);

    SELECT o.network, o.token, o.amount, o.recipient INTO arrival
    FROM observations o
    WHERE o.network = t.source_network AND o.block_number = t.source_block_number
      AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
      AND o.position_index = t.source_position_index AND o.kind = 'convertible_arrival';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'conversion refused: deposit % is not bound to a convertible arrival (REQ-CV-1, REQ-CV-2)',
            t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    -- The output: a final, executed conversion output at the same reserve, of a token admitted for
    -- the deposit's asset, not a surplus contribution (amendment 0018, A2). Final as 0046 has it.
    SELECT o.network, o.token, o.amount, o.recipient, o.sender, o.block_number INTO output
    FROM observations o
    JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final'
    WHERE o.network = p_network AND o.block_number = p_block AND o.block_hash = p_hash
      AND o.position_kind = p_kind AND o.position_index = p_index
      AND o.kind = 'conversion_output' AND o.executed
      AND o.token IS NOT NULL AND o.amount IS NOT NULL AND o.on_chain_reference IS NULL
      AND EXISTS (SELECT 1 FROM final_heads h
                  WHERE h.network = o.network AND h.block_number = x.final_block_number
                    AND h.block_hash = x.final_block_hash AND h.block_number >= o.block_number)
      AND NOT EXISTS (SELECT 1 FROM final_heads h
                      WHERE h.network = o.network AND h.block_number = o.block_number
                        AND h.block_hash <> o.block_hash);
    IF NOT FOUND THEN
        RAISE EXCEPTION 'conversion refused: the coordinate does not hold a final conversion output (REQ-CV-2, INV-6)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF output.network <> arrival.network OR output.recipient IS DISTINCT FROM arrival.recipient
       OR NOT EXISTS (SELECT 1 FROM admitted_tokens k
                      WHERE k.network = output.network AND k.token = output.token
                        AND k.asset = t.asset)
       OR funding_arrival(t.asset, output.network, output.sender, output.block_number) THEN
        RAISE EXCEPTION 'conversion refused: the output is not of deposit %''s asset at the reserve its convertible arrived at (REQ-CV-2, REQ-CV-7, REQ-IN-4)',
            t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    INSERT INTO deposit_conversions (
        transfer_id, intake_id,
        arrival_network, arrival_block_number, arrival_block_hash, arrival_position_kind,
        arrival_position_index, arrival_token, arrival_amount,
        output_network, output_block_number, output_block_hash, output_position_kind,
        output_position_index, output_amount)
    VALUES (
        t.id, p_intake,
        t.source_network, t.source_block_number, t.source_block_hash, t.source_position_kind,
        t.source_position_index, arrival.token, arrival.amount,
        p_network, p_block, p_hash, p_kind, p_index, output.amount);

    UPDATE transfers SET
        source_network = p_network, source_block_number = p_block, source_block_hash = p_hash,
        source_position_kind = p_kind, source_position_index = p_index, amount = output.amount
    WHERE id = t.id;
END
$$;

COMMENT ON FUNCTION deposit_conversion_record(BIGINT, BIGINT, TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-RQ-4, REQ-CV-2, INV-2, INV-6 (amendment 0018, A1b; Open 6): records a deposit''s conversion and moves '
    'its source and amount from its convertible arrival to the output, once, while it awaits it or is in review '
    'from there with no refund recorded or started. The ingress alone calls it.';

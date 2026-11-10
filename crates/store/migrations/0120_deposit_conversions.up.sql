-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #10 (F-010), amendment 0018 as ruled (`d-cv-conversion-link`, A1b: the request comes first).
--
-- A deposit may name a final convertible arrival as its source (REQ-RQ-3, REQ-CV-1). The core takes
-- it to Source-Final holding CND-AwaitingConversion, and nothing is minted against it: the mint
-- order's guard (0049, 0056) admits only an observed token admitted for the deposit's asset, which
-- a convertible is not. A later request by the same requester names, by the deposit's request
-- reference, the conversion's output (REQ-RQ-4, REQ-CV-2). The deposit's source then moves, once,
-- from the arrival to the output, with its amount, as one guarded update; the arrival is recorded
-- against the deposit with its amount and stays consumed (INV-2), and the mint is the output's
-- (INV-6, REQ-CV-5).
--
--   * deposit_conversions: the record of REQ-CV-2 (the convertible arrival and the converted output,
--     with both amounts), one per deposit, the arrival and the output each once. Written only by
--     deposit_conversion_record(), with the move, in one transaction.
--   * deposit_conversion_record(): the move. SECURITY DEFINER; the ingress alone may call it. It
--     checks what A1b needs (amendment 0018, A2): the deposit is the caller's request's requester's,
--     at Source-Final holding CND-AwaitingConversion, bound to a convertible arrival; the output is
--     a final, executed conversion output at the same reserve on the same network, of a token
--     admitted for the deposit's asset, not a surplus contribution, not consumed. That the arrival
--     fed that conversion is the requester's word; the store cannot check it (open, amendment 0018).
--   * transfers_guard_update (0003): the source binding and the amount stay write-once, except for
--     that one move, which a record row must match.
--   * source_bindable() (0034): a deposit may name a `convertible_arrival` too, with the exclusions an
--     `arrival` has. A refund still never names one (REQ-CV-1, REQ-CV-6).
--   * transfers_source_bindable() (0034): checks the kind on a move too, and refuses a binding of a
--     coordinate a deposit recorded as its convertible arrival (INV-2). The binding and the move take
--     the same transaction-scoped lock on the coordinate, so neither passes the other unseen.
--   * transfers_observed_amount() (0048): a deposit bound to a convertible arrival takes the amount
--     observed and the asset of the reserve the arrival is at (the convertible is not an admitted
--     token, so its family is the reserve's).
--   * unassociated_arrivals (0014): a recorded convertible arrival is not unassociated.
--   * intake_answers: a deposit is answered to its own request and, once, to the request that named
--     its conversion's output.

-- ---------------------------------------------------------------------------------------------
-- The record.

CREATE TABLE deposit_conversions (
    transfer_id            BIGINT PRIMARY KEY REFERENCES transfers (id),
    -- The request that named the output (REQ-RQ-4), the deposit's requester's.
    intake_id              BIGINT NOT NULL UNIQUE REFERENCES intake (id),
    -- The convertible arrival the deposit named first, and what was observed there.
    arrival_network        TEXT NOT NULL,
    arrival_block_number   BIGINT NOT NULL,
    arrival_block_hash     BYTEA NOT NULL,
    arrival_position_kind  position_kind NOT NULL,
    arrival_position_index BIGINT NOT NULL,
    arrival_token          TEXT NOT NULL,
    arrival_amount         minor_units NOT NULL,
    -- The conversion's output it became, and its amount: the deposit's source and amount from now on.
    output_network         TEXT NOT NULL,
    output_block_number    BIGINT NOT NULL,
    output_block_hash      BYTEA NOT NULL,
    output_position_kind   position_kind NOT NULL,
    output_position_index  BIGINT NOT NULL,
    output_amount          minor_units NOT NULL,
    recorded_at            TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT deposit_conversions_one_network CHECK (arrival_network = output_network),
    CONSTRAINT deposit_conversions_arrival_once
        UNIQUE (arrival_network, arrival_block_number, arrival_block_hash, arrival_position_kind,
                arrival_position_index),
    CONSTRAINT deposit_conversions_output_once
        UNIQUE (output_network, output_block_number, output_block_hash, output_position_kind,
                output_position_index)
);

COMMENT ON TABLE deposit_conversions IS
    'REQ-CV-2, REQ-RQ-4, INV-2 (amendment 0018, A1b): a deposit that named a convertible arrival, the '
    'conversion output it then named, and both amounts. The arrival stays consumed by the deposit; '
    'the output is its source. Written only by deposit_conversion_record(). Append-only.';

CREATE TRIGGER deposit_conversions_append_only
    BEFORE UPDATE OR DELETE ON deposit_conversions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-CV-2, INV-2');
CREATE TRIGGER deposit_conversions_no_truncate
    BEFORE TRUNCATE ON deposit_conversions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-CV-2, INV-2');

-- Whether the coordinate is a convertible arrival a deposit recorded (INV-2).
CREATE FUNCTION coordinate_recorded_converted(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (SELECT 1 FROM deposit_conversions c
                   WHERE c.arrival_network = p_network AND c.arrival_block_number = p_block
                     AND c.arrival_block_hash = p_hash AND c.arrival_position_kind = p_kind
                     AND c.arrival_position_index = p_index)
$$;

COMMENT ON FUNCTION coordinate_recorded_converted(TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'INV-2, REQ-CV-2: whether a deposit recorded the coordinate as the convertible arrival its '
    'conversion''s output replaced. Such an arrival is consumed: nothing binds it again.';

-- The transaction-scoped lock a binding and a move take on a coordinate (INV-2).
CREATE FUNCTION coordinate_lock(
    p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind, p_index BIGINT)
    RETURNS VOID
    LANGUAGE sql VOLATILE
    SET search_path = public, pg_temp
    AS $$
    SELECT pg_advisory_xact_lock(hashtextextended(
        'bezent/coordinate/' || p_network || '/' || p_block || '/' || encode(p_hash, 'hex') || '/'
        || p_kind || '/' || p_index, 0))
$$;

-- ---------------------------------------------------------------------------------------------
-- What a request may bind (0034): a deposit may name a convertible arrival.

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
               ELSE FALSE
           END
        FROM observations o
        WHERE o.network = p_network AND o.block_number = p_block AND o.block_hash = p_hash
          AND o.position_kind = p_kind AND o.position_index = p_index
    ), FALSE)
$$;

COMMENT ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2: whether the observation at a '
    'coordinate is one a deposit (p_purpose = deposit: an arrival, a convertible arrival or a '
    'conversion output) or the refund of an unassociated arrival (p_purpose = refund) may name. Its '
    'finality, and whether it is consumed, are checked apart.';

-- The binding rule (0034), now also on a move of a deposit's source, and refusing a coordinate a
-- deposit recorded as its convertible arrival. The binding locks the coordinate first, so a move
-- recording it either committed before (and is seen here) or waits for this transaction.
CREATE OR REPLACE FUNCTION transfers_source_bindable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    purpose TEXT;
    binding BOOLEAN;
    moving BOOLEAN;
    finalising BOOLEAN;
    observed BOOLEAN;
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
    observed := EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index);
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

-- ---------------------------------------------------------------------------------------------
-- The observed amount (0048): a deposit bound to a convertible arrival takes the observed amount,
-- and the asset of the reserve the arrival is at. Unchanged otherwise.

CREATE OR REPLACE FUNCTION transfers_observed_amount() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    filling BOOLEAN := TG_ARGV[0] = 'fill';
    o       RECORD;
    family  TEXT;
BEGIN
    IF NOT (NEW.kind = 'deposit' OR (NEW.kind = 'refund' AND NEW.refund_of IS NULL))
       OR NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT ob.network, ob.token, ob.amount, ob.kind, ob.recipient
    INTO o
    FROM observations ob
    WHERE ob.network = NEW.source_network AND ob.block_number = NEW.source_block_number
      AND ob.block_hash = NEW.source_block_hash
      AND ob.position_kind = NEW.source_position_kind
      AND ob.position_index = NEW.source_position_index;
    IF NOT FOUND THEN
        -- Bound before it was observed (the ingress refuses that, ERR-SourceNotFound): checked
        -- again when the transfer becomes Source-Final, which needs the observation (0046).
        RETURN NEW;
    END IF;
    IF NEW.kind = 'deposit' AND o.kind = 'convertible_arrival' THEN
        -- REQ-CV-1, REQ-CV-2 (amendment 0018, A1b): a convertible is admitted for no asset; the
        -- deposit's is the reserve's it arrived at. Nothing is minted against it (0049's order guard).
        SELECT r.asset INTO family
        FROM reserves r WHERE r.network = o.network AND r.address = o.recipient;
    ELSE
        SELECT k.asset INTO family
        FROM admitted_tokens k WHERE k.network = o.network AND k.token = o.token;
    END IF;

    IF filling THEN
        IF NEW.amount IS NULL THEN
            NEW.amount := o.amount;
        END IF;
        IF NEW.asset IS NULL AND NEW.token IS NULL THEN
            IF family IS NOT NULL THEN
                NEW.asset := family;
            ELSIF NEW.kind = 'refund' THEN
                NEW.token := o.token;
            END IF;
        END IF;
        RETURN NEW;
    END IF;

    IF o.token IS NULL OR o.amount IS NULL THEN
        RAISE EXCEPTION 'transfer % refused: its source observation carries no token or no amount; a deposit and a refund take theirs from it (REQ-IN-4, REQ-RQ-7)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    -- The amount: the observation's, in the origin token's minor units (the book unit, INV-11); for
    -- a deposit awaiting its conversion, the convertible's, never minted (REQ-CV-1).
    IF NEW.amount IS DISTINCT FROM o.amount THEN
        RAISE EXCEPTION 'transfer % refused: amount % is not the % its source observation carries; a transfer''s amount is observed, never a request''s (REQ-IN-4, REQ-CV-2, REQ-RQ-7)',
            NEW.bridge_reference, NEW.amount, o.amount
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.kind = 'deposit' THEN
        -- The same token family (REQ-IN-4): the bridged asset the observed token is admitted for.
        IF family IS NULL THEN
            RAISE EXCEPTION 'deposit % refused: token % on % is not admitted for any bridged asset (REQ-IN-4, REQ-IN-6)',
                NEW.bridge_reference, o.token, o.network
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.asset IS DISTINCT FROM family OR NEW.token IS NOT NULL THEN
            RAISE EXCEPTION 'deposit % refused: asset % is not %, the family of the observed token % (REQ-IN-4, REQ-RQ-7)',
                NEW.bridge_reference, coalesce(NEW.asset, NEW.token), family, o.token
                USING ERRCODE = 'BZ001';
        END IF;
    -- The refund of an unassociated arrival names the observed token's asset, or the token itself
    -- (REQ-IN-6; amendment 0010).
    ELSIF NOT ((NEW.token IS NULL AND family IS NOT NULL AND NEW.asset = family)
               OR (NEW.asset IS NULL AND NEW.token = o.token)) THEN
        RAISE EXCEPTION 'refund % refused: % is not the observed token % nor its asset (REQ-IN-4, REQ-IN-6, REQ-RQ-7)',
            NEW.bridge_reference, coalesce(NEW.asset, NEW.token), o.token
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- The write-once rules (0003), with the one move.

CREATE OR REPLACE FUNCTION transfers_guard_update() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    -- REQ-CV-2, INV-2 (amendment 0018, A1b): a deposit at Source-Final holding
    -- CND-AwaitingConversion moves its source and its amount once, to the output and the amount a
    -- record of deposit_conversions names for it (only deposit_conversion_record() writes one).
    converting BOOLEAN := COALESCE(OLD.kind = 'deposit' AND OLD.state = 'Source-Final'
        AND NEW.state = 'Source-Final'
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

-- ---------------------------------------------------------------------------------------------
-- The move.
--
-- Records the conversion and moves the deposit's source and amount to the output, in one
-- transaction. The writer is the session's login: the ingress (a request's) or the migration
-- owner (drills, NFR-7). Every refusal is BZ001 with the requirement it rests on; a second use of
-- the output or the arrival is the unique indexes' (23505). The ingress checks the same first, so
-- a request is refused with its named error and this is the store's backstop.
CREATE FUNCTION deposit_conversion_record(
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
    IF t.state <> 'Source-Final' OR t.condition IS DISTINCT FROM 'CND-AwaitingConversion' THEN
        RAISE EXCEPTION 'conversion refused: deposit % is % holding %, not Source-Final holding CND-AwaitingConversion; it names its output only while it awaits it (REQ-CV-2, REQ-CV-6, REQ-RV-2)',
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
    'REQ-RQ-4, REQ-CV-2, INV-2, INV-6 (amendment 0018, A1b): records a deposit''s conversion and moves '
    'its source and amount from its convertible arrival to the output, once. The ingress alone calls it.';

-- ---------------------------------------------------------------------------------------------
-- The answers: a deposit is answered to its own request and, once, to the one naming its output.

ALTER TABLE intake_answers ADD COLUMN conversion BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN intake_answers.conversion IS
    'REQ-RQ-4, REQ-CV-2: the answer to the request that named a deposit''s conversion output, which '
    'deposit_conversions records with the same intake.';

DROP INDEX intake_answers_transfer_once;
CREATE UNIQUE INDEX intake_answers_transfer_once
    ON intake_answers (transfer_id, conversion) WHERE transfer_id IS NOT NULL;

CREATE FUNCTION intake_answers_guard_conversion() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF NEW.conversion AND NOT EXISTS (
        SELECT 1 FROM deposit_conversions c
        WHERE c.intake_id = NEW.intake_id AND c.transfer_id = NEW.transfer_id) THEN
        RAISE EXCEPTION 'answer refused: a conversion answer names the deposit its request recorded a conversion for (REQ-RQ-4, REQ-ID-4)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intake_answers_guard_conversion
    BEFORE INSERT ON intake_answers
    FOR EACH ROW EXECUTE FUNCTION intake_answers_guard_conversion();

-- ---------------------------------------------------------------------------------------------
-- A recorded convertible arrival is not unassociated (REQ-IN-6, INV-2): the deposit consumed it.

CREATE OR REPLACE VIEW unassociated_arrivals AS
SELECT
    o.id                               AS observation_id,
    o.network,
    o.block_number,
    o.block_hash,
    o.block_time,
    o.position_kind,
    o.position_index,
    o.token,
    o.amount,
    o.recipient,
    a.asset                            AS admitted_for,
    x.decided_at                       AS final_at,
    'operations'::text                 AS owner,
    one_business_day_after(x.decided_at) AS due_at
FROM observations o
JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final'
JOIN networks n ON n.network = o.network AND n.role = 'origin'
JOIN reserves r ON r.network = o.network AND r.address = o.recipient
LEFT JOIN admitted_tokens a ON a.network = o.network AND a.token = o.token
WHERE o.executed
  AND NOT EXISTS (
    SELECT 1 FROM transfers t
    WHERE t.source_network = o.network AND t.source_block_number = o.block_number
      AND t.source_block_hash = o.block_hash AND t.source_position_kind = o.position_kind
      AND t.source_position_index = o.position_index
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn'))
  AND NOT EXISTS (
    SELECT 1 FROM deposit_conversions c
    WHERE c.arrival_network = o.network AND c.arrival_block_number = o.block_number
      AND c.arrival_block_hash = o.block_hash AND c.arrival_position_kind = o.position_kind
      AND c.arrival_position_index = o.position_index);

-- ---------------------------------------------------------------------------------------------
-- Grants. Nothing here is PUBLIC's. The ingress records a conversion by the function alone (it
-- holds no UPDATE on the source or the amount) and reads the record; the core and the operator read it.

REVOKE ALL ON deposit_conversions FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION
    coordinate_recorded_converted(TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    coordinate_lock(TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    deposit_conversion_record(BIGINT, BIGINT, TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    intake_answers_guard_conversion()
FROM PUBLIC;

GRANT SELECT ON deposit_conversions TO bezent_ingress, bezent_core, bezent_operator;
-- The binding rule runs as the inserting login (0034): the ingress is the one login that binds a
-- deposit's or an unassociated refund's source (0047), so it calls these; the core binds none.
GRANT EXECUTE ON FUNCTION
    coordinate_recorded_converted(TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    coordinate_lock(TEXT, BIGINT, BYTEA, position_kind, BIGINT)
    TO bezent_ingress;
GRANT EXECUTE ON FUNCTION
    deposit_conversion_record(BIGINT, BIGINT, TEXT, BIGINT, BYTEA, position_kind, BIGINT)
    TO bezent_ingress;

-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-001 T-001-03/04: transfers and their state transitions (SPEC §5.4, PLAN DEC-4).
--
-- Store rules:
--   * a transfer is inserted only in an initial state: `Received`, or `Source-Final` for a deposit
--     that names its source binding and its source-final milestone (REQ-IN-3); with no condition;
--   * every later state, terminal ones included, is reached only by appending a transition along
--     an edge of SPEC §5.4 (transfer_edge_allowed below); the transition records the coordinate
--     the change rests on, and the time bezent saw it (REQ-TR-2);
--   * the latency milestones are written by the transition that requires them, from its own
--     coordinate, and by nothing else (REQ-TR-2, REQ-TR-4, NFR-3);
--   * every transition, and the transfer's creation, appends its event to the sequenced log in
--     the same transaction (REQ-ST-4);
--   * a transfer in a terminal state never changes, and no transfer is deleted (INV-5, NFR-7);
--   * what the transfer was asked never changes (REQ-ST-7);
--   * the source binding, the on-chain reference, the refund destination and the expiry are
--     write-once (INV-2, REQ-RF-2, REQ-RQ-4);
--   * a source coordinate, and an on-chain reference, fund at most one transfer (INV-2, INV-10);
--   * transitions are append-only.

CREATE TABLE transfers (
    id                         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    bridge_reference           TEXT NOT NULL UNIQUE CHECK (bridge_reference <> ''),
    request_reference          TEXT NOT NULL CHECK (request_reference <> ''),
    kind                       TEXT NOT NULL CHECK (kind IN ('deposit', 'withdrawal', 'refund')),
    asset                      TEXT NOT NULL CHECK (asset <> ''),
    amount                     minor_units NOT NULL,
    destination_network        TEXT NOT NULL CHECK (destination_network <> ''),
    destination                TEXT NOT NULL CHECK (destination <> ''),
    -- A refund is a transfer of its own (REQ-RF-3). The refund's row names the original here,
    -- at most one refund per original; the original's record reaches its refund through it.
    refund_of                  BIGINT UNIQUE REFERENCES transfers (id),
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Write-once: given in the request, or added later by the redemption's reference (REQ-RQ-4).
    refund_destination_network TEXT CHECK (refund_destination_network <> ''),
    refund_destination         TEXT CHECK (refund_destination <> ''),
    expires_at                 TIMESTAMPTZ,

    -- Write-once: the source binding, the coordinate of the leg that funds it (§5.4, INV-2).
    -- Set only when the leg is bound to this transfer: a request refused because its coordinate
    -- is consumed (ERR-SourceConsumed) keeps the coordinate in intake, not here.
    source_network             TEXT CHECK (source_network <> ''),
    source_block_number        BIGINT CHECK (source_block_number >= 0),
    source_block_hash          BYTEA CHECK (length(source_block_hash) > 0),
    source_position_kind       position_kind,
    source_position_index      BIGINT CHECK (source_position_index BETWEEN 0 AND 4294967295),

    -- Write-once: the on-chain reference the transfer's movements carry (REQ-ID-2), consumed at
    -- most once (INV-2).
    on_chain_reference         BYTEA CHECK (length(on_chain_reference) > 0),

    -- Current state and condition: changed only by a transition (below).
    state                      transfer_state NOT NULL,
    condition                  transfer_condition,

    -- Latency milestones (NFR-3, REQ-TR-4): network, block number, block hash, block time, and
    -- the time bezent saw it. Written only by the transition into the state that requires them
    -- (`Source-Final`, `Delivered`, `Completed`), or at insert for a deposit that begins at
    -- `Source-Final`; never changed afterwards.
    source_final_network                TEXT,
    source_final_block_number           BIGINT,
    source_final_block_hash             BYTEA,
    source_final_block_time             TIMESTAMPTZ,
    source_final_seen_at                TIMESTAMPTZ,
    destination_included_network        TEXT,
    destination_included_block_number   BIGINT,
    destination_included_block_hash     BYTEA,
    destination_included_block_time     TIMESTAMPTZ,
    destination_included_seen_at        TIMESTAMPTZ,
    destination_final_network           TEXT,
    destination_final_block_number      BIGINT,
    destination_final_block_hash        BYTEA,
    destination_final_block_time        TIMESTAMPTZ,
    destination_final_seen_at           TIMESTAMPTZ,

    -- REQ-ID-3: a bridge reference is distinct from the request reference.
    CONSTRAINT transfers_references_distinct CHECK (bridge_reference <> request_reference),
    CONSTRAINT transfers_refund_destination_whole
        CHECK (num_nulls(refund_destination_network, refund_destination) IN (0, 2)),
    CONSTRAINT transfers_source_whole
        CHECK (num_nulls(source_network, source_block_number, source_block_hash,
                         source_position_kind, source_position_index) IN (0, 5)),
    CONSTRAINT transfers_source_final_whole
        CHECK (num_nulls(source_final_network, source_final_block_number, source_final_block_hash,
                         source_final_block_time, source_final_seen_at) IN (0, 5)),
    CONSTRAINT transfers_destination_included_whole
        CHECK (num_nulls(destination_included_network, destination_included_block_number,
                         destination_included_block_hash, destination_included_block_time,
                         destination_included_seen_at) IN (0, 5)),
    CONSTRAINT transfers_destination_final_whole
        CHECK (num_nulls(destination_final_network, destination_final_block_number,
                         destination_final_block_hash, destination_final_block_time,
                         destination_final_seen_at) IN (0, 5)),
    -- N3 of the F-001 review: a condition is held in a non-terminal state only (§5.4).
    CONSTRAINT transfers_no_condition_when_terminal
        CHECK (condition IS NULL OR NOT is_terminal_state(state))
);

COMMENT ON COLUMN transfers.amount IS
    'INV-11, REQ-AS-4: exact integer count of the asset''s book unit, the minor unit of its origin '
    'token (the finer side when the origin token has more precision than the bridged asset). An '
    'issuing-network amount is stored multiplied by the asset''s declared ratio (REQ-AS-9, REQ-AS-10).';

CREATE INDEX transfers_request_reference ON transfers (request_reference);

-- INV-2, INV-10: one source coordinate funds at most one transfer. A refund (refund_of set) may
-- point at the coordinate of the transfer it refunds (REQ-RF-4: tied to the redemption); a refund
-- of an unassociated arrival (REQ-RQ-6) is not a refund_of and conflicts with any deposit that
-- binds the same arrival. A transfer that ended with nothing moved (`Refused`, `Expired`,
-- `Withdrawn`: §5.4) consumed nothing and leaves the index.
CREATE UNIQUE INDEX transfers_source_consumed_once
    ON transfers (source_network, source_block_number, source_block_hash,
                  source_position_kind, source_position_index)
    WHERE source_network IS NOT NULL
      AND refund_of IS NULL
      AND state NOT IN ('Refused', 'Expired', 'Withdrawn');

-- INV-2: an on-chain reference is consumed at most once, on the same terms.
CREATE UNIQUE INDEX transfers_on_chain_reference_consumed_once
    ON transfers (on_chain_reference)
    WHERE on_chain_reference IS NOT NULL
      AND refund_of IS NULL
      AND state NOT IN ('Refused', 'Expired', 'Withdrawn');

CREATE TABLE transitions (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id      BIGINT NOT NULL REFERENCES transfers (id),
    from_state       transfer_state NOT NULL,
    to_state         transfer_state NOT NULL,
    -- The condition the transfer holds after this transition, if any.
    condition        transfer_condition,
    -- The named error of a refusal (§5.4: `Refused` carries one, and only it).
    error            refusal_error,
    -- The coordinate the change rests on, with block number, block hash and block time
    -- (REQ-TR-2). Required by every edge that rests on chain data (transfer_edge_allowed).
    network          TEXT CHECK (network <> ''),
    block_number     BIGINT CHECK (block_number >= 0),
    block_hash       BYTEA CHECK (length(block_hash) > 0),
    block_time       TIMESTAMPTZ,
    position_kind    position_kind,
    position_index   BIGINT CHECK (position_index BETWEEN 0 AND 4294967295),
    -- The time bezent saw it (REQ-TR-2).
    seen_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT transitions_coordinate_whole
        CHECK (num_nulls(network, block_number, block_hash, block_time) IN (0, 4)),
    CONSTRAINT transitions_position_whole
        CHECK (num_nulls(position_kind, position_index) IN (0, 2)
               AND (position_kind IS NULL OR network IS NOT NULL)),
    CONSTRAINT transitions_error_only_when_refused
        CHECK ((to_state = 'Refused') = (error IS NOT NULL)),
    CONSTRAINT transitions_no_condition_when_terminal
        CHECK (condition IS NULL OR NOT is_terminal_state(to_state))
);

CREATE INDEX transitions_transfer ON transitions (transfer_id, id);

-- The state graph of SPEC §5.4, and only the edges it states. Returns whether the edge needs a
-- coordinate (REQ-TR-2), or NULL when the edge is not in the graph.
--
--   Received     -> Refused       refused at request with a named error; rests on no chain data
--   Received     -> Accepted      within route rules; the source leg is not yet final; no chain data
--   Accepted     -> Source-Final  the source leg is final (writes source_final)
--   Accepted     -> Expired       the source leg never arrived before the request's expiry
--   Accepted     -> Withdrawn     the source observation no longer matches the chain
--   Source-Final -> Delivering    a destination transaction is signed and recorded; no chain data
--   Delivering   -> Delivered     the destination leg is included (writes destination_included)
--   Delivered    -> Completed     the destination leg is final and executed in full, REQ-TR-3
--                                 (writes destination_final)
--   Delivering   -> Refunding     the destination leg is proven not executed, at finality, and a
--   Delivered    -> Refunding       refund transfer exists
--   Refunding    -> Refunded      its refund transfer is `Completed`
--   Delivering   -> Needs-Review  a leg is unknown past its deadline (REQ-RV-1); a partial or
--   Delivered    -> Needs-Review    mismatched execution (REQ-TR-3, REQ-TR-5); may rest on none
--   S            -> S             a non-terminal state takes or clears a named condition (§5.4)
--
-- Every other edge is refused, including those §5.4 leaves open: Received -> Source-Final,
-- Received -> Withdrawn, Received -> Expired, Accepted -> Needs-Review,
-- Source-Final -> Needs-Review, Refunding -> Needs-Review, Delivered -> Delivering (a new attempt,
-- REQ-RF-5), and every way out of Needs-Review (REQ-RV-2 names no target state). Each needs the
-- spec to state it before the store admits it.
CREATE FUNCTION transfer_edge_needs_coordinate(from_state transfer_state, to_state transfer_state)
    RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$
SELECT CASE
    WHEN from_state = to_state AND NOT is_terminal_state(from_state) THEN FALSE
    WHEN from_state = 'Received'     AND to_state = 'Refused'      THEN FALSE
    WHEN from_state = 'Received'     AND to_state = 'Accepted'     THEN FALSE
    WHEN from_state = 'Accepted'     AND to_state = 'Source-Final' THEN TRUE
    WHEN from_state = 'Accepted'     AND to_state = 'Expired'      THEN TRUE
    WHEN from_state = 'Accepted'     AND to_state = 'Withdrawn'    THEN TRUE
    WHEN from_state = 'Source-Final' AND to_state = 'Delivering'   THEN FALSE
    WHEN from_state = 'Delivering'   AND to_state = 'Delivered'    THEN TRUE
    WHEN from_state = 'Delivered'    AND to_state = 'Completed'    THEN TRUE
    WHEN from_state = 'Delivering'   AND to_state = 'Refunding'    THEN TRUE
    WHEN from_state = 'Delivered'    AND to_state = 'Refunding'    THEN TRUE
    WHEN from_state = 'Refunding'    AND to_state = 'Refunded'     THEN TRUE
    WHEN from_state = 'Delivering'   AND to_state = 'Needs-Review' THEN FALSE
    WHEN from_state = 'Delivered'    AND to_state = 'Needs-Review' THEN FALSE
    ELSE NULL
END
$$;

-- REQ-TR-2: a transfer is inserted only in an initial state.
CREATE FUNCTION transfers_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.condition IS NOT NULL THEN
        RAISE EXCEPTION 'transfer % is inserted with condition %; a condition is taken only by a transition (REQ-TR-2)',
            NEW.bridge_reference, NEW.condition
            USING ERRCODE = 'BZ001';
    END IF;
    IF num_nonnulls(NEW.destination_included_network, NEW.destination_final_network) > 0 THEN
        RAISE EXCEPTION 'transfer % is inserted with a destination milestone; only its transition writes it (REQ-TR-2)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF NEW.state = 'Received' THEN
        IF NEW.source_final_network IS NOT NULL THEN
            RAISE EXCEPTION 'transfer % is inserted Received with a source-final milestone (REQ-TR-2)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.state = 'Source-Final' THEN
        -- REQ-IN-3: a deposit begins at Source-Final when it binds a final arrival; the binding and
        -- the coordinate it became final at are recorded with it.
        IF NEW.kind <> 'deposit' OR NEW.source_network IS NULL OR NEW.source_final_network IS NULL
           OR NEW.source_final_network <> NEW.source_network THEN
            RAISE EXCEPTION 'transfer % is inserted Source-Final; only a deposit with its source binding and its source-final milestone on the same network begins there (REQ-TR-2, REQ-IN-3)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSE
        RAISE EXCEPTION 'transfer % is inserted %; a transfer begins Received, or Source-Final for a bound deposit, and reaches every other state by a transition (REQ-TR-2, INV-5)',
            NEW.bridge_reference, NEW.state
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- Guards every UPDATE of a transfer.
CREATE FUNCTION transfers_guard_update() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF is_terminal_state(OLD.state) THEN
        RAISE EXCEPTION 'transfer % is % (terminal); a terminal state never changes (INV-5)',
            OLD.bridge_reference, OLD.state
            USING ERRCODE = 'BZ001';
    END IF;

    IF ROW(NEW.id, NEW.bridge_reference, NEW.request_reference, NEW.kind, NEW.asset, NEW.amount,
           NEW.destination_network, NEW.destination, NEW.refund_of, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.bridge_reference, OLD.request_reference, OLD.kind, OLD.asset, OLD.amount,
           OLD.destination_network, OLD.destination, OLD.refund_of, OLD.created_at) THEN
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
               OLD.source_position_kind, OLD.source_position_index) THEN
        RAISE EXCEPTION 'transfer %: the source binding is write-once (INV-2)', OLD.bridge_reference
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

CREATE TRIGGER transfers_guard_insert
    BEFORE INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_insert();
CREATE TRIGGER transfers_guard_update
    BEFORE UPDATE ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_update();
CREATE TRIGGER transfers_no_delete
    BEFORE DELETE ON transfers
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-5, NFR-7');
CREATE TRIGGER transfers_no_truncate
    BEFORE TRUNCATE ON transfers
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-5, NFR-7');

-- REQ-ST-4: a transfer's creation is appended to the sequenced log in its own transaction.
-- SECURITY DEFINER: the log's head is advanced by the migration owner, never by the service role.
CREATE FUNCTION transfers_log_created() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    INSERT INTO event_log (kind, transfer_id, payload)
    VALUES ('transfer', NEW.id, jsonb_build_object(
        'bridge_reference', NEW.bridge_reference,
        'request_reference', NEW.request_reference,
        'state', NEW.state));
    RETURN NULL;
END
$$;

CREATE TRIGGER transfers_log_created
    AFTER INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_log_created();

-- Applies an appended transition to its transfer: checks the edge (§5.4) and its coordinate
-- (REQ-TR-2), writes the milestone the target state requires from the transition's own
-- coordinate, and appends the event (REQ-ST-4), all in the transition's transaction.
-- SECURITY DEFINER: the service role holds no UPDATE privilege on the state, the condition or the
-- milestones; only this function, owned by the migration owner, writes them.
CREATE FUNCTION transitions_apply() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t             transfers%ROWTYPE;
    needs_coord   BOOLEAN;
    refund        transfers%ROWTYPE;
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

    needs_coord := transfer_edge_needs_coordinate(NEW.from_state, NEW.to_state);
    IF needs_coord IS NULL THEN
        RAISE EXCEPTION 'transfer %: % -> % is not an edge of the state graph (§5.4, REQ-TR-2)',
            t.bridge_reference, NEW.from_state, NEW.to_state
            USING ERRCODE = 'BZ001';
    END IF;
    IF needs_coord AND NEW.network IS NULL THEN
        RAISE EXCEPTION 'transfer %: % -> % rests on chain data and must record its coordinate (REQ-TR-2)',
            t.bridge_reference, NEW.from_state, NEW.to_state
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
        UPDATE transfers SET
            destination_included_network = NEW.network,
            destination_included_block_number = NEW.block_number,
            destination_included_block_hash = NEW.block_hash,
            destination_included_block_time = NEW.block_time,
            destination_included_seen_at = NEW.seen_at
        WHERE id = t.id;
    ELSIF NEW.to_state = 'Completed' THEN
        IF NEW.network <> t.destination_network THEN
            RAISE EXCEPTION 'transfer %: Completed rests on the destination network % (REQ-TR-2, REQ-TR-3)',
                t.bridge_reference, t.destination_network
                USING ERRCODE = 'BZ001';
        END IF;
        UPDATE transfers SET
            destination_final_network = NEW.network,
            destination_final_block_number = NEW.block_number,
            destination_final_block_hash = NEW.block_hash,
            destination_final_block_time = NEW.block_time,
            destination_final_seen_at = NEW.seen_at
        WHERE id = t.id;
    ELSIF NEW.to_state = 'Expired' THEN
        -- §5.4: the source leg never arrived before the request's expiry: the coordinate is a
        -- block at or past it.
        IF t.expires_at IS NULL OR NEW.block_time < t.expires_at THEN
            RAISE EXCEPTION 'transfer %: Expired needs an expiry and a coordinate at or past it (REQ-TR-2)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.to_state = 'Refunding' AND NEW.from_state <> 'Refunding' THEN
        -- §5.4: a refund transfer exists (REQ-RF-3).
        IF NOT EXISTS (SELECT 1 FROM transfers WHERE refund_of = t.id) THEN
            RAISE EXCEPTION 'transfer %: Refunding needs its refund transfer (REQ-RF-3)', t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NEW.to_state = 'Refunded' THEN
        -- §5.4: its refund transfer is Completed; the change rests on that refund's final point.
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
        'network', NEW.network,
        'block_number', NEW.block_number,
        'block_hash', encode(NEW.block_hash, 'hex'),
        'block_time', NEW.block_time,
        'seen_at', NEW.seen_at));
    RETURN NULL;
END
$$;

CREATE TRIGGER transitions_apply
    AFTER INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_apply();
CREATE TRIGGER transitions_append_only
    BEFORE UPDATE OR DELETE ON transitions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-TR-2, REQ-ST-7');
CREATE TRIGGER transitions_no_truncate
    BEFORE TRUNCATE ON transitions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-TR-2, REQ-ST-7');

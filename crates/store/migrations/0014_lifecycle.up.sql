-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-003 T-003-01: the store's state graph made the §5.4 transition table (amendment 0009), numbered
-- delivery attempts with their own milestones, one signature per (reference, attempt) behind the
-- proof of non-execution (REQ-EO-6, issue #24), and the records review and the core read (§5.9,
-- §10, REQ-GV-5). Additive: no merged migration changes; functions that change get new names and
-- their triggers are pointed at them, so the down migration points them back.
--
--   * CND-CutOff joins the named conditions (amendment 0010 A2).
--   * networks.backend: how a redemption names the origin network it is paid out on (the
--     on-chain component's `Backend`, PLAN DEC-6), so the core maps it by declaration.
--   * final_heads: the final point an endpoint read on each network, with its block time. Every
--     decision rests on one (REQ-OB-5, INV-16); one hash per height (F-001 NOTES, S4).
--   * attempt_proof() and attempt_gate_open(): the proof of non-execution of an attempt, read from
--     the endpoints' non_execution_proofs (F-007, PR #31) once that table exists.
--   * intents.attempt and intents.reference_scope; signatures copy them, with the network and the
--     purpose. One signature per (network, purpose, scope, reference, attempt); attempt n+1 only
--     once attempt n is proven not executed (REQ-EO-6). References of different kinds are
--     separate namespaces (amendment 0008): a redemption's reference is scoped to its redeemer.
--   * transfers.redeemer: a withdrawal's reference is (redeemer, reference); the store's
--     one-reference-once index keys on it, so a redemption can never pre-empt a mint's reference.
--   * transitions.attempt and transitions.manual_action_id; attempt_milestones: inclusion and
--     finality per attempt; the transfer's destination milestones are those of the attempt that
--     completed it, written by `Completed` (F-001 NOTES, S5).
--   * manual_actions and manual_approvals: the append-only record of every manual action, with
--     actor and justification, and its second approver (REQ-GV-5). Written by the operator's role
--     only.
--   * the state graph of §5.4 (transfer_edge, transitions_apply_lifecycle), the reviews view with
--     owner and service level (REQ-RV-1), and the unassociated_arrivals view (CND-Unassociated,
--     REQ-IN-6, REQ-ST-6).

-- ---------------------------------------------------------------------------------------------
-- Conditions (§10, amendment 0010 A2).

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff'
    ));

-- ---------------------------------------------------------------------------------------------
-- Declarations: the backend a redemption names (`evm:<chain id>` or `attested:<id>`).

ALTER TABLE assets ADD COLUMN issuing_asset TEXT UNIQUE CHECK (issuing_asset <> '');

COMMENT ON COLUMN assets.issuing_asset IS
    'The asset''s identity on the issuing network, as the on-chain component and its events name it '
    '(e.g. `Here(101)`, PLAN DEC-7). The core writes it into a mint order and maps events back by it.';

ALTER TABLE networks ADD COLUMN backend TEXT UNIQUE
    CHECK (backend ~ '^(evm|attested):[0-9]+$');

COMMENT ON COLUMN networks.backend IS
    'How the on-chain component names this origin network in a redemption (PLAN DEC-6 Backend): '
    '`evm:<chain id>` or `attested:<id>`. The core maps a redemption to its network by it.';

COMMENT ON COLUMN routes.cutoffs IS
    'REQ-LM-1, CND-CutOff: the windows in which the route is closed, an array of '
    '{"weekdays": [1..7, ISO, Monday = 1], "from": "HH:MM", "to": "HH:MM"} in UTC, `to` exclusive '
    'and after `from`. A transfer whose next step falls inside one holds CND-CutOff until it ends.';

-- ---------------------------------------------------------------------------------------------
-- Final heads (REQ-OB-2, REQ-OB-5, INV-16).

CREATE TABLE final_heads (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network      TEXT NOT NULL CHECK (network <> ''),
    block_number BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash   BYTEA NOT NULL CHECK (length(block_hash) > 0),
    block_time   TIMESTAMPTZ NOT NULL,
    seen_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- No chain finalises two blocks at one height (F-001 NOTES, S4).
    CONSTRAINT final_heads_one_hash_per_height UNIQUE (network, block_number)
);

CREATE INDEX final_heads_latest ON final_heads (network, block_number DESC);

CREATE TRIGGER final_heads_append_only
    BEFORE UPDATE OR DELETE ON final_heads
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-OB-2, INV-16');
CREATE TRIGGER final_heads_no_truncate
    BEFORE TRUNCATE ON final_heads
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-OB-2, INV-16');

-- ---------------------------------------------------------------------------------------------
-- Attempts on intents, and the proof of non-execution (REQ-NX-1, REQ-NX-2, REQ-EO-6).

ALTER TABLE intents ADD COLUMN attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1);
ALTER TABLE intents ADD COLUMN reference_scope TEXT CHECK (reference_scope <> '');

COMMENT ON COLUMN intents.attempt IS
    'REQ-EO-6: the delivery attempt this intent signs for, from 1. Attempt n+1 carries the same '
    'on-chain reference and is recorded only once attempt n is proven not executed.';
COMMENT ON COLUMN intents.reference_scope IS
    'Amendment 0008: the redeemer, for an intent that carries a redemption''s reference (a payout '
    'or a refund mint); NULL for a mint''s reference.';

CREATE INDEX intents_transfer_attempt ON intents (transfer_id, attempt) WHERE transfer_id IS NOT NULL;
CREATE INDEX intents_lane ON intents (network, purpose, on_chain_reference, attempt)
    WHERE on_chain_reference IS NOT NULL;

-- The proofs of non-execution are the endpoints' (PR #31, F-007: `non_execution_proofs`, one per
-- signed transaction, with what REQ-NX-2 says makes it checkable). That table is not on main yet,
-- so the store reads it only when it exists (`to_regclass`); without it no proof exists, and every
-- gate below stays closed: fail closed. A proof answers a signed transaction, whose intent is the
-- signing intent of a (transfer, attempt), or of a lane and attempt.

-- The proof of non-execution of a transfer's attempt, if one is recorded: its network and the
-- final block that proves it (REQ-NX-2). Every column NULL when there is none.
CREATE FUNCTION attempt_proof(
    p_transfer BIGINT, p_attempt INTEGER,
    OUT network TEXT, OUT final_block_number BIGINT, OUT final_block_hash BYTEA
)
    LANGUAGE plpgsql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF to_regclass('public.non_execution_proofs') IS NULL THEN
        RETURN;
    END IF;
    EXECUTE 'SELECT n.network, n.final_block_number, n.final_block_hash '
         || 'FROM non_execution_proofs n '
         || 'JOIN signed_transactions st ON st.id = n.signed_transaction_id '
         || 'JOIN intents i ON i.id = st.intent_id '
         || 'WHERE i.transfer_id = $1 AND i.attempt = $2 ORDER BY n.id LIMIT 1'
        INTO network, final_block_number, final_block_hash
        USING p_transfer, p_attempt;
END
$$;

-- REQ-EO-6: whether attempt `p_attempt` may be recorded or signed in its lane (network, purpose,
-- reference scope, on-chain reference). Attempt 1 always; attempt n+1 only with the same reference
-- and once a signed transaction of an intent of attempt n in the lane is proven not executed.
-- Attempt n itself could only be recorded once n-1 was proven, so every earlier one is proven too.
-- SECURITY DEFINER: the signer may read intents but not the proofs.
CREATE FUNCTION attempt_gate_open(
    p_network TEXT, p_purpose TEXT, p_scope TEXT, p_reference BYTEA, p_attempt INTEGER
) RETURNS BOOLEAN
    LANGUAGE plpgsql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    proven BOOLEAN;
BEGIN
    IF p_attempt = 1 THEN
        RETURN TRUE;
    END IF;
    IF p_reference IS NULL OR p_attempt < 1 OR to_regclass('public.non_execution_proofs') IS NULL THEN
        RETURN FALSE;
    END IF;
    EXECUTE 'SELECT EXISTS (SELECT 1 FROM non_execution_proofs n '
         || 'JOIN signed_transactions st ON st.id = n.signed_transaction_id '
         || 'JOIN intents i ON i.id = st.intent_id '
         || 'WHERE i.network = $1 AND i.purpose = $2 AND i.reference_scope IS NOT DISTINCT FROM $3 '
         || 'AND i.on_chain_reference = $4 AND i.attempt = $5)'
        INTO proven
        USING p_network, p_purpose, p_scope, p_reference, p_attempt - 1;
    RETURN proven;
END
$$;

-- An intent's attempt passes the gate, and an intent for a transfer carries that transfer's
-- reference and scope: a lane is one transfer's movement (REQ-EO-6, INV-2).
CREATE FUNCTION intents_guard_attempt() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t transfers%ROWTYPE;
BEGIN
    IF NEW.transfer_id IS NOT NULL THEN
        SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
        IF NEW.on_chain_reference IS DISTINCT FROM t.on_chain_reference
           OR NEW.reference_scope IS DISTINCT FROM t.redeemer THEN
            RAISE EXCEPTION 'intent for transfer % must carry its on-chain reference and scope (INV-2, REQ-EO-6)',
                t.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    IF NOT attempt_gate_open(NEW.network, NEW.purpose, NEW.reference_scope,
                             NEW.on_chain_reference, NEW.attempt) THEN
        RAISE EXCEPTION 'intent attempt % refused: attempt n+1 carries the same on-chain reference and is recorded only once attempt n is proven not executed (REQ-EO-6)',
            NEW.attempt
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_guard_attempt
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_attempt();

-- ---------------------------------------------------------------------------------------------
-- Signatures: one per (network, purpose, scope, reference, attempt), behind the gate (#24).

ALTER TABLE signatures ADD COLUMN network TEXT;
ALTER TABLE signatures ADD COLUMN purpose TEXT;
ALTER TABLE signatures ADD COLUMN reference_scope TEXT;
ALTER TABLE signatures ADD COLUMN attempt INTEGER;

-- Rows recorded before this migration take their lane from their intent. The table is
-- append-only; only the migration owner, here, may lift that for the copy.
ALTER TABLE signatures DISABLE TRIGGER signatures_append_only;
UPDATE signatures s SET network = i.network, purpose = i.purpose,
                        reference_scope = i.reference_scope, attempt = i.attempt
FROM intents i WHERE i.id = s.intent_id;
ALTER TABLE signatures ENABLE TRIGGER signatures_append_only;

ALTER TABLE signatures ALTER COLUMN network SET NOT NULL;
ALTER TABLE signatures ALTER COLUMN purpose SET NOT NULL;
ALTER TABLE signatures ALTER COLUMN attempt SET NOT NULL;

DROP INDEX signatures_once_per_reference;
CREATE UNIQUE INDEX signatures_once_per_reference_attempt
    ON signatures (network, purpose, COALESCE(reference_scope, ''), on_chain_reference, attempt)
    WHERE on_chain_reference IS NOT NULL;

-- As signatures_from_intent (0012), and also the lane and the attempt, and the gate.
CREATE FUNCTION signatures_from_intent_attempt() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
DECLARE
    rec intents%ROWTYPE;
    declared JSONB;
BEGIN
    SELECT * INTO rec FROM intents WHERE id = NEW.intent_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'signature for intent % refused: no such intent (INV-9)', NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    declared := rec.payload -> 'amount';
    IF declared IS NULL OR jsonb_typeof(declared) <> 'string' OR NOT (declared #>> '{}') ~ '^[0-9]+$' THEN
        RAISE EXCEPTION 'signature for intent % refused: the intent has no amount in minor units (INV-11)', NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT attempt_gate_open(rec.network, rec.purpose, rec.reference_scope,
                             rec.on_chain_reference, rec.attempt) THEN
        RAISE EXCEPTION 'signature for intent % refused: attempt % is signed only once attempt % is proven not executed (REQ-EO-6)',
            NEW.intent_id, rec.attempt, rec.attempt - 1
            USING ERRCODE = 'BZ001';
    END IF;
    NEW.on_chain_reference := rec.on_chain_reference;
    NEW.amount := (declared #>> '{}')::minor_units;
    NEW.network := rec.network;
    NEW.purpose := rec.purpose;
    NEW.reference_scope := rec.reference_scope;
    NEW.attempt := rec.attempt;
    RETURN NEW;
END
$$;

DROP TRIGGER signatures_from_intent ON signatures;
CREATE TRIGGER signatures_from_intent
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_from_intent_attempt();

COMMENT ON FUNCTION signatures_from_intent() IS 'Superseded by signatures_from_intent_attempt() (0013).';

-- ---------------------------------------------------------------------------------------------
-- Transfers: a redemption's reference is scoped to its redeemer (amendment 0008).

ALTER TABLE transfers ADD COLUMN redeemer TEXT CHECK (redeemer <> '');
ALTER TABLE transfers ADD CONSTRAINT transfers_redeemer_of_a_redemption
    CHECK (redeemer IS NULL OR kind IN ('withdrawal', 'refund'));

COMMENT ON COLUMN transfers.redeemer IS
    'Amendment 0008, REQ-OT-2: the redeemer of the redemption a withdrawal (or its refund mint) is '
    'bound to; with on_chain_reference it is the redemption''s reference. Set at insert, never changed.';

CREATE FUNCTION transfers_redeemer_unchanged() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'transfer %: its redeemer never changes; it is part of the redemption''s reference (REQ-ST-7, REQ-OT-2)',
        OLD.bridge_reference
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER transfers_redeemer_unchanged
    BEFORE UPDATE ON transfers
    FOR EACH ROW WHEN (NEW.redeemer IS DISTINCT FROM OLD.redeemer)
    EXECUTE FUNCTION transfers_redeemer_unchanged();

-- INV-2 with amendment 0008: a mint's references and a redemption's are separate namespaces, and a
-- redemption's is (redeemer, reference). Otherwise as transfers_on_chain_reference_consumed_once.
DROP INDEX transfers_on_chain_reference_consumed_once;
CREATE UNIQUE INDEX transfers_on_chain_reference_consumed_once_per_namespace
    ON transfers ((kind = 'withdrawal'), COALESCE(redeemer, ''), on_chain_reference)
    WHERE on_chain_reference IS NOT NULL
      AND refund_of IS NULL
      AND state NOT IN ('Refused', 'Expired', 'Withdrawn');

CREATE INDEX transfers_open ON transfers (id) WHERE NOT is_terminal_state(state);
CREATE INDEX observations_reference ON observations (network, on_chain_reference)
    WHERE on_chain_reference IS NOT NULL;

-- ---------------------------------------------------------------------------------------------
-- Manual actions and their second approver (REQ-GV-5, REQ-RV-2, REQ-IN-6).

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_operator') THEN
        BEGIN
            CREATE ROLE bezent_operator NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;
    IF current_user = 'bezent_operator'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_operator' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_operator (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_operator' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_operator must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

CREATE TABLE manual_actions (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- review-resolution: ends a review with evidence (REQ-RV-2); release-unassociated: releases an
    -- unassociated arrival by its refund (REQ-IN-6, REQ-RQ-6).
    kind           TEXT NOT NULL CHECK (kind IN ('review-resolution', 'release-unassociated')),
    transfer_id    BIGINT NOT NULL REFERENCES transfers (id),
    -- The state a review resolution leads to.
    to_state       transfer_state,
    -- The evidence: the coordinate of the edge the resolution runs (REQ-RV-2).
    network        TEXT CHECK (network <> ''),
    block_number   BIGINT CHECK (block_number >= 0),
    block_hash     BYTEA CHECK (length(block_hash) > 0),
    block_time     TIMESTAMPTZ,
    actor          TEXT NOT NULL CHECK (actor <> ''),
    justification  TEXT NOT NULL CHECK (justification <> ''),
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT manual_actions_coordinate_whole
        CHECK (num_nulls(network, block_number, block_hash, block_time) IN (0, 4)),
    CONSTRAINT manual_actions_resolution_has_evidence
        CHECK (kind <> 'review-resolution' OR (to_state IS NOT NULL AND network IS NOT NULL))
);

CREATE TABLE manual_approvals (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- Checked by manual_approvals_second_person(), not by a foreign key.
    action_id     BIGINT NOT NULL UNIQUE,
    approver      TEXT NOT NULL CHECK (approver <> ''),
    justification TEXT NOT NULL CHECK (justification <> ''),
    approved_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE FUNCTION manual_approvals_second_person() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    by_actor TEXT;
BEGIN
    SELECT actor INTO by_actor FROM manual_actions WHERE id = NEW.action_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'approval of manual action % refused: no such action (REQ-GV-5)', NEW.action_id
            USING ERRCODE = 'BZ001';
    END IF;
    IF by_actor = NEW.approver THEN
        RAISE EXCEPTION 'manual action % is approved by its own actor; a second approver is another person (REQ-GV-5)',
            NEW.action_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER manual_approvals_second_person
    BEFORE INSERT ON manual_approvals
    FOR EACH ROW EXECUTE FUNCTION manual_approvals_second_person();
CREATE TRIGGER manual_actions_append_only
    BEFORE UPDATE OR DELETE ON manual_actions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');
CREATE TRIGGER manual_actions_no_truncate
    BEFORE TRUNCATE ON manual_actions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');
CREATE TRIGGER manual_approvals_append_only
    BEFORE UPDATE OR DELETE ON manual_approvals
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');
CREATE TRIGGER manual_approvals_no_truncate
    BEFORE TRUNCATE ON manual_approvals
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');

-- ---------------------------------------------------------------------------------------------
-- Transitions: attempts, manual actions, and per-attempt milestones.

ALTER TABLE transitions ADD COLUMN attempt INTEGER CHECK (attempt >= 1);
-- Checked by transitions_apply_lifecycle(), not by a foreign key (a TRUNCATE of manual_actions
-- meets its append-only refusal).
ALTER TABLE transitions ADD COLUMN manual_action_id BIGINT;
ALTER TABLE transitions ADD CONSTRAINT transitions_attempt_only_into_delivering
    CHECK (attempt IS NULL OR to_state = 'Delivering');
-- A manual action is run once (REQ-GV-5).
CREATE UNIQUE INDEX transitions_manual_action_once
    ON transitions (manual_action_id) WHERE manual_action_id IS NOT NULL;

CREATE TABLE attempt_milestones (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id   BIGINT NOT NULL REFERENCES transfers (id),
    attempt       INTEGER NOT NULL CHECK (attempt >= 1),
    milestone     TEXT NOT NULL CHECK (milestone IN ('included', 'final')),
    network       TEXT NOT NULL,
    block_number  BIGINT NOT NULL,
    block_hash    BYTEA NOT NULL,
    block_time    TIMESTAMPTZ NOT NULL,
    seen_at       TIMESTAMPTZ NOT NULL,
    -- The transition that wrote it. No foreign key, so a TRUNCATE of transitions meets its
    -- append-only refusal, not a reference (as F-007's tables do).
    transition_id BIGINT NOT NULL UNIQUE,
    CONSTRAINT attempt_milestones_once UNIQUE (transfer_id, attempt, milestone)
);

CREATE TRIGGER attempt_milestones_append_only
    BEFORE UPDATE OR DELETE ON attempt_milestones
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-TR-2, NFR-3');
CREATE TRIGGER attempt_milestones_no_truncate
    BEFORE TRUNCATE ON attempt_milestones
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-TR-2, NFR-3');

-- Written by transitions_apply_lifecycle() only: the service role may read them.
CREATE FUNCTION attempt_milestones_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'an attempt''s milestone is written only by its transition (REQ-TR-2)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER attempt_milestones_guard_insert
    BEFORE INSERT ON attempt_milestones
    FOR EACH ROW EXECUTE FUNCTION attempt_milestones_guard_insert();

-- ---------------------------------------------------------------------------------------------
-- The state graph: the transition table of §5.4 (amendment 0009), ordinary edges only. Returns
-- whether the edge rests on chain data (REQ-TR-2), or NULL when it is not an edge.
--
--   Received     -> Refused       refused at request with a named error
--   Received     -> Accepted      within route rules; the source leg is not yet final
--   Accepted     -> Source-Final  the source leg is final and consumed for it          (coordinate)
--   Accepted     -> Expired       the source leg never arrived before the expiry        (coordinate)
--   Accepted     -> Withdrawn     the source observation no longer matches the chain    (coordinate)
--   Accepted     -> Needs-Review  observed but neither final nor withdrawn past its deadline
--   Source-Final -> Delivering    a destination transaction is signed and recorded
--   Source-Final -> Needs-Review  CND-AwaitingConversion or CND-DestinationRefused past deadline
--   Delivering   -> Delivered     the destination leg is included                      (coordinate)
--   Delivered    -> Delivering    a new attempt (also Delivering -> Delivering, marked by its
--                                 attempt number in transitions_apply_lifecycle)      (coordinate)
--   Delivering   -> Refunding     proven not executed at finality; a refund exists     (coordinate)
--   Delivered    -> Refunding       "
--   Delivering   -> Needs-Review  a leg unknown past its deadline; partial or mismatched
--   Delivered    -> Needs-Review    "
--   Delivered    -> Completed     final and executed in full                           (coordinate)
--   Refunding    -> Refunded      its refund transfer is Completed                     (coordinate)
--   S            -> S             a non-terminal state takes or clears a condition
--
-- Needs-Review -> X is not an ordinary edge: transitions_apply_lifecycle admits it when X is one
-- ordinary edge from the state held before review (REQ-RV-2).
CREATE FUNCTION transfer_edge(from_state transfer_state, to_state transfer_state)
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
    WHEN from_state = 'Accepted'     AND to_state = 'Needs-Review' THEN FALSE
    WHEN from_state = 'Source-Final' AND to_state = 'Delivering'   THEN FALSE
    WHEN from_state = 'Source-Final' AND to_state = 'Needs-Review' THEN FALSE
    WHEN from_state = 'Delivering'   AND to_state = 'Delivered'    THEN TRUE
    WHEN from_state = 'Delivered'    AND to_state = 'Delivering'   THEN TRUE
    WHEN from_state = 'Delivering'   AND to_state = 'Refunding'    THEN TRUE
    WHEN from_state = 'Delivered'    AND to_state = 'Refunding'    THEN TRUE
    WHEN from_state = 'Delivering'   AND to_state = 'Needs-Review' THEN FALSE
    WHEN from_state = 'Delivered'    AND to_state = 'Needs-Review' THEN FALSE
    WHEN from_state = 'Delivered'    AND to_state = 'Completed'    THEN TRUE
    WHEN from_state = 'Refunding'    AND to_state = 'Refunded'     THEN TRUE
    ELSE NULL
END
$$;

COMMENT ON FUNCTION transfer_edge_needs_coordinate(transfer_state, transfer_state) IS
    'Superseded by transfer_edge() (0013, amendment 0009).';

-- A review resolution that moves value: it releases a mint, signs a delivery, or starts a refund
-- (REQ-RV-2, REQ-GV-5).
CREATE FUNCTION resolution_moves_value(to_state transfer_state) RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT to_state IN ('Source-Final', 'Delivering', 'Refunding') $$;

-- Applies an appended transition to its transfer (replaces transitions_apply() of 0003 as the
-- trigger): checks the edge against the §5.4 table (REQ-TR-6), the attempt and its proof
-- (REQ-EO-6), the review's evidence and second approver (REQ-RV-2, REQ-GV-5), writes the
-- milestones from the transition's own coordinate (REQ-TR-2, NFR-3), and appends the event
-- (REQ-ST-4), all in the transition's transaction.
-- SECURITY DEFINER: only this function, owned by the migration owner, writes the state, the
-- condition and the milestones.
CREATE FUNCTION transitions_apply_lifecycle() RETURNS trigger
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

    -- A delivery attempt: from Source-Final (attempt 1), or a new attempt from Delivering or
    -- Delivered. Delivering -> Delivering without an attempt number is a condition only.
    attempt_edge := NEW.to_state = 'Delivering'
        AND (edge_from = 'Source-Final' OR edge_from = 'Delivered'
             OR (edge_from = 'Delivering' AND NEW.attempt IS NOT NULL));

    -- The edge (REQ-TR-6).
    IF review_exit THEN
        needs_coord := transfer_edge(edge_from, NEW.to_state);
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
        IF t.kind = 'refund' AND t.refund_of IS NULL
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
        -- on the proof's final block, and a refund transfer exists (REQ-RF-3).
        SELECT * INTO proof FROM attempt_proof(t.id, current_attempt);
        IF proof.network IS NULL
           OR ROW(proof.network, proof.final_block_number, proof.final_block_hash)
              IS DISTINCT FROM ROW(NEW.network, NEW.block_number, NEW.block_hash) THEN
            RAISE EXCEPTION 'transfer %: Refunding only once attempt % is proven not executed at finality, resting on the proof''s final block (REQ-RF-1, INV-8)',
                t.bridge_reference, current_attempt
                USING ERRCODE = 'BZ001';
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

DROP TRIGGER transitions_apply ON transitions;
CREATE TRIGGER transitions_apply
    AFTER INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_apply_lifecycle();

COMMENT ON FUNCTION transitions_apply() IS 'Superseded by transitions_apply_lifecycle() (0013).';

-- ---------------------------------------------------------------------------------------------
-- Review: owner and service level (REQ-RV-1, OQ-3 closed by amendment 0005 A2).

-- One business day after `ts`: the same time on the next day that is not a Saturday or a Sunday,
-- in UTC. Public holidays are not declared anywhere yet, so they are not skipped.
CREATE FUNCTION one_business_day_after(ts TIMESTAMPTZ) RETURNS TIMESTAMPTZ
    LANGUAGE sql IMMUTABLE
    AS $$
SELECT ts + make_interval(days => CASE extract(isodow FROM ts AT TIME ZONE 'UTC')::int
                                      WHEN 5 THEN 3   -- Friday -> Monday
                                      WHEN 6 THEN 2   -- Saturday -> Monday
                                      ELSE 1 END)
$$;

-- Each transfer in review: the state it held before, when it entered, its owner and when the
-- service level ends.
CREATE VIEW reviews AS
SELECT
    t.id                               AS transfer_id,
    t.bridge_reference,
    t.kind,
    t.asset,
    t.condition,
    e.from_state                       AS state_before_review,
    e.id                               AS entered_transition_id,
    e.seen_at                          AS entered_at,
    'operations'::text                 AS owner,
    one_business_day_after(e.seen_at)  AS due_at
FROM transfers t
JOIN LATERAL (
    SELECT x.id, x.from_state, x.seen_at FROM transitions x
    WHERE x.transfer_id = t.id AND x.to_state = 'Needs-Review' AND x.from_state <> 'Needs-Review'
    ORDER BY x.id DESC LIMIT 1
) e ON TRUE
WHERE t.state = 'Needs-Review';

-- CND-Unassociated (F-001 NOTES, N4): a final, executed arrival at a reserve address that no
-- transfer binds, or that only transfers which moved nothing bound. Its token may be admitted or
-- not; it is never minted, absorbed or dropped (REQ-IN-6). Owner and service level as review.
CREATE VIEW unassociated_arrivals AS
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
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn'));

-- ---------------------------------------------------------------------------------------------
-- Grants. The endpoints record final heads and proofs (bezent_service); the core reads everything
-- here; only the operator's role records a manual action or its approval.

GRANT SELECT, INSERT ON final_heads TO bezent_service;
GRANT SELECT ON attempt_milestones, manual_actions, manual_approvals, reviews, unassociated_arrivals
    TO bezent_service;

GRANT USAGE ON SCHEMA public TO bezent_operator;
GRANT SELECT, INSERT ON manual_actions, manual_approvals TO bezent_operator;
GRANT SELECT ON reviews, unassociated_arrivals TO bezent_operator;

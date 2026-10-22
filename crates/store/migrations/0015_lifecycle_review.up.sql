-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-003, review of PR #32. Additive: 0014 is left as it is; the functions that change get new
-- names and their triggers are pointed at them, so the down migration points them back.
--
--   * M1 (REQ-OT-2, INV-2, PLAN DEC-10): transfers.destination_reference. A withdrawal's payout
--     writes on its origin network a reference unique to that withdrawal, derived from its bridge
--     reference, which is derived from (redeemer, redemption reference). A redemption's reference
--     alone is scoped to its redeemer (amendment 0008), so two redeemers may share one; the
--     payout's may not. A payout intent of a withdrawal carries it, never the redemption's.
--   * M2 (INV-8, REQ-RF-1, REQ-EO-6): a refund rests on every attempt that has an intent (ordered,
--     prepared or signed) being proven not executed at finality; and once a transfer is refunding
--     (or terminal) its attempt set is closed: no intent is recorded and no signature is recorded
--     for it any more. Both sides lock the transfer, so the refund decision and a new attempt are
--     serialised.
--   * M3 (REQ-GV-5, REQ-RV-2): a manual action's actor and an approval's approver are the
--     session's login, stamped by the store, as are their times; the operator's role may name
--     neither. Each operator logs in as their own login, a member of bezent_operator; an approval
--     by the action's own login is refused.
--   * S6 (PLAN DEC-3, issue #27): only an endpoint's login (bezent_endpoint, as PR #31 makes it)
--     records a final head. The core's login, a member of bezent_service and bezent_core, may not.

-- ---------------------------------------------------------------------------------------------
-- M1: the reference a withdrawal's payout writes on its origin network.

ALTER TABLE transfers ADD COLUMN destination_reference BYTEA
    CHECK (length(destination_reference) = 32);
ALTER TABLE transfers ADD CONSTRAINT transfers_destination_reference_of_a_withdrawal
    CHECK (destination_reference IS NULL OR kind = 'withdrawal');

COMMENT ON COLUMN transfers.destination_reference IS
    'REQ-OT-2, INV-2, PLAN DEC-10: the 32-byte reference a withdrawal''s payout writes on its origin '
    'network (the reference emitter''s Reference(bytes32)), derived from the bridge reference, itself '
    'derived from (redeemer, redemption reference). Unique, set at insert, never changed. Its '
    'destination leg is matched by it alone; on_chain_reference stays the redemption''s.';

CREATE UNIQUE INDEX transfers_destination_reference_once
    ON transfers (destination_reference) WHERE destination_reference IS NOT NULL;

CREATE FUNCTION transfers_destination_reference_unchanged() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'transfer %: its destination reference is set at insert and never changes (INV-2, REQ-OT-2)',
        OLD.bridge_reference
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER transfers_destination_reference_unchanged
    BEFORE UPDATE ON transfers
    FOR EACH ROW WHEN (NEW.destination_reference IS DISTINCT FROM OLD.destination_reference)
    EXECUTE FUNCTION transfers_destination_reference_unchanged();

CREATE INDEX observations_reference_event ON observations (network, on_chain_reference, (detail ->> 'event'))
    WHERE on_chain_reference IS NOT NULL;

-- ---------------------------------------------------------------------------------------------
-- M2: the attempt set of a transfer, closed by its refund.

-- Whether transfer `p_transfer` takes no further attempt: it is refunding or refunded, or has a
-- refund transfer (REQ-RF-3). FOR SHARE: a refund decision in flight holds the transfer FOR UPDATE
-- (transitions_apply_lifecycle), so this waits for it and then sees it; and while this
-- transaction holds the share lock, no refund decision can be made (INV-8).
-- SECURITY DEFINER: neither the core's nor the signer's role may lock a transfer row.
CREATE FUNCTION transfer_attempts_closed(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE plpgsql
    VOLATILE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    st transfer_state;
BEGIN
    SELECT state INTO st FROM transfers WHERE id = p_transfer FOR SHARE;
    IF NOT FOUND THEN
        RETURN FALSE;
    END IF;
    RETURN st IN ('Refunding', 'Refunded')
        OR EXISTS (SELECT 1 FROM transfers WHERE refund_of = p_transfer);
END
$$;

-- The attempts of transfer `p_transfer` that have an intent (an order, or a signing intent: so
-- ordered, prepared or signed) and no recorded proof of non-execution, lowest first.
CREATE FUNCTION attempts_unproven(p_transfer BIGINT) RETURNS INTEGER[]
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
SELECT COALESCE(array_agg(k.attempt ORDER BY k.attempt), '{}')
FROM (SELECT DISTINCT attempt FROM intents WHERE transfer_id = p_transfer) k
WHERE (SELECT p.network FROM attempt_proof(p_transfer, k.attempt) p) IS NULL
$$;

-- As intents_guard_attempt (0014), with the destination reference (M1) and the closed set (M2).
CREATE FUNCTION intents_guard_attempt_set() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t transfers%ROWTYPE;
BEGIN
    IF NEW.transfer_id IS NOT NULL THEN
        IF transfer_attempts_closed(NEW.transfer_id) THEN
            RAISE EXCEPTION 'intent for transfer % refused: it is refunding or refunded, and takes no further attempt (INV-8, REQ-RF-1)',
                NEW.transfer_id
                USING ERRCODE = 'BZ001';
        END IF;
        SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
        IF t.kind = 'withdrawal' AND NEW.purpose = 'payout' THEN
            -- The payout writes the withdrawal's own reference, unscoped: it is unique (M1).
            IF t.destination_reference IS NULL
               OR NEW.on_chain_reference IS DISTINCT FROM t.destination_reference
               OR NEW.reference_scope IS NOT NULL THEN
                RAISE EXCEPTION 'payout intent for withdrawal % must carry its destination reference, unique to it (REQ-OT-2, INV-2)',
                    t.bridge_reference
                    USING ERRCODE = 'BZ001';
            END IF;
        ELSIF NEW.on_chain_reference IS DISTINCT FROM t.on_chain_reference
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

DROP TRIGGER intents_guard_attempt ON intents;
CREATE TRIGGER intents_guard_attempt
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_attempt_set();

COMMENT ON FUNCTION intents_guard_attempt() IS 'Superseded by intents_guard_attempt_set() (0015).';

-- No signature for an intent of a transfer whose attempt set is closed (M2). Beside
-- signatures_from_intent, which still copies the lane and checks the gate.
CREATE FUNCTION signatures_attempt_set_open() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    owner BIGINT;
BEGIN
    SELECT transfer_id INTO owner FROM intents WHERE id = NEW.intent_id;
    IF owner IS NOT NULL AND transfer_attempts_closed(owner) THEN
        RAISE EXCEPTION 'signature for intent % refused: its transfer is refunding or refunded, and takes no further attempt (INV-8, REQ-RF-1)',
            NEW.intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER signatures_attempt_set_open
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_attempt_set_open();

-- Refunding rests on every attempt with an intent proven not executed at finality (INV-8:
-- "every payout attempt"). Beside transitions_apply_lifecycle, which checks the proof the
-- transition rests on and that a refund transfer exists.
CREATE FUNCTION transitions_refund_every_attempt_proven() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    st       transfer_state;
    unproven INTEGER[];
BEGIN
    IF NEW.to_state = 'Refunding' AND NEW.from_state <> 'Refunding' THEN
        -- Waits for any transaction recording an intent or a signature for it (FOR SHARE).
        SELECT state INTO st FROM transfers WHERE id = NEW.transfer_id FOR UPDATE;
        IF NOT FOUND OR is_terminal_state(st) OR st <> NEW.from_state THEN
            RETURN NEW;  -- refused by transitions_apply_lifecycle, with its own reason
        END IF;
        unproven := attempts_unproven(NEW.transfer_id);
        IF cardinality(unproven) > 0 THEN
            RAISE EXCEPTION 'transfer %: Refunding only once every attempt ordered, prepared or signed is proven not executed at finality; attempt % is not (INV-8, REQ-RF-1)',
                NEW.transfer_id, unproven[1]
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transitions_refund_every_attempt_proven
    BEFORE INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_refund_every_attempt_proven();

-- ---------------------------------------------------------------------------------------------
-- M3: the actor and the approver are the session's login; the times are the store's.

ALTER TABLE manual_actions ALTER COLUMN actor SET DEFAULT session_user;
ALTER TABLE manual_approvals ALTER COLUMN approver SET DEFAULT session_user;

COMMENT ON COLUMN manual_actions.actor IS
    'REQ-GV-5: the login that recorded the action (session_user), stamped by the store. Each operator '
    'logs in as their own login, a member of bezent_operator.';
COMMENT ON COLUMN manual_approvals.approver IS
    'REQ-GV-5: the login that approved (session_user), stamped by the store; never the action''s.';

CREATE FUNCTION manual_actions_by_session() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.actor := session_user;
    NEW.recorded_at := now();
    RETURN NEW;
END
$$;

CREATE FUNCTION manual_approvals_by_session() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    NEW.approver := session_user;
    NEW.approved_at := now();
    RETURN NEW;
END
$$;

-- Fires before manual_approvals_second_person (triggers fire in name order), which then compares
-- the two logins.
CREATE TRIGGER manual_actions_by_session
    BEFORE INSERT ON manual_actions
    FOR EACH ROW EXECUTE FUNCTION manual_actions_by_session();
CREATE TRIGGER manual_approvals_by_session
    BEFORE INSERT ON manual_approvals
    FOR EACH ROW EXECUTE FUNCTION manual_approvals_by_session();

-- The operator's role names only what it decides; the store fills in who and when.
REVOKE INSERT ON manual_actions, manual_approvals FROM bezent_operator;
GRANT INSERT (kind, transfer_id, to_state, network, block_number, block_hash, block_time,
              justification)
    ON manual_actions TO bezent_operator;
GRANT INSERT (action_id, justification) ON manual_approvals TO bezent_operator;

-- ---------------------------------------------------------------------------------------------
-- S6: only an endpoint records a final head (as PR #31 does for proofs, prepared transactions and
-- read ranges). The role is created as PR #31 creates it; whichever migration runs first makes it.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_endpoint') THEN
        BEGIN
            CREATE ROLE bezent_endpoint NOLOGIN;
            -- PostgreSQL 16 records the creator as a member of the role it made (ADMIN OPTION, and SET and
            -- INHERIT too if createrole_self_grant says so). The owner keeps ADMIN OPTION (it grants the role to
            -- the services' logins) and drops the rest, so it never acts as the role nor inherits it.
            IF EXISTS (SELECT 1 FROM pg_auth_members m
                       JOIN pg_roles g ON g.oid = m.roleid
                       JOIN pg_roles u ON u.oid = m.member
                       WHERE g.rolname = 'bezent_endpoint' AND u.rolname = current_user
                         AND (m.set_option OR m.inherit_option)) THEN
                REVOKE SET OPTION FOR bezent_endpoint FROM CURRENT_USER;
                REVOKE INHERIT OPTION FOR bezent_endpoint FROM CURRENT_USER;
            END IF;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;
    IF current_user = 'bezent_endpoint'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_endpoint' AND u.rolname = current_user
                    AND (m.set_option OR m.inherit_option)) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_endpoint (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_endpoint' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_endpoint must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname = 'bezent_endpoint'
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_signer', 'bezent_operator')) THEN
        RAISE EXCEPTION 'bezent_service, bezent_core, bezent_signer and bezent_operator must not be members of bezent_endpoint'
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_auth_members m
                   JOIN pg_roles g ON g.oid = m.roleid
                   JOIN pg_roles u ON u.oid = m.member
                   WHERE g.rolname = 'bezent_service' AND u.rolname = 'bezent_endpoint') THEN
        BEGIN
            GRANT bezent_service TO bezent_endpoint;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- granted concurrently by another database's migration on the same server
        END;
    END IF;
END
$$;

REVOKE INSERT ON final_heads FROM bezent_service;
GRANT USAGE ON SCHEMA public TO bezent_endpoint;
GRANT INSERT ON final_heads TO bezent_endpoint;

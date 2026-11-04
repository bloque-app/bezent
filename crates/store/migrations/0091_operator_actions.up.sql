-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-014: the operator surface for the actions that had no record (REQ-GV-5, AC-I3.1), and the
-- pause an operator can make and lift (REQ-GV-1, REQ-GV-2, AC-I2.1).
--
-- Before this, the review resolution and the release of an unassociated arrival were recorded
-- (`manual_actions`, 0014), but a pause could be made and lifted only by the schema owner, with no
-- actor and no justification, and every other administrative act (a key rotation, a raised bound,
-- a route admission, a credential, a surplus movement) left no record in the store at all.
--
--   * `admin_actions` is the append-only record of those acts. The actor is the session's login
--     and the time the store's, stamped by a trigger: a caller names neither. A justification is
--     required. No UPDATE, DELETE or TRUNCATE. Only the functions below insert into it.
--   * `operator_pause` makes a pause and records it in one transaction. A pause stops new work
--     only (INV-15, REQ-GV-1); it needs one authorised party (REQ-GV-2).
--   * `operator_request_unpause` and `operator_approve_unpause` lift a pause in two steps by two
--     different logins (REQ-GV-2: unpausing SHOULD need several). The lift deletes the pause row
--     and records the approval, in one transaction. It applies to a pause the core made as well:
--     the core never lifts its own (F-004 NOTES).
--   * `operator_record` records an act performed elsewhere (a key rotation on the signer's
--     custody, a bound changed on chain, a route admitted by the owner's migration): it changes
--     nothing but the record. It is how REQ-GV-5 reaches what the store does not perform.
--
-- Nothing here moves value or edits a status. The functions touch `pauses` and `admin_actions`
-- and nothing else (REQ-GV-5, AC-I3.2). They run as the schema owner (SECURITY DEFINER) because
-- no service role owns `pauses`, and are executable only by `bezent_operator`.

CREATE TABLE admin_actions (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind          TEXT NOT NULL CHECK (kind IN (
                      'pause', 'unpause-request', 'unpause',
                      'key-rotation', 'bound-change', 'route-admission', 'credential',
                      'surplus-contribution', 'surplus-withdrawal', 'float-top-up', 'other')),
    -- What the act applies to, in words a person reads: an asset, a route, a key's name. Never a
    -- secret and never key material (REQ-KY-1).
    subject       TEXT NOT NULL CHECK (subject <> ''),
    -- Structured detail: the pause's scope, the evidence coordinate, the old and new bound.
    detail        JSONB NOT NULL DEFAULT '{}'::jsonb,
    -- An `unpause` rests on the `unpause-request` it approves, made by another login.
    approves      BIGINT UNIQUE REFERENCES admin_actions (id),
    actor         TEXT NOT NULL DEFAULT session_user CHECK (actor <> ''),
    justification TEXT NOT NULL CHECK (btrim(justification) <> ''),
    recorded_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT admin_actions_approves_only_unpause CHECK ((kind = 'unpause') = (approves IS NOT NULL))
);

COMMENT ON TABLE admin_actions IS
    'REQ-GV-5, AC-I3.1: every administrative act, with its actor (the session login) and its '
    'justification, append-only. A pause, a request to lift it and the approval that lifts it; '
    'acts performed elsewhere are recorded here by operator_record.';

CREATE FUNCTION admin_actions_by_session() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    req admin_actions%ROWTYPE;
BEGIN
    NEW.actor := session_user;
    NEW.recorded_at := now();
    IF NEW.kind = 'unpause' THEN
        SELECT * INTO req FROM admin_actions WHERE id = NEW.approves AND kind = 'unpause-request';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'unpause refused: it approves no request to lift a pause (REQ-GV-2, REQ-GV-5)'
                USING ERRCODE = 'BZ001';
        END IF;
        IF req.actor = NEW.actor THEN
            RAISE EXCEPTION 'unpause refused: the request was made by this login; the approver is another person (REQ-GV-2, REQ-GV-5)'
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER admin_actions_by_session
    BEFORE INSERT ON admin_actions
    FOR EACH ROW EXECUTE FUNCTION admin_actions_by_session();
CREATE TRIGGER admin_actions_append_only
    BEFORE UPDATE OR DELETE ON admin_actions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');
CREATE TRIGGER admin_actions_no_truncate
    BEFORE TRUNCATE ON admin_actions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-GV-5');

-- ---------------------------------------------------------------------------------------------
-- The functions. Each insists on an operator's login (a member of bezent_operator) in addition
-- to the EXECUTE grant, so a grant widened by mistake still refuses.

CREATE FUNCTION operator_require() RETURNS void
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF NOT pg_has_role(session_user, 'bezent_operator', 'MEMBER') THEN
        RAISE EXCEPTION 'refused: only an operator''s own login acts here (REQ-GV-5)'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

-- A pause of one asset, one network, one direction, or any combination; a NULL matches every
-- value, so (NULL, NULL, NULL) pauses the bridge (REQ-GV-1). Returns the pause's id.
CREATE FUNCTION operator_pause(p_asset TEXT, p_network TEXT, p_direction TEXT, p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    pid BIGINT;
BEGIN
    PERFORM operator_require();
    INSERT INTO pauses (asset, network, direction) VALUES (p_asset, p_network, p_direction)
        RETURNING id INTO pid;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES ('pause',
                coalesce('asset=' || p_asset, 'all assets') || ', '
                    || coalesce('network=' || p_network, 'all networks') || ', '
                    || coalesce('direction=' || p_direction, 'both directions'),
                jsonb_build_object('pause_id', pid, 'asset', p_asset, 'network', p_network,
                                   'direction', p_direction),
                p_justification);
    RETURN pid;
END
$$;

-- Step one of lifting a pause: a request, by one login. Returns the request's id.
CREATE FUNCTION operator_request_unpause(p_pause BIGINT, p_justification TEXT) RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    p pauses%ROWTYPE;
    rid BIGINT;
BEGIN
    PERFORM operator_require();
    SELECT * INTO p FROM pauses WHERE id = p_pause;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'unpause request refused: no pause %', p_pause USING ERRCODE = 'BZ001';
    END IF;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES ('unpause-request',
                'pause ' || p_pause,
                jsonb_build_object('pause_id', p.id, 'asset', p.asset, 'network', p.network,
                                   'direction', p.direction, 'cause', p.cause),
                p_justification)
        RETURNING id INTO rid;
    RETURN rid;
END
$$;

-- Step two: another login approves the request, and the pause is lifted. The new work it was
-- holding resumes by itself at the core's next cycle and the ingress's next request.
CREATE FUNCTION operator_approve_unpause(p_request BIGINT, p_justification TEXT) RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    req admin_actions%ROWTYPE;
    aid BIGINT;
BEGIN
    PERFORM operator_require();
    SELECT * INTO req FROM admin_actions WHERE id = p_request AND kind = 'unpause-request';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'unpause approval refused: no such request %', p_request
            USING ERRCODE = 'BZ001';
    END IF;
    -- The trigger refuses the request's own actor, and a request approved twice (UNIQUE).
    INSERT INTO admin_actions (kind, subject, detail, approves, justification)
        VALUES ('unpause', req.subject, req.detail, req.id, p_justification)
        RETURNING id INTO aid;
    DELETE FROM pauses WHERE id = (req.detail ->> 'pause_id')::bigint;
    RETURN aid;
END
$$;

-- An act performed outside the store, recorded here. It changes nothing else.
CREATE FUNCTION operator_record(p_kind TEXT, p_subject TEXT, p_detail JSONB, p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    rid BIGINT;
BEGIN
    PERFORM operator_require();
    IF p_kind IN ('pause', 'unpause-request', 'unpause') THEN
        RAISE EXCEPTION 'refused: % is made by its own function, never recorded after the fact (REQ-GV-5)', p_kind
            USING ERRCODE = 'BZ001';
    END IF;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES (p_kind, p_subject, coalesce(p_detail, '{}'::jsonb), p_justification)
        RETURNING id INTO rid;
    RETURN rid;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- Grants. No direct write to admin_actions or to pauses by an operator.

REVOKE ALL ON admin_actions FROM PUBLIC;
GRANT SELECT ON admin_actions TO bezent_operator;
-- What a runbook's "confirm" reads, read-only: the pauses in force (to name one to lift), a
-- transfer's status and the sequenced log. The log holds every page's own entry with its detail
-- (a requester reads only the entries about its own transfers, through the ingress), so an
-- operator needs the whole of it to confirm a page.
GRANT SELECT ON pauses, transfer_status, event_log TO bezent_operator;

REVOKE EXECUTE ON FUNCTION
    admin_actions_by_session(), operator_require(), operator_pause(TEXT, TEXT, TEXT, TEXT),
    operator_request_unpause(BIGINT, TEXT), operator_approve_unpause(BIGINT, TEXT),
    operator_record(TEXT, TEXT, JSONB, TEXT)
    FROM PUBLIC;
GRANT EXECUTE ON FUNCTION
    operator_pause(TEXT, TEXT, TEXT, TEXT), operator_request_unpause(BIGINT, TEXT),
    operator_approve_unpause(BIGINT, TEXT), operator_record(TEXT, TEXT, JSONB, TEXT)
    TO bezent_operator;

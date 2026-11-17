-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-22 to T-018-25 (amendment 0031; SPEC v0.5.14): five rulings of @pandres95 on the
-- decision form, 2026-11-17, as the store holds them.
--
--   * CND-DepositCapacity (`d-f018-capacity`, ruled: wait; REQ-LM-4, REQ-IN-11): a directed deposit
--     over its route's capacity waits, and leaves the condition by itself once the transfers in
--     flight that do not wait for capacity, plus the directed deposits waiting for it ahead by
--     coordinate, leave room (directed_capacity_fits(), under the route's lock). While it waits it is
--     a transfer in flight, so route_usage_locked()'s count, the ingress's, counts it: first come
--     first served. It is never returned for capacity. A deposit waiting for the window or held by
--     an upgrade of its token carries nothing yet, so neither count holds it in flight (review M1 and
--     M2 of PR #142): it takes its place in flight when it leaves that wait, through
--     directed_capacity_fits() again.
--   * The window's queue claims one window at most (`d-f018-window-queue`; REQ-IN-11, AC-A6.7):
--     route_window_used() is what a request sees of the window. A directed deposit waiting in
--     CND-DepositWindow counts against it only for one window (the route's `window_seconds`) from
--     when it took the condition, and the waiting deposits together never more than the window's
--     limit. directed_window_fits(), the core's question for one directed deposit, still counts every
--     deposit waiting ahead of it by coordinate, however long, so the queue drains in order and what
--     is minted never exceeds the window.
--   * The requests' reserve (the proposed implementation of `d-f018-window-queue`, pending the
--     owner's confirmation as `d-f018-window-reserve`; review M3 of PR #142): once the queue is older
--     than one window it no longer counts against requests, but its head would still take every
--     window's room as it frees, within seconds, before any request came. So a directed deposit that
--     waits behind, or is itself, a deposit that began to wait more than one window ago may leave the
--     window's wait only while what the window has taken, plus every deposit waiting ahead of it,
--     plus its own amount, is within (1 - `routes.request_reserve`) of the window's limit (0.5 by
--     default; 0 restores amendment 0029's rule). One whose amount alone is over that share leaves
--     only when the window holds nothing else, so that it is never stuck. Directed deposits that
--     have waited less than one window are unaffected; the order by coordinate and the window's
--     limit hold as before (directed_window_fits()).
--   * The review edge judged at resolution (`d-f018-review-exit-later`; REQ-RV-2, REQ-RF-1,
--     AC-A6.9): directed_review_refund_open() no longer asks that the review was entered holding
--     CND-DestinationRefused. The newest dry run of attempt 1 (across its orders, a resolution's
--     delivery included; by block, then row) must be the issuing network's refusal of the
--     destination with the asset Live and its block time (directed_refusal_recorded(), 0145, as
--     before), and that refusal must be evidence as the review stands: either the one whose deadline
--     began the review (entered from Source-Final holding CND-DestinationRefused, resting on the
--     destination network's head: the case 0145 allowed), or one whose order was recorded at or after
--     the review began. A refusal from before a review that began for an unknown (a destination or
--     list the core could not read, a preparation that never came, a delivery anomaly) is stale and
--     opens nothing (guardrail 4). Every other gate is 0145's.
--   * The hold on an origin token's upgrade (`d-f018-usdc-upgrade`, ruled: hold and alert;
--     REQ-AS-12, AC-A6.8): a `token_upgrade` observation (0147) of an admitted token, not withdrawn,
--     holds every directed arrival of that token on that network at a block at or after its own
--     (directed_arrival_token_held()); the core holds such a deposit in CND-TokenUpgraded before its
--     mint is ordered, and makes no return of such an arrival, and the store refuses a mint order for
--     a held deposit (intents_guard_token_hold()). An operator lifts it in two steps by two logins,
--     recorded in `admin_actions` as a pause's lift is: operator_request_token_hold_lift() names the
--     upgrade, operator_approve_token_hold_lift() approves it. A requester's deposit is never held.
--
-- Replaced, as their latest migration made them plus these lines: route_usage_locked() (0145),
-- directed_window_fits() (0145), directed_review_refund_open() (0145), admin_actions_by_session() and
-- operator_record() (0091).

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff', 'CND-PayoutWindow',
        'CND-DepositWindow', 'CND-DepositCapacity', 'CND-TokenUpgraded'
    ));

-- The share of a route's window kept for requests from a queue of directed deposits older than one
-- window (directed_window_fits(); `d-f018-window-reserve`, proposed). Route-level, as the window is:
-- 0 gives the queue the whole window again, 1 keeps all of it for requests until the queue is
-- younger than a window.
ALTER TABLE routes ADD COLUMN request_reserve NUMERIC NOT NULL DEFAULT 0.5
    CONSTRAINT routes_request_reserve_share CHECK (request_reserve >= 0 AND request_reserve <= 1);

COMMENT ON COLUMN routes.request_reserve IS
    'REQ-IN-11, REQ-LM-1 (amendment 0031, d-f018-window-queue; proposed as d-f018-window-reserve): '
    'the share of the window''s limit a directed deposit waiting behind a queue older than one window '
    'leaves to requests. 0.5 by default; 0 turns the reserve off.';

-- ---------------------------------------------------------------------------------------------
-- The window, as a request sees it (`d-f018-window-queue`).

-- What a route's trailing window of `p_window_seconds` has used, as a request sees it: every
-- transfer of the route that moved or may move (not Refused, Expired or Withdrawn) and took its share
-- within the window (when it was made, or, if it waited in CND-DepositWindow, when it left the
-- condition), plus the claims of the directed deposits that wait in CND-DepositWindow and began to
-- wait within the window, together no more than the route's window limit. Takes no lock: the
-- ingress's route table reads it as it is, and route_usage_locked() reads it under the route's lock.
-- Runs with the caller's privileges, as route_usage_locked() does.
CREATE FUNCTION route_window_used(
    p_asset TEXT, p_network TEXT, p_direction TEXT, p_window_seconds BIGINT)
    RETURNS TEXT
    LANGUAGE plpgsql
    STABLE
    SET search_path = public, pg_temp
    AS $$
DECLARE
    taken   NUMERIC;
    claimed NUMERIC;
    lim     NUMERIC;
BEGIN
    SELECT COALESCE(sum(t.amount), 0) INTO taken
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn')
      AND t.condition IS DISTINCT FROM 'CND-DepositWindow'
      AND (t.created_at > now() - make_interval(secs => p_window_seconds)
           OR t.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                                WHERE w.condition = 'CND-DepositWindow')))
      AND CASE
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
    -- The claims: a waiting deposit only for one window from when it took the condition.
    SELECT COALESCE(sum(t.amount), 0) INTO claimed
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND t.condition = 'CND-DepositWindow'
      AND t.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                            WHERE w.condition = 'CND-DepositWindow'))
      AND (SELECT min(w.seen_at) FROM transitions w
           WHERE w.transfer_id = t.id AND w.condition = 'CND-DepositWindow')
          > now() - make_interval(secs => p_window_seconds);
    -- Together never more than one window's limit (none declared: no cap to apply).
    SELECT r.window_limit INTO lim FROM routes r
    WHERE r.asset = p_asset AND r.network = p_network AND r.direction = p_direction;
    RETURN (taken + LEAST(claimed, COALESCE(lim, claimed)))::text;
END
$$;

COMMENT ON FUNCTION route_window_used(TEXT, TEXT, TEXT, BIGINT) IS
    'REQ-LM-1, REQ-IN-11 (amendment 0031, d-f018-window-queue): what a route''s trailing window has '
    'used as a request sees it: the shares taken in it, plus the claims of directed deposits that began '
    'to wait for it within the window, together at most one window''s limit.';

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
    used := route_window_used(p_asset, p_network, p_direction, p_window_seconds);
    -- A deposit waiting for the window, or held by an upgrade of its token, carries nothing yet: it
    -- is not in flight (review M1 and M2 of PR #142). One waiting for capacity is, first come first
    -- served.
    SELECT count(*) INTO in_flight
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND NOT is_terminal_state(t.state)
      AND (t.condition IS NULL OR t.condition NOT IN ('CND-DepositWindow', 'CND-TokenUpgraded'));
END
$$;

-- ---------------------------------------------------------------------------------------------
-- The requests' reserve (`d-f018-window-queue`, as proposed for `d-f018-window-reserve`).

-- Whether deposit `p_transfer` fits in what its route's window has left (REQ-LM-1, amendment 0029
-- Open 1): what the other deposits of the route took within the window, plus what every deposit
-- waiting ahead of it by coordinate claims (first come first served: block, then position), plus its
-- own amount, is within the room it may take. That room is the window's limit, unless it, or a
-- deposit waiting ahead of it, began to wait in CND-DepositWindow more than one window ago: then it
-- is (1 - the route's `request_reserve`) of the limit, the rest left to requests (amendment 0031,
-- `d-f018-window-queue`; review M3 of PR #142), or its own amount if that alone is more, which it
-- takes only when the window holds nothing else; never more than the limit. A deposit behind one so
-- bounded is bounded too, so none leaves before one ahead of it. FALSE for a transfer that is not a
-- deposit with a declared route. Takes the route's lock (route_usage_locked), held until the
-- transaction ends, so that the decision and the transition it leads to are serial with every other
-- reading of the window, the ingress's included. Runs with the caller's privileges; the core calls
-- it.
CREATE OR REPLACE FUNCTION directed_window_fits(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE plpgsql
    VOLATILE
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t      transfers%ROWTYPE;
    r      routes%ROWTYPE;
    taken  NUMERIC;
    ahead  NUMERIC;
    old    BOOLEAN;
    room   NUMERIC;
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
    WHERE d.asset = t.asset AND d.kind = 'deposit'
      AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)
          = t.source_network
      AND d.id <> t.id
      AND d.state NOT IN ('Refused', 'Expired', 'Withdrawn')
      AND d.condition IS DISTINCT FROM 'CND-DepositWindow'
      AND (d.created_at > now() - make_interval(secs => r.window_seconds)
           OR d.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                                WHERE w.condition = 'CND-DepositWindow')))
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
    WHERE d.asset = t.asset AND d.kind = 'deposit'
      AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)
          = t.source_network
      AND d.id <> t.id
      AND d.condition = 'CND-DepositWindow'
      AND d.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                            WHERE w.condition = 'CND-DepositWindow'))
      AND ROW(d.source_block_number, d.source_position_index, d.id)
          < ROW(t.source_block_number, t.source_position_index, t.id);
    -- Whether it, or one waiting ahead of it, began to wait more than one window ago.
    SELECT EXISTS (
        SELECT 1 FROM transfers d
        WHERE d.asset = t.asset AND d.kind = 'deposit'
          AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)
              = t.source_network
          AND d.condition = 'CND-DepositWindow'
          AND d.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                                WHERE w.condition = 'CND-DepositWindow'))
          AND ROW(d.source_block_number, d.source_position_index, d.id)
              <= ROW(t.source_block_number, t.source_position_index, t.id)
          AND (SELECT min(w.seen_at) FROM transitions w
               WHERE w.transfer_id = d.id AND w.condition = 'CND-DepositWindow')
              <= now() - make_interval(secs => r.window_seconds)) INTO old;
    room := CASE WHEN old
                 THEN LEAST(r.window_limit,
                            GREATEST((1 - r.request_reserve) * r.window_limit, t.amount))
                 ELSE r.window_limit END;
    RETURN taken + ahead + t.amount <= room;
END
$$;

COMMENT ON FUNCTION directed_window_fits(BIGINT) IS
    'REQ-LM-1, REQ-IN-11 (amendments 0029, Open 1, and 0031, d-f018-window-queue): whether a directed '
    'deposit fits in what its route''s window has left, behind every deposit waiting ahead of it by '
    'coordinate; behind a queue older than one window, only in (1 - request_reserve) of the limit. '
    'Takes the route''s lock until the transaction ends. The core holds CND-DepositWindow while it '
    'does not.';

-- ---------------------------------------------------------------------------------------------
-- Capacity (`d-f018-capacity`).

-- Whether deposit `p_transfer` fits in its route's capacity (REQ-LM-4, REQ-IN-11): the route's
-- other transfers in flight that do not wait (for capacity, for the window, or under the hold of an
-- upgrade of their token: those carry nothing yet), plus the deposits waiting for capacity ahead by
-- coordinate (block, then position, then id), plus itself, are within the declared capacity. FALSE
-- for a transfer that is not a deposit with a declared route. Takes the route's lock
-- (route_usage_locked), held until the transaction ends, so that the decision and the transition it
-- leads to are serial with every other reading of the capacity, the ingress's included. Runs with
-- the caller's privileges; the core calls it.
CREATE FUNCTION directed_capacity_fits(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE plpgsql
    VOLATILE
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t      transfers%ROWTYPE;
    r      routes%ROWTYPE;
    others BIGINT;
    ahead  BIGINT;
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
    SELECT count(*) INTO others
    FROM transfers d
    WHERE d.asset = t.asset AND d.kind = 'deposit'
      AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)
          = t.source_network
      AND d.id <> t.id
      AND NOT is_terminal_state(d.state)
      AND (d.condition IS NULL
           OR d.condition NOT IN ('CND-DepositCapacity', 'CND-DepositWindow', 'CND-TokenUpgraded'));
    SELECT count(*) INTO ahead
    FROM transfers d
    WHERE d.asset = t.asset AND d.kind = 'deposit'
      AND (CASE d.kind WHEN 'deposit' THEN d.source_network ELSE d.destination_network END)
          = t.source_network
      AND d.id <> t.id
      AND d.condition = 'CND-DepositCapacity'
      AND NOT is_terminal_state(d.state)
      AND ROW(d.source_block_number, d.source_position_index, d.id)
          < ROW(t.source_block_number, t.source_position_index, t.id);
    RETURN others + ahead + 1 <= r.capacity;
END
$$;

COMMENT ON FUNCTION directed_capacity_fits(BIGINT) IS
    'REQ-LM-4, REQ-IN-11 (amendment 0031, d-f018-capacity): whether a directed deposit fits in its '
    'route''s capacity, behind every deposit waiting for it ahead by coordinate; takes the route''s lock '
    'until the transaction ends. The core holds CND-DepositCapacity while it does not.';

-- ---------------------------------------------------------------------------------------------
-- The hold on an origin token's upgrade (`d-f018-usdc-upgrade`).

-- The two operator acts that lift it, recorded with every other administrative act (0091).
ALTER TABLE admin_actions DROP CONSTRAINT admin_actions_kind_check;
ALTER TABLE admin_actions ADD CONSTRAINT admin_actions_kind_check CHECK (kind IN (
    'pause', 'unpause-request', 'unpause',
    'key-rotation', 'bound-change', 'route-admission', 'credential',
    'surplus-contribution', 'surplus-withdrawal', 'float-top-up', 'other',
    'token-hold-lift-request', 'token-hold-lift'));
ALTER TABLE admin_actions DROP CONSTRAINT admin_actions_approves_only_unpause;
ALTER TABLE admin_actions ADD CONSTRAINT admin_actions_approves_only_unpause
    CHECK ((kind IN ('unpause', 'token-hold-lift')) = (approves IS NOT NULL));

CREATE OR REPLACE FUNCTION admin_actions_by_session() RETURNS trigger
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
    ELSIF NEW.kind = 'token-hold-lift' THEN
        SELECT * INTO req FROM admin_actions
        WHERE id = NEW.approves AND kind = 'token-hold-lift-request';
        IF NOT FOUND THEN
            RAISE EXCEPTION 'token hold lift refused: it approves no request to lift the hold on an upgrade (REQ-AS-12, REQ-GV-5)'
                USING ERRCODE = 'BZ001';
        END IF;
        IF req.actor = NEW.actor THEN
            RAISE EXCEPTION 'token hold lift refused: the request was made by this login; the approver is another person (REQ-AS-12, REQ-GV-5)'
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION operator_record(p_kind TEXT, p_subject TEXT, p_detail JSONB, p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    rid BIGINT;
BEGIN
    PERFORM operator_require();
    IF p_kind IN ('pause', 'unpause-request', 'unpause', 'token-hold-lift-request', 'token-hold-lift') THEN
        RAISE EXCEPTION 'refused: % is made by its own function, never recorded after the fact (REQ-GV-5)', p_kind
            USING ERRCODE = 'BZ001';
    END IF;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES (p_kind, p_subject, coalesce(p_detail, '{}'::jsonb), p_justification)
        RETURNING id INTO rid;
    RETURN rid;
END
$$;

-- Whether a directed arrival of `p_token` on `p_network` at block `p_block` is held: an upgrade of
-- that token there (a `token_upgrade` observation naming it, by its token column or its detail's,
-- without case on an `evm` network, as addresses compare there) at or before that block, that the
-- network has not withdrawn (an upgrade not yet final already holds: holding is the safe side), and
-- whose hold no approved lift names.
CREATE FUNCTION directed_arrival_token_held(p_network TEXT, p_token TEXT, p_block BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT p_token IS NOT NULL AND EXISTS (
        SELECT 1 FROM observations u
        JOIN networks n ON n.network = u.network
        -- `u.kind IN (...)` restates the predicate of the partial index `observations_paged_kinds`
        -- (0147), which the planner uses only when the query implies it; `u.kind = 'token_upgrade'`
        -- is the filter that matters.
        WHERE u.kind IN ('unpaired_direction', 'token_upgrade') AND u.kind = 'token_upgrade'
          AND u.network = p_network AND u.block_number <= p_block
          AND (CASE WHEN n.address_format = 'evm'
                    THEN lower(COALESCE(u.token, u.detail ->> 'token')) = lower(p_token)
                    ELSE COALESCE(u.token, u.detail ->> 'token') = p_token END)
          AND NOT EXISTS (SELECT 1 FROM observation_outcomes x
                          WHERE x.observation_id = u.id AND x.outcome = 'withdrawn')
          AND NOT EXISTS (SELECT 1 FROM admin_actions a
                          WHERE a.kind = 'token-hold-lift'
                            AND a.detail ->> 'observation_id' = u.id::text))
$$;

COMMENT ON FUNCTION directed_arrival_token_held(TEXT, TEXT, BIGINT) IS
    'REQ-AS-12 (amendment 0031, d-f018-usdc-upgrade): whether an upgrade of the token, at or before the '
    'block, not withdrawn and not lifted by two operators, holds a directed arrival of it.';

-- Whether transfer `p_transfer` is a directed deposit (bound to a `directed_arrival`) that the
-- upgrade of its token holds. A requester's deposit never is.
CREATE FUNCTION directed_token_held(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN observations o
          ON o.network = t.source_network AND o.block_number = t.source_block_number
         AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
         AND o.position_index = t.source_position_index
        WHERE t.id = p_transfer AND t.kind = 'deposit' AND t.refund_of IS NULL
          AND o.kind = 'directed_arrival'
          AND directed_arrival_token_held(o.network, o.token, o.block_number))
$$;

COMMENT ON FUNCTION directed_token_held(BIGINT) IS
    'REQ-AS-12 (amendment 0031): whether a directed deposit is held by an upgrade of its token '
    '(CND-TokenUpgraded). No requester''s deposit is.';

-- Step one of lifting the hold of upgrade `p_observation`: a request, by one login, saying that the
-- checks of the token's behaviour (the fork suite) were run again against the new code. Returns the
-- request's id.
CREATE FUNCTION operator_request_token_hold_lift(p_observation BIGINT, p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    u   observations%ROWTYPE;
    rid BIGINT;
BEGIN
    PERFORM operator_require();
    SELECT * INTO u FROM observations WHERE id = p_observation AND kind = 'token_upgrade';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'token hold lift request refused: observation % is no upgrade of an origin token (REQ-AS-12)', p_observation
            USING ERRCODE = 'BZ001';
    END IF;
    INSERT INTO admin_actions (kind, subject, detail, justification)
        VALUES ('token-hold-lift-request',
                'upgrade of ' || COALESCE(u.token, u.detail ->> 'token', 'unknown token') || ' on ' || u.network,
                jsonb_build_object('observation_id', u.id, 'network', u.network,
                                   'token', COALESCE(u.token, u.detail ->> 'token'),
                                   'implementation', u.detail ->> 'implementation',
                                   'block_number', u.block_number),
                p_justification)
        RETURNING id INTO rid;
    RETURN rid;
END
$$;

-- Step two: another login approves the request, and the hold is lifted. The held directed deposits
-- resume by themselves at the core's next cycle.
CREATE FUNCTION operator_approve_token_hold_lift(p_request BIGINT, p_justification TEXT)
    RETURNS BIGINT
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    req admin_actions%ROWTYPE;
    aid BIGINT;
BEGIN
    PERFORM operator_require();
    SELECT * INTO req FROM admin_actions WHERE id = p_request AND kind = 'token-hold-lift-request';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'token hold lift refused: no such request %', p_request
            USING ERRCODE = 'BZ001';
    END IF;
    -- The trigger refuses the request's own actor, and a request approved twice (UNIQUE).
    INSERT INTO admin_actions (kind, subject, detail, approves, justification)
        VALUES ('token-hold-lift', req.subject, req.detail, req.id, p_justification)
        RETURNING id INTO aid;
    RETURN aid;
END
$$;

-- The store's side of the hold: no mint order is recorded for a held directed deposit, whoever
-- writes it but the migration owner (drills and restores, NFR-7). The core holds it first.
CREATE FUNCTION intents_guard_token_hold() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
BEGIN
    IF NEW.purpose <> 'mint' OR NOT (NEW.payload ? 'order') OR NEW.transfer_id IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF directed_token_held(NEW.transfer_id) THEN
        RAISE EXCEPTION 'mint order for transfer % refused: an upgrade of its origin token holds it until two operators lift the hold (REQ-AS-12, CND-TokenUpgraded)',
            NEW.transfer_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_guard_token_hold
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_token_hold();

-- ---------------------------------------------------------------------------------------------
-- The review edge, judged at resolution (`d-f018-review-exit-later`).

-- Whether directed deposit `p_transfer` may be resolved from its review into Refunding (amendments
-- 0029, Open 2, and 0031): a deposit bound to a directed arrival, in Needs-Review entered from
-- Source-Final, whose newest attempt-1 dry run is the issuing network's refusal of its destination,
-- the asset Live, with its block time (directed_refusal_recorded()), that refusal being the one whose
-- deadline began the review or one ordered at or after the review began; with no signature and no
-- signed bytes recorded for any intent of it, no delivery anomaly recorded for it, and no mint of it
-- observed. The core asks it before it runs a resolution; the store asks it again of every row it
-- writes.
CREATE OR REPLACE FUNCTION directed_review_refund_open(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN observations o
          ON o.network = t.source_network AND o.block_number = t.source_block_number
         AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
         AND o.position_index = t.source_position_index
        JOIN LATERAL (
            SELECT x.from_state, x.condition, x.network, x.seen_at FROM transitions x
            WHERE x.transfer_id = t.id AND x.to_state = 'Needs-Review'
              AND x.from_state <> 'Needs-Review'
            ORDER BY x.id DESC LIMIT 1) e ON TRUE
        LEFT JOIN LATERAL (
            SELECT i.recorded_at FROM intents i
            JOIN prepared_transactions p ON p.order_intent_id = i.id
            WHERE i.transfer_id = t.id AND i.attempt = 1 AND i.purpose = 'mint'
              AND i.network = t.destination_network AND p.network = t.destination_network
            ORDER BY p.dry_run_block_number DESC, p.id DESC LIMIT 1) newest ON TRUE
        WHERE t.id = p_transfer AND t.kind = 'deposit' AND t.refund_of IS NULL
          AND t.state = 'Needs-Review' AND o.kind = 'directed_arrival'
          AND e.from_state = 'Source-Final'
          AND directed_refusal_recorded(t.id, NULL, NULL, NULL, NULL)
          AND ((e.condition = 'CND-DestinationRefused'
                AND e.network IS NOT DISTINCT FROM t.destination_network)
               OR newest.recorded_at >= e.seen_at)
          AND NOT EXISTS (SELECT 1 FROM intents i JOIN signatures s ON s.intent_id = i.id
                          WHERE i.transfer_id = t.id)
          AND NOT EXISTS (SELECT 1 FROM event_log a
                          WHERE a.kind = 'delivery-anomaly-surfaced' AND a.transfer_id = t.id)
          AND NOT EXISTS (
              SELECT 1 FROM event_log a
              WHERE a.kind = 'delivery-anomaly' AND a.payload ->> 'network' = t.destination_network
                AND (EXISTS (SELECT 1 FROM intents i
                             WHERE i.transfer_id = t.id AND i.id::text = a.payload ->> 'intent')
                     OR EXISTS (SELECT 1 FROM intents i JOIN signed_transactions st
                                  ON st.intent_id = i.id
                                WHERE i.transfer_id = t.id
                                  AND st.id::text IN (a.payload ->> 'signed_transaction',
                                                      a.payload ->> 'signed'))))
          AND NOT EXISTS (SELECT 1 FROM intents i JOIN signed_transactions st ON st.intent_id = i.id
                          WHERE i.transfer_id = t.id)
          AND NOT EXISTS (
              SELECT 1 FROM observations m
              WHERE m.network = t.destination_network
                AND (m.on_chain_reference = t.on_chain_reference
                     OR m.on_chain_reference = reference_tagged('bezent/mint-reference/v0', ARRAY[
                            coordinate_digest(t.source_network, t.source_block_number,
                                              t.source_block_hash, t.source_position_kind,
                                              t.source_position_index)]))))
$$;

COMMENT ON FUNCTION directed_review_refund_open(BIGINT) IS
    'REQ-IN-11, REQ-RV-2, REQ-RF-1, INV-8 (amendments 0029, Open 2, and 0031): whether a directed '
    'deposit in review entered from Source-Final, judged on the evidence at resolution (the newest '
    'attempt-1 dry run is the issuing network''s refusal of its destination with the asset Live, either '
    'the one whose deadline began the review or one ordered since it began; never an unknown), with no '
    'mint attempt ever signed, no mint of it observed and no delivery anomaly recorded for it, may be '
    'resolved into Refunding (with a second approver, REQ-GV-5). No other transfer may.';

-- ---------------------------------------------------------------------------------------------
-- Grants. route_window_used() is PUBLIC's, as route_usage_locked() is: it runs with the caller's
-- privileges, and the ingress's route table reads it. The core asks the capacity and the hold; the
-- operator requests and approves a lift; the rest run as a trigger's or a definer's own.

REVOKE EXECUTE ON FUNCTION
    directed_capacity_fits(BIGINT),
    directed_arrival_token_held(TEXT, TEXT, BIGINT),
    directed_token_held(BIGINT),
    operator_request_token_hold_lift(BIGINT, TEXT),
    operator_approve_token_hold_lift(BIGINT, TEXT),
    intents_guard_token_hold()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION directed_capacity_fits(BIGINT), directed_token_held(BIGINT),
    directed_arrival_token_held(TEXT, TEXT, BIGINT)
    TO bezent_core;
GRANT EXECUTE ON FUNCTION operator_request_token_hold_lift(BIGINT, TEXT),
    operator_approve_token_hold_lift(BIGINT, TEXT)
    TO bezent_operator;

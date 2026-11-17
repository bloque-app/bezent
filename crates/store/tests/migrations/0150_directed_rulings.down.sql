-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): amendment 0031's store side, back to 0149's.
DROP TRIGGER intents_guard_token_hold ON intents;
DROP FUNCTION intents_guard_token_hold();
DROP FUNCTION operator_approve_token_hold_lift(BIGINT, TEXT);
DROP FUNCTION operator_request_token_hold_lift(BIGINT, TEXT);
DROP FUNCTION directed_token_held(BIGINT);
DROP FUNCTION directed_arrival_token_held(TEXT, TEXT, BIGINT);
DROP FUNCTION directed_capacity_fits(BIGINT);

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
            SELECT x.from_state, x.condition, x.network FROM transitions x
            WHERE x.transfer_id = t.id AND x.to_state = 'Needs-Review'
              AND x.from_state <> 'Needs-Review'
            ORDER BY x.id DESC LIMIT 1) e ON TRUE
        WHERE t.id = p_transfer AND t.kind = 'deposit' AND t.refund_of IS NULL
          AND t.state = 'Needs-Review' AND o.kind = 'directed_arrival'
          AND e.from_state = 'Source-Final' AND e.condition = 'CND-DestinationRefused'
          AND e.network IS NOT DISTINCT FROM t.destination_network
          AND directed_refusal_recorded(t.id, NULL, NULL, NULL, NULL)
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
      AND (t.created_at > now() - make_interval(secs => p_window_seconds)
           OR t.id = ANY (ARRAY(SELECT w.transfer_id FROM transitions w
                                WHERE w.condition = 'CND-DepositWindow')))
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

DROP FUNCTION route_window_used(TEXT, TEXT, TEXT, BIGINT);

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
    RETURN taken + ahead + t.amount <= r.window_limit;
END
$$;

COMMENT ON FUNCTION directed_window_fits(BIGINT) IS
    'REQ-LM-1, REQ-IN-11 (amendment 0029, Open 1): whether a directed deposit fits in what its '
    'route''s window has left, behind every deposit waiting ahead of it by coordinate; takes the '
    'route''s lock until the transaction ends. The core holds CND-DepositWindow while it does not.';

ALTER TABLE routes DROP COLUMN request_reserve;

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

ALTER TABLE admin_actions DROP CONSTRAINT admin_actions_approves_only_unpause;
ALTER TABLE admin_actions ADD CONSTRAINT admin_actions_approves_only_unpause
    CHECK ((kind = 'unpause') = (approves IS NOT NULL));
ALTER TABLE admin_actions DROP CONSTRAINT admin_actions_kind_check;
ALTER TABLE admin_actions ADD CONSTRAINT admin_actions_kind_check CHECK (kind IN (
    'pause', 'unpause-request', 'unpause',
    'key-rotation', 'bound-change', 'route-admission', 'credential',
    'surplus-contribution', 'surplus-withdrawal', 'float-top-up', 'other'));

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff', 'CND-PayoutWindow',
        'CND-DepositWindow'
    ));

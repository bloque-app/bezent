-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-002, review of PR #22: three additive changes to what the ingress records and reads.
--
--   * intake_answers: a transfer is answered to at most one request (REQ-ID-4, REQ-RQ-4). A repeat
--     inserts no answer, a refused request names no transfer it did not make, so the only way to
--     a second answer is a second request reaching the same withdrawal: its extras are written
--     once, and the store now refuses the second link as well as the ingress;
--   * transfers.token: a refund of an unassociated arrival whose token is not admitted
--     (REQ-RQ-6, REQ-IN-6) names no bridged asset. It names the arrival's origin token instead,
--     and its amount is in that token's minor units on its source network;
--   * the route usage the intake reads under the route lock (REQ-LM-1, REQ-LM-3, REQ-LM-4): one
--     call that takes the lock and then reads, with a snapshot taken after the lock, the window
--     used and the transfers in flight, each from an index bounded by what it counts (the
--     window, the transfers not terminal) rather than by the route's whole history.

CREATE UNIQUE INDEX intake_answers_transfer_once
    ON intake_answers (transfer_id) WHERE transfer_id IS NOT NULL;
DROP INDEX intake_answers_transfer;

ALTER TABLE transfers ALTER COLUMN asset DROP NOT NULL;
ALTER TABLE transfers ADD COLUMN token TEXT CHECK (token <> '');
ALTER TABLE transfers ADD CONSTRAINT transfers_asset_or_token CHECK (
    (asset IS NOT NULL AND token IS NULL)
    OR (asset IS NULL AND token IS NOT NULL
        AND kind = 'refund' AND refund_of IS NULL AND source_network IS NOT NULL)
);

COMMENT ON COLUMN transfers.token IS
    'REQ-RQ-6, REQ-IN-6: the origin token of an unassociated arrival whose token is not admitted, '
    'set only on its refund, which names no bridged asset. The amount is then an exact integer '
    'count of that token''s minor units on the source network (INV-11).';

-- REQ-ST-7: what the transfer was asked never changes (transfers_guard_update covers the other
-- columns; the token is new here).
CREATE FUNCTION transfers_token_unchanged() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION 'transfer %: its token never changes; what a transfer was asked is fixed (REQ-ST-7)',
        OLD.bridge_reference
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER transfers_token_unchanged
    BEFORE UPDATE ON transfers
    FOR EACH ROW WHEN (NEW.token IS DISTINCT FROM OLD.token)
    EXECUTE FUNCTION transfers_token_unchanged();

-- A route's transfers are those of its asset and kind on its origin network: the source network
-- of a deposit, the destination network of a withdrawal.
DROP INDEX transfers_route_created;
CREATE INDEX transfers_route_window
    ON transfers (asset, kind,
                  (CASE kind WHEN 'deposit' THEN source_network ELSE destination_network END),
                  created_at)
    WHERE asset IS NOT NULL;
CREATE INDEX transfers_route_in_flight
    ON transfers (asset, kind,
                  (CASE kind WHEN 'deposit' THEN source_network ELSE destination_network END))
    WHERE asset IS NOT NULL AND NOT is_terminal_state(state);

-- Takes the route's lock for the rest of the transaction, then reads what its window has used
-- (transfers that did not end with nothing moved, created within the trailing window) and how
-- many of its transfers are not terminal. VOLATILE: each read takes its snapshot after the lock,
-- so it sees every transfer committed by the request that held the lock before.
CREATE FUNCTION route_usage_locked(
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
      AND t.created_at > now() - make_interval(secs => p_window_seconds)
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn');
    SELECT count(*) INTO in_flight
    FROM transfers t
    WHERE t.asset = p_asset AND t.kind = p_direction
      AND (CASE t.kind WHEN 'deposit' THEN t.source_network ELSE t.destination_network END) = p_network
      AND NOT is_terminal_state(t.state);
END
$$;

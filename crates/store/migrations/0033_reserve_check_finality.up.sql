-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-004, third review round of PR #40 (SF-A, SF-B, SF-C). Additive to 0032.
--
--   * transfers_refuse_funding_source(): a refund of an unassociated arrival (REQ-RQ-6) never
--     binds an arrival from a declared funding address either (SF-A; INV-1, INV-2, REQ-IN-3).
--   * networks.finality_seconds: the network's time to its own finality, configured per network.
--     The lag bound defaults to it plus one minute, capped at an hour, so a network that finalizes
--     slowly is not paused for finalizing (SF-B; NFR-5, REQ-IC-2).
--   * The lag bound against the route budgets runs on insert as well, of a network and of a route,
--     so any order of setup is checked (SF-C; NFR-5, NFR-6, REQ-IC-2).

-- ---------------------------------------------------------------------------------------------
-- A refund of an unassociated arrival never binds an operator top-up (SF-A).

CREATE OR REPLACE FUNCTION transfers_refuse_funding_source() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT (NEW.kind = 'deposit' OR (NEW.kind = 'refund' AND NEW.refund_of IS NULL))
       OR NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE'
       AND NOT (OLD.source_final_network IS NULL AND NEW.source_final_network IS NOT NULL) THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index
          AND funding_arrival(NEW.asset, o.network, o.sender, o.block_number)) THEN
        RAISE EXCEPTION '% refused: the arrival at its source coordinate is from an operator funding address, a surplus contribution no deposit or refund binds (INV-1, INV-2, REQ-IN-3)',
            NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- The lag bound defaults to the network's finality time plus a minute (SF-B).

ALTER TABLE networks
    ADD COLUMN finality_seconds INTEGER NOT NULL DEFAULT 0
        CHECK (finality_seconds >= 0 AND finality_seconds <= 3600);

COMMENT ON COLUMN networks.finality_seconds IS
    'NFR-5, REQ-IC-2: how long, in seconds, the network takes to its own finality: the most the '
    'latest final head''s block time lags the clock when heads arrive on time. Configured per '
    'network. The lag bound defaults to this plus 60 s, at most an hour. 0 (the default) is a '
    'network that finalizes within the block time; a network that finalizes slower MUST set it.';

ALTER TABLE networks ALTER COLUMN reserve_point_lag_seconds DROP DEFAULT;

-- An unset bound is the network's finality time plus a minute, at most an hour. Re-derived when
-- only the finality time changes; an explicit bound is the operator's, within the ceiling and the
-- route budgets (0032).
CREATE FUNCTION networks_derive_lag_bound() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.reserve_point_lag_seconds IS NULL THEN
            NEW.reserve_point_lag_seconds := LEAST(NEW.finality_seconds + 60, 3600);
        END IF;
    ELSIF NEW.finality_seconds IS DISTINCT FROM OLD.finality_seconds
          AND NEW.reserve_point_lag_seconds IS NOT DISTINCT FROM OLD.reserve_point_lag_seconds THEN
        NEW.reserve_point_lag_seconds := LEAST(NEW.finality_seconds + 60, 3600);
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER networks_a_derive_lag_bound
    BEFORE INSERT OR UPDATE OF finality_seconds ON networks
    FOR EACH ROW EXECUTE FUNCTION networks_derive_lag_bound();

COMMENT ON COLUMN networks.reserve_point_lag_seconds IS
    'NFR-5, REQ-IC-2: the most, in block time, the final point the reserve check reads this network '
    'at may lag its latest recorded final head, and the most the latest recorded final head may lag '
    'the clock. Unless set, finality_seconds plus 60 s. At most an hour, and set no higher than the '
    'completion budget of any route the network serves. Beyond it the evaluation diverges; an '
    'evaluation that cannot be made for longer pauses too.';

-- ---------------------------------------------------------------------------------------------
-- The lag bound against the route budgets, whatever the order of setup (SF-C).

DROP TRIGGER networks_reserve_point_lag_within_budget ON networks;
CREATE TRIGGER networks_reserve_point_lag_within_budget
    BEFORE INSERT OR UPDATE OF reserve_point_lag_seconds, finality_seconds ON networks
    FOR EACH ROW EXECUTE FUNCTION reserve_point_lag_within_budget();

-- A route's budget is held against the bound of the network it runs on and of the issuing
-- network, which serves every route.
CREATE FUNCTION routes_budget_covers_lag_bound() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    over RECORD;
BEGIN
    SELECT n.network, n.reserve_point_lag_seconds AS bound INTO over FROM networks n
    WHERE (n.network = NEW.network OR n.role = 'issuing')
      AND n.reserve_point_lag_seconds > NEW.completion_budget_seconds
    ORDER BY n.network LIMIT 1;
    IF FOUND THEN
        RAISE EXCEPTION 'route budget refused: % s is below the lag bound % s of % (NFR-5, NFR-6, REQ-IC-2)',
            NEW.completion_budget_seconds, over.bound, over.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER routes_budget_covers_lag_bound
    BEFORE INSERT OR UPDATE OF completion_budget_seconds, network ON routes
    FOR EACH ROW EXECUTE FUNCTION routes_budget_covers_lag_bound();

-- ---------------------------------------------------------------------------------------------
-- Grants. The trigger functions fire without an EXECUTE grant.

REVOKE EXECUTE ON FUNCTION networks_derive_lag_bound() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION routes_budget_covers_lag_bound() FROM PUBLIC;

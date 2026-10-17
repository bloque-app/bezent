-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): the third review fixes of F-004 (refunds never bind a
-- funding arrival, the lag bound from the finality time, the budget rule on insert).
DROP TRIGGER routes_budget_covers_lag_bound ON routes;
DROP FUNCTION routes_budget_covers_lag_bound();
DROP TRIGGER networks_reserve_point_lag_within_budget ON networks;
CREATE TRIGGER networks_reserve_point_lag_within_budget
    BEFORE UPDATE OF reserve_point_lag_seconds ON networks
    FOR EACH ROW EXECUTE FUNCTION reserve_point_lag_within_budget();
DROP TRIGGER networks_a_derive_lag_bound ON networks;
DROP FUNCTION networks_derive_lag_bound();
ALTER TABLE networks ALTER COLUMN reserve_point_lag_seconds SET DEFAULT 60;
ALTER TABLE networks DROP COLUMN finality_seconds;
CREATE OR REPLACE FUNCTION transfers_refuse_funding_source() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.kind <> 'deposit' OR NEW.source_network IS NULL THEN
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
        RAISE EXCEPTION 'deposit refused: the arrival at its source coordinate is from an operator funding address, a surplus contribution no deposit binds (INV-1, INV-2, REQ-IN-3)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

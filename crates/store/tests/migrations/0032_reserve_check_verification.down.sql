-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): the verification fixes of F-004 (funding arrivals never
-- bound, declarations from their block, the lag bound's ceiling, the evaluation's book cut).
ALTER TABLE reserve_evaluations DROP COLUMN book_entry_cut;
DROP TRIGGER networks_reserve_point_lag_within_budget ON networks;
DROP FUNCTION reserve_point_lag_within_budget();
ALTER TABLE networks DROP CONSTRAINT networks_reserve_point_lag_at_most_an_hour;
DROP TRIGGER transfers_source_not_funding ON transfers;
DROP FUNCTION transfers_refuse_funding_source();
DROP FUNCTION funding_arrival(TEXT, TEXT, TEXT, BIGINT);
DROP TRIGGER surplus_funding_addresses_from_block ON surplus_funding_addresses;
DROP FUNCTION surplus_funding_from_block();
ALTER TABLE surplus_funding_addresses DROP COLUMN from_block;

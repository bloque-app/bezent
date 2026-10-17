-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03).
DROP TABLE transitions;
DROP TABLE transfers;
DROP FUNCTION transitions_apply();
DROP FUNCTION transfers_log_created();
DROP FUNCTION transfers_guard_update();
DROP FUNCTION transfers_guard_insert();
DROP FUNCTION transfer_edge_needs_coordinate(transfer_state, transfer_state);

-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): production never runs a down migration.
DROP FUNCTION refuse_append_only_change();
DROP FUNCTION is_terminal_state(transfer_state);
DROP DOMAIN refusal_error;
DROP DOMAIN transfer_condition;
DROP DOMAIN transfer_state;
DROP DOMAIN position_kind;
DROP DOMAIN minor_units;

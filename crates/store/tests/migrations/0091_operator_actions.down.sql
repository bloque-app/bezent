-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): F-014, the operator's recorded actions and pause.
DROP FUNCTION operator_record(TEXT, TEXT, JSONB, TEXT);
DROP FUNCTION operator_approve_unpause(BIGINT, TEXT);
DROP FUNCTION operator_request_unpause(BIGINT, TEXT);
DROP FUNCTION operator_pause(TEXT, TEXT, TEXT, TEXT);
DROP FUNCTION operator_require();
DROP TABLE admin_actions;
DROP FUNCTION admin_actions_by_session();

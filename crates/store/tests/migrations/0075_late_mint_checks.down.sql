-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #26, the late-mint checks.
DROP INDEX event_log_anomaly_by_signed_transaction;
DROP INDEX observations_by_signed_transaction;
DROP TRIGGER observation_outcomes_late_mint_enqueue ON observation_outcomes;
DROP FUNCTION late_mint_enqueue();
DROP TABLE late_mint_checks;

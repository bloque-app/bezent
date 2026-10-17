-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03).
DROP TRIGGER observation_outcomes_guard_writer ON observation_outcomes;
DROP FUNCTION observation_outcomes_guard_writer();
DROP TRIGGER observations_guard_writer ON observations;
DROP FUNCTION observations_guard_writer();
DROP TRIGGER event_log_guard_signal ON event_log;
DROP FUNCTION event_log_guard_signal();
DROP FUNCTION bezent_writer_holds(TEXT, OID);

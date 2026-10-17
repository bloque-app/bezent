-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): the review of PR #56.
DROP FUNCTION bezent_applied_migrations();
DROP TRIGGER event_log_guard_writer ON event_log;
DROP FUNCTION event_log_guard_writer();
DROP TRIGGER transfers_source_verified_final ON transfers;
DROP FUNCTION transfers_source_verified_final();
DROP TRIGGER transitions_guard_writer ON transitions;
DROP FUNCTION transitions_guard_writer();

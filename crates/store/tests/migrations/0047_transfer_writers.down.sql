-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): review M1b of PR #56.
DROP TRIGGER transfers_guard_writer ON transfers;
DROP FUNCTION transfers_guard_writer();

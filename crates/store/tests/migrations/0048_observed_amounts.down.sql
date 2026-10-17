-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): review M1c, S7 and S8 of PR #56. The guard 0048 replaced,
-- transfers_guard_writer, is dropped by 0047's reversal.
DROP TRIGGER transfers_sourced_observed_amount ON transfers;
DROP TRIGGER transfers_amount_from_observation ON transfers;
DROP FUNCTION transfers_observed_amount();

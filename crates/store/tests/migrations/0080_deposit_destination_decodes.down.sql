-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #45, a deposit's destination decodes when it is bound.
DROP TRIGGER transitions_guard_writer_destination_decodes ON transitions;
DROP TRIGGER transfers_guard_writer_destination_decodes ON transfers;
DROP FUNCTION transfers_guard_writer_destination_decodes();

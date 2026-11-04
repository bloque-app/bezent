-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #26, the held cycles of the late-mint checks.
ALTER TABLE late_mint_checks DROP COLUMN held_cycles;

-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): the approval's record of the output it approved. Dropping the columns
-- drops the operator's grant on them.
ALTER TABLE manual_approvals
    DROP CONSTRAINT manual_approvals_output_whole,
    DROP COLUMN approved_output_network,
    DROP COLUMN approved_output_block_number,
    DROP COLUMN approved_output_block_hash,
    DROP COLUMN approved_output_position_kind,
    DROP COLUMN approved_output_position_index,
    DROP COLUMN approved_output_amount;

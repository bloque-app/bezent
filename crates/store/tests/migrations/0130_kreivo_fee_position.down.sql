-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #14, the Kreivo endpoint's fee position. 0110's grants and comment return.
REVOKE SELECT, INSERT ON fee_position_readings FROM bezent_kreivo_endpoint;
COMMENT ON TABLE fee_position_readings IS
    'REQ-ST-6, AC-H3.1, DEC-18: the native balance of a network''s delivery account at a final head '
    'its endpoint recorded. NULL amount is unknown, with its reason. Append-only; only the '
    'network''s endpoint records it.';

-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #41's CND-PayoutWindow.
ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff'
    ));

-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #41 (F-003 follow-up): CND-PayoutWindow joins the named conditions (amendment 0012, SPEC
-- v0.5.2 §10). A withdrawal whose payout the reserve's outflow allowance cannot take this window
-- holds it in its current state, by a self-transition, and resumes by itself when the allowance
-- refills; nothing is refunded or failed because of it.
--
-- Additive: the domain's check gains one value; no row is touched. The forced non-execution of
-- REQ-NX-4 needs no change to the store: a cancel is an order of purpose `cancel` the EVM endpoint
-- already prepares (F-009), recorded by the core with no transfer, no reference and no amount, so
-- attempt_proof() and attempts_unproven() still read only the payout's own signed transactions.

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion', 'CND-CutOff', 'CND-PayoutWindow'
    ));

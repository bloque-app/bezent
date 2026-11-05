-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #26 (second review of PR #72): the late-mint check no longer ends its wait on an
-- undecided attempt when the observer's final head passes the attempt's era. That head is written
-- by the observer and the supply reader, the attempt is decided by the prover, a separate worker,
-- and a head that is ahead of the prover is not a decision (a legitimate retry included in the
-- last block of its era, or a catch-up over old history, raised a global pause).
-- The row now waits on the prover's own record (a proof, an inclusion, an anomaly) and is bounded
-- by a page instead of a raise: this counts the cycles it waited with the attempt's era over, and
-- the core pages once when the count reaches its bound (event `late-mint-prover-silent`).
-- Additive: one column on the work table of 0075 and the core's right to update it.

ALTER TABLE late_mint_checks
    ADD COLUMN held_cycles INTEGER NOT NULL DEFAULT 0 CHECK (held_cycles >= 0);

COMMENT ON COLUMN late_mint_checks.held_cycles IS
    'How many cycles the core looked at this mint and found an attempt of its reference with no '
    'record from the prover (no proof, inclusion or anomaly) though its era is over; a page is '
    'raised when it passes the core''s bound. Never raises the alert.';

GRANT UPDATE (held_cycles) ON late_mint_checks TO bezent_core;

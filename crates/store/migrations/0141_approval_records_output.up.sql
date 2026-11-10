-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #10 (F-010), amendment 0018 Open 6, chat ruling of 2026-11-06 by @pandres95: the approval of a
-- resolution records the exact conversion output it approved. A resolution approved before the
-- output was named, or for another output than the deposit's, does not run; a new approval is needed.
--
-- The approver supplies the output (its coordinate and amount): it is what they reviewed, so only they can say
-- which output they approved. The store does not stamp it from what the deposit holds when the
-- row is inserted, which would bind an output named between the approver's look and the insert.
-- An approval that names none records none. The columns are all set or all null. The core
-- compares them with the deposit's current source and amount before it orders the mint, and refuses
-- when none was recorded or when they differ. The comparison is on the output's identity, never on
-- two clocks (the previous check compared approved_at with recorded_at, both now(), which races).
--
-- The operator's insert grant on manual_approvals (0015) widens by these six columns. The trigger
-- that fills in the approver and the time is unchanged.

ALTER TABLE manual_approvals
    ADD COLUMN approved_output_network        TEXT,
    ADD COLUMN approved_output_block_number   BIGINT,
    ADD COLUMN approved_output_block_hash     BYTEA,
    ADD COLUMN approved_output_position_kind  position_kind,
    ADD COLUMN approved_output_position_index BIGINT,
    ADD COLUMN approved_output_amount         minor_units,
    ADD CONSTRAINT manual_approvals_output_whole CHECK (
        num_nonnulls(approved_output_network, approved_output_block_number, approved_output_block_hash,
                     approved_output_position_kind, approved_output_position_index,
                     approved_output_amount) IN (0, 6));

COMMENT ON COLUMN manual_approvals.approved_output_network IS
    'REQ-GV-5, REQ-CV-2 (amendment 0018, Open 6): the conversion output the approver reviewed and approved '
    '(network, block, hash, position and amount; NULL when the approval names none), supplied by the approver. '
    'A resolution into Delivering runs only against the same output.';


GRANT INSERT (approved_output_network, approved_output_block_number, approved_output_block_hash,
              approved_output_position_kind, approved_output_position_index, approved_output_amount)
    ON manual_approvals TO bezent_operator;

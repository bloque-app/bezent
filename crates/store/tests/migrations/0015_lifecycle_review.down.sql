-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (F-003, review of PR #32). The roles are shared by every database on the
-- server and stay.
REVOKE INSERT ON final_heads FROM bezent_endpoint;
REVOKE USAGE ON SCHEMA public FROM bezent_endpoint;
GRANT INSERT ON final_heads TO bezent_service;

REVOKE INSERT (action_id, justification) ON manual_approvals FROM bezent_operator;
REVOKE INSERT (kind, transfer_id, to_state, network, block_number, block_hash, block_time,
               justification)
    ON manual_actions FROM bezent_operator;
GRANT INSERT ON manual_actions, manual_approvals TO bezent_operator;
DROP TRIGGER manual_approvals_by_session ON manual_approvals;
DROP TRIGGER manual_actions_by_session ON manual_actions;
DROP FUNCTION manual_approvals_by_session();
DROP FUNCTION manual_actions_by_session();
COMMENT ON COLUMN manual_approvals.approver IS NULL;
COMMENT ON COLUMN manual_actions.actor IS NULL;
ALTER TABLE manual_approvals ALTER COLUMN approver DROP DEFAULT;
ALTER TABLE manual_actions ALTER COLUMN actor DROP DEFAULT;

DROP TRIGGER transitions_refund_every_attempt_proven ON transitions;
DROP FUNCTION transitions_refund_every_attempt_proven();
DROP TRIGGER signatures_attempt_set_open ON signatures;
DROP FUNCTION signatures_attempt_set_open();
DROP TRIGGER intents_guard_attempt ON intents;
CREATE TRIGGER intents_guard_attempt
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_attempt();
COMMENT ON FUNCTION intents_guard_attempt() IS NULL;
DROP FUNCTION intents_guard_attempt_set();
DROP FUNCTION attempts_unproven(BIGINT);
DROP FUNCTION transfer_attempts_closed(BIGINT);

DROP INDEX observations_reference_event;
DROP TRIGGER transfers_destination_reference_unchanged ON transfers;
DROP FUNCTION transfers_destination_reference_unchanged();
DROP INDEX transfers_destination_reference_once;
ALTER TABLE transfers DROP CONSTRAINT transfers_destination_reference_of_a_withdrawal;
ALTER TABLE transfers DROP COLUMN destination_reference;

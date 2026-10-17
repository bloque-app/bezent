-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (F-003 T-003-01). The roles are shared by every database on the server and stay.
REVOKE ALL ON manual_actions, manual_approvals, reviews, unassociated_arrivals FROM bezent_operator;
REVOKE USAGE ON SCHEMA public FROM bezent_operator;
DROP VIEW unassociated_arrivals;
DROP VIEW reviews;
DROP FUNCTION one_business_day_after(TIMESTAMPTZ);

DROP TRIGGER transitions_apply ON transitions;
CREATE TRIGGER transitions_apply
    AFTER INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_apply();
COMMENT ON FUNCTION transitions_apply() IS NULL;
DROP FUNCTION transitions_apply_lifecycle();
DROP FUNCTION resolution_moves_value(transfer_state);
DROP FUNCTION transfer_edge(transfer_state, transfer_state);
COMMENT ON FUNCTION transfer_edge_needs_coordinate(transfer_state, transfer_state) IS NULL;

DROP TABLE attempt_milestones;
DROP FUNCTION attempt_milestones_guard_insert();
DROP INDEX transitions_manual_action_once;
ALTER TABLE transitions DROP CONSTRAINT transitions_attempt_only_into_delivering;
ALTER TABLE transitions DROP COLUMN manual_action_id;
ALTER TABLE transitions DROP COLUMN attempt;

DROP TABLE manual_approvals;
DROP TABLE manual_actions;
DROP FUNCTION manual_approvals_second_person();

DROP INDEX observations_reference;
DROP INDEX transfers_open;
DROP INDEX transfers_on_chain_reference_consumed_once_per_namespace;
CREATE UNIQUE INDEX transfers_on_chain_reference_consumed_once
    ON transfers (on_chain_reference)
    WHERE on_chain_reference IS NOT NULL
      AND refund_of IS NULL
      AND state NOT IN ('Refused', 'Expired', 'Withdrawn');
DROP TRIGGER transfers_redeemer_unchanged ON transfers;
DROP FUNCTION transfers_redeemer_unchanged();
ALTER TABLE transfers DROP CONSTRAINT transfers_redeemer_of_a_redemption;
ALTER TABLE transfers DROP COLUMN redeemer;

DROP TRIGGER signatures_from_intent ON signatures;
CREATE TRIGGER signatures_from_intent
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_from_intent();
COMMENT ON FUNCTION signatures_from_intent() IS NULL;
DROP FUNCTION signatures_from_intent_attempt();
DROP INDEX signatures_once_per_reference_attempt;
CREATE UNIQUE INDEX signatures_once_per_reference
    ON signatures (on_chain_reference) WHERE on_chain_reference IS NOT NULL;
ALTER TABLE signatures DROP COLUMN attempt;
ALTER TABLE signatures DROP COLUMN reference_scope;
ALTER TABLE signatures DROP COLUMN purpose;
ALTER TABLE signatures DROP COLUMN network;

DROP TRIGGER intents_guard_attempt ON intents;
DROP FUNCTION intents_guard_attempt();
DROP FUNCTION attempt_gate_open(TEXT, TEXT, TEXT, BYTEA, INTEGER);
DROP FUNCTION attempt_proof(BIGINT, INTEGER);
DROP INDEX intents_lane;
DROP INDEX intents_transfer_attempt;
ALTER TABLE intents DROP COLUMN reference_scope;
ALTER TABLE intents DROP COLUMN attempt;

DROP TABLE final_heads;

COMMENT ON COLUMN routes.cutoffs IS NULL;
ALTER TABLE networks DROP COLUMN backend;
ALTER TABLE assets DROP COLUMN issuing_asset;

ALTER DOMAIN transfer_condition DROP CONSTRAINT transfer_condition_check;
ALTER DOMAIN transfer_condition ADD CONSTRAINT transfer_condition_check
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion'
    ));

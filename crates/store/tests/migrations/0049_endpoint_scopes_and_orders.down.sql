-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): the write-path audit of PR #56.
REVOKE SELECT (seq) ON event_log FROM bezent_signer;
REVOKE SELECT (id, intent_id) ON signed_transactions FROM bezent_signer;
REVOKE SELECT ON admitted_tokens FROM bezent_signer;
DROP INDEX signatures_by_key_network_nonce;
ALTER TABLE signatures DROP COLUMN asset;
ALTER TABLE signatures DROP COLUMN nonce;

DROP TRIGGER transitions_guard_destination ON transitions;
DROP TRIGGER transfers_guard_destination ON transfers;
DROP FUNCTION transfers_guard_destination();
DROP FUNCTION transfer_destination_refusal(transfers);

REVOKE INSERT ON event_log FROM bezent_signer;
CREATE OR REPLACE FUNCTION event_log_guard_signal() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.kind IN ('key-compromise', 'pause-requested', 'delivery-anomaly')
       AND NOT (bezent_writer_holds('bezent_endpoint', TG_RELID)
                OR pg_has_role(current_user, 'bezent_core', 'USAGE')) THEN
        RAISE EXCEPTION '% refused: only an endpoint or the core records it (AC-I1.2, REQ-IC-4)', NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
DROP INDEX event_log_one_key_compromise_per_detection;
DROP TRIGGER event_log_b_stamp_key_compromise ON event_log;
DROP FUNCTION event_log_stamp_key_compromise();

DROP TRIGGER transitions_guard_id ON transitions;
DROP FUNCTION transitions_guard_id();

DROP TRIGGER intents_guard_order ON intents;
DROP FUNCTION intents_guard_order();
DROP FUNCTION bezent_final_source(BIGINT);

DROP TRIGGER non_execution_proofs_endpoint_scope ON non_execution_proofs;
DROP TRIGGER signed_transactions_endpoint_scope ON signed_transactions;
DROP TRIGGER prepared_transactions_endpoint_scope ON prepared_transactions;
DROP TRIGGER reserve_readings_endpoint_scope ON reserve_readings;
DROP TRIGGER final_heads_endpoint_scope ON final_heads;
DROP TRIGGER observation_ranges_endpoint_scope ON observation_ranges;
DROP TRIGGER observation_outcomes_endpoint_scope ON observation_outcomes;
DROP TRIGGER observations_endpoint_scope ON observations;
DROP FUNCTION endpoint_guard_scope();

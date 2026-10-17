-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03). The roles, and their memberships, are shared by every
-- database on the server and stay.
DROP INDEX intents_one_signing_intent_per_prepared;
CREATE OR REPLACE FUNCTION non_execution_proofs_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM signed_transactions st JOIN intents i ON i.id = st.intent_id
                   WHERE st.id = NEW.signed_transaction_id AND i.network = NEW.network) THEN
        RAISE EXCEPTION 'proof for signed transaction % refused: no such signed transaction on % (REQ-NX-2)',
            NEW.signed_transaction_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;
REVOKE ALL ON non_execution_proofs, prepared_transactions, observation_ranges FROM bezent_endpoint;
REVOKE USAGE ON SCHEMA public FROM bezent_endpoint;
GRANT INSERT ON non_execution_proofs, prepared_transactions, observation_ranges TO bezent_service;

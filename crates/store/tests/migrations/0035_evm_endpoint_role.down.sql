-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03). The role, and its membership, are shared by every database
-- on the server and stay.
DELETE FROM endpoint_scopes WHERE endpoint_role = 'bezent_evm_endpoint';
DROP TRIGGER observations_evm_endpoint_guard ON observations;
DROP TRIGGER observation_outcomes_evm_endpoint_guard ON observation_outcomes;
DROP TRIGGER observation_ranges_evm_endpoint_guard ON observation_ranges;
DROP TRIGGER final_heads_evm_endpoint_guard ON final_heads;
DROP TRIGGER prepared_transactions_evm_endpoint_guard ON prepared_transactions;
DROP TRIGGER signed_transactions_evm_endpoint_guard ON signed_transactions;
DROP TRIGGER non_execution_proofs_evm_endpoint_guard ON non_execution_proofs;
DROP FUNCTION evm_endpoint_guard_network();
DROP FUNCTION bezent_row_network(OID, JSONB);
DROP FUNCTION bezent_evm_endpoint_writer(OID);
REVOKE USAGE ON SCHEMA public FROM bezent_evm_endpoint;

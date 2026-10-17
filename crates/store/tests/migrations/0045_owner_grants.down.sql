-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): issues #35, #33, #46. The roles and their memberships are
-- shared by every database on the server and stay; the grants of 0045 go, and the shared roles'
-- grants of 0009 to 0044 come back.
GRANT EXECUTE ON FUNCTION event_log_assign_seq(), transfers_log_created(), transitions_apply()
    TO PUBLIC;

REVOKE SELECT ON
    admitted_tokens, assets, attempt_milestones, endpoint_scopes, event_log, event_log_head,
    final_heads, intake, intake_answers, manual_actions, manual_approvals, networks,
    non_execution_proofs, observation_outcomes, observation_ranges, observations,
    prepared_transactions, relayed_instructions, reserve_readings, reserves, routes, signatures,
    signed_transactions, surplus_funding_addresses, transfers, transitions,
    book_balances, event_log_position, observation_gaps, observation_watermarks, reviews,
    transfer_status, unassociated_arrivals
    FROM bezent_core;
REVOKE INSERT ON transfers, transitions, event_log FROM bezent_core;
REVOKE UPDATE (on_chain_reference) ON transfers FROM bezent_core;
REVOKE EXECUTE ON FUNCTION one_business_day_after(TIMESTAMPTZ), bezent_writer_holds(TEXT, OID)
    FROM bezent_core;

REVOKE ALL ON
    admitted_tokens, assets, attempt_milestones, event_log, event_log_head, intake,
    intake_answers, networks, observation_outcomes, observations, pauses, relayed_instructions,
    requester_credentials, requester_grants, requesters, reserves, routes,
    surplus_funding_addresses, transfers, transitions,
    book_balances, event_log_position, ledger_accounts_listed, transfer_status, webhook_deliveries
    FROM bezent_ingress;
REVOKE SELECT ON webhook_secrets, webhook_transfer_urls, webhook_condition_urls FROM bezent_ingress;
REVOKE EXECUTE ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    funding_arrival(TEXT, TEXT, TEXT, BIGINT), bezent_writer_holds(TEXT, OID)
    FROM bezent_ingress;
REVOKE USAGE ON SCHEMA public FROM bezent_ingress;

REVOKE ALL ON
    admitted_tokens, assets, endpoint_scopes, event_log, final_heads, intents, networks,
    non_execution_proofs, observation_outcomes, observation_ranges, observations,
    prepared_transactions, reserve_readings, reserves, signatures, signed_transactions,
    observation_gaps, observation_watermarks
    FROM bezent_kreivo_endpoint, bezent_evm_endpoint;
REVOKE EXECUTE ON FUNCTION
    bezent_writer_holds(TEXT, OID), bezent_endpoint_writer(OID), bezent_evm_endpoint_writer(OID),
    bezent_row_network(OID, JSONB), observation_kind_on_network(TEXT, TEXT),
    observation_kind_for_event(TEXT), observation_writer_covers(TEXT, OID)
    FROM bezent_kreivo_endpoint, bezent_evm_endpoint;

GRANT USAGE ON SCHEMA public TO bezent_service, bezent_endpoint;
GRANT SELECT ON
    admitted_tokens, assets, attempt_milestones, book_balances, book_entries, book_postings,
    book_quarantine, endpoint_scopes, event_log, event_log_head, event_log_position, final_heads,
    intake, intake_answers, intents, ledger_accounts, ledger_accounts_listed, manual_actions,
    manual_approvals, networks, non_execution_proofs, observation_gaps, observation_outcomes,
    observation_ranges, observation_watermarks, observations, pauses, prepared_transactions,
    relayed_instructions, requester_credentials, requester_grants, requesters, reserve_evaluations,
    reserve_readings, reserve_statement_abandoned, reserve_statements, reserves, reviews, routes,
    signatures, signed_transactions, surplus_funding_addresses, transfer_status, transfers,
    transitions, unassociated_arrivals, webhook_deliveries
    TO bezent_service;
GRANT SELECT (id, requester, created_at) ON webhook_secrets TO bezent_service;
GRANT SELECT (id, requester, transfer_id, url_digest, previous_digest, after_seq, registered_at)
    ON webhook_transfer_urls TO bezent_service;
GRANT SELECT (id, requester, registered_at) ON webhook_condition_urls TO bezent_service;
GRANT INSERT ON event_log, intake, intake_answers, relayed_instructions, transfers, transitions
    TO bezent_service;
GRANT UPDATE (refund_destination_network, refund_destination, expires_at,
              source_network, source_block_number, source_block_hash,
              source_position_kind, source_position_index, on_chain_reference)
    ON transfers TO bezent_service;
GRANT EXECUTE ON FUNCTION
    bezent_writer_holds(TEXT, OID), bezent_endpoint_writer(OID), bezent_evm_endpoint_writer(OID),
    bezent_row_network(OID, JSONB), observation_kind_on_network(TEXT, TEXT),
    observation_kind_for_event(TEXT), observation_writer_covers(TEXT, OID),
    source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    funding_arrival(TEXT, TEXT, TEXT, BIGINT), one_business_day_after(TIMESTAMPTZ)
    TO bezent_service;
GRANT INSERT ON
    final_heads, non_execution_proofs, observation_outcomes, observation_ranges, observations,
    prepared_transactions, reserve_readings, signed_transactions
    TO bezent_endpoint;

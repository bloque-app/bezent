-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03): F-004's reserve check, statements and book columns.
REVOKE INSERT (asset, cause, evaluation_id) ON pauses FROM bezent_core;
REVOKE SELECT ON pauses FROM bezent_core;
REVOKE SELECT, INSERT ON book_entries, book_postings FROM bezent_core;
GRANT INSERT ON book_entries, book_postings TO bezent_service;
DROP TABLE reserve_statements;
DROP FUNCTION reserve_statements_guard_insert();
DROP TRIGGER pauses_guard_insert ON pauses;
DROP FUNCTION pauses_guard_insert();
ALTER TABLE pauses DROP COLUMN evaluation_id, DROP COLUMN cause;
DROP TABLE reserve_evaluations;
ALTER TABLE assets DROP COLUMN surplus_alert, DROP COLUMN surplus_floor;
DROP INDEX book_postings_asset_account;
ALTER TABLE book_postings DROP COLUMN item;
DROP INDEX book_entries_observation;
DROP INDEX book_entries_movement_once;
ALTER TABLE book_entries
    DROP COLUMN transfer_id, DROP COLUMN observation_id, DROP COLUMN block_hash,
    DROP COLUMN block_number, DROP COLUMN network, DROP COLUMN kind;

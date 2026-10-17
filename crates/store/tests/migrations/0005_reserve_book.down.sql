-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03).
DROP TABLE book_postings;
DROP TABLE book_entries;
DROP FUNCTION book_entry_balanced();
DROP FUNCTION book_postings_same_transaction();
DROP FUNCTION book_entries_stamp();

-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #49's declared ledger accounts.
DROP VIEW ledger_accounts_listed;
DROP TABLE ledger_accounts;
DROP FUNCTION ledger_accounts_canonical();
DROP FUNCTION ledger_accounts_base58(TEXT);

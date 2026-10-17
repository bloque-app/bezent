-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03).
DROP TRIGGER intents_guard_prepared ON intents;
DROP TRIGGER prepared_transactions_guard_order ON prepared_transactions;
DROP FUNCTION intents_guard_prepared();
DROP FUNCTION prepared_transactions_guard_order();
DROP FUNCTION bezent_endpoint_writer(OID);

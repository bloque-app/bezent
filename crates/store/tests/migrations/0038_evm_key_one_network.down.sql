-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03).
DROP TRIGGER signatures_one_network_per_evm_key ON signatures;
DROP TRIGGER prepared_transactions_one_network_per_evm_lane ON prepared_transactions;
DROP FUNCTION signatures_one_network_per_evm_key();
DROP FUNCTION prepared_transactions_one_network_per_evm_lane();

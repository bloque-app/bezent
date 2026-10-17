-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03). The roles are shared by every database on the server and stay.
DROP TABLE signatures;
DROP FUNCTION signatures_from_intent();
REVOKE ALL ON intents FROM bezent_signer;
REVOKE USAGE ON SCHEMA public FROM bezent_signer;
REVOKE ALL ON intents FROM bezent_core;
REVOKE USAGE ON SCHEMA public FROM bezent_core;
GRANT INSERT ON intents TO bezent_service;

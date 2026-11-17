-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): F-018, T-018-10, what the public status route reads. The role
-- is the server's, shared by every database on it, and stays, holding nothing here.
DROP VIEW public_status;
REVOKE EXECUTE ON FUNCTION bezent_applied_migrations() FROM bezent_public;
REVOKE USAGE ON SCHEMA public FROM bezent_public;

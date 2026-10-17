-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Reversal for tests only (T-001-03). The role is shared by every database on the server and stays.
REVOKE ALL ON ALL TABLES IN SCHEMA public FROM bezent_service;
REVOKE USAGE ON SCHEMA public FROM bezent_service;
DO $$
BEGIN
    EXECUTE format('GRANT TEMPORARY ON DATABASE %I TO PUBLIC', current_database());
END
$$;

-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (F-002, review of PR #22).
DROP FUNCTION route_usage_locked(TEXT, TEXT, TEXT, BIGINT);
DROP INDEX transfers_route_in_flight;
DROP INDEX transfers_route_window;
CREATE INDEX transfers_route_created ON transfers (asset, kind, created_at);
DROP TRIGGER transfers_token_unchanged ON transfers;
DROP FUNCTION transfers_token_unchanged();
ALTER TABLE transfers DROP CONSTRAINT transfers_asset_or_token;
ALTER TABLE transfers DROP COLUMN token;
ALTER TABLE transfers ALTER COLUMN asset SET NOT NULL;
CREATE INDEX intake_answers_transfer ON intake_answers (transfer_id) WHERE transfer_id IS NOT NULL;
DROP INDEX intake_answers_transfer_once;

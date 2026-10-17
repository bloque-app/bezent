-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #2's webhooks. The dispatcher's role is shared by every
-- database on the server and stays; its grants here go.
REVOKE SELECT ON event_log, event_log_head, transfers, intake, intake_answers, requester_grants
    FROM bezent_webhooks;
REVOKE USAGE ON SCHEMA public FROM bezent_webhooks;
DROP TABLE webhook_deliveries;
DROP TABLE webhook_condition_holds;
DROP TABLE webhook_cursor;
DROP TABLE webhook_condition_urls;
DROP TABLE webhook_transfer_urls;
DROP TABLE webhook_secrets;
DROP FUNCTION webhook_dispatcher_only();
DROP FUNCTION webhook_ingress_only();
DROP FUNCTION webhook_deliveries_guard();
DROP FUNCTION webhook_cursor_guard();
DROP FUNCTION webhook_transfer_urls_guard();

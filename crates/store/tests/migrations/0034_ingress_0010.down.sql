-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #30 (refusal errors, observation kinds, the endpoints'
-- scopes, the binding rule). The role bezent_kreivo_endpoint, and its membership, are shared by
-- every database on the server and stay.
DROP TRIGGER transfers_source_bindable ON transfers;
DROP FUNCTION transfers_source_bindable();
DROP FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP TRIGGER observation_outcomes_guard_scope ON observation_outcomes;
DROP FUNCTION observation_outcomes_guard_scope();
DROP TRIGGER observations_guard_kind ON observations;
DROP FUNCTION observations_guard_kind();
DROP FUNCTION observation_writer_covers(TEXT, OID);
DROP TABLE endpoint_scopes;
REVOKE USAGE ON SCHEMA public FROM bezent_kreivo_endpoint;
ALTER TABLE observations DROP COLUMN kind;
DROP FUNCTION observation_kind_for_event(TEXT);
DROP FUNCTION observation_kind_on_network(TEXT, TEXT);
DROP DOMAIN observation_kind;
ALTER DOMAIN refusal_error DROP CONSTRAINT refusal_error_check;
ALTER DOMAIN refusal_error ADD CONSTRAINT refusal_error_check
    CHECK (VALUE IN (
        'ERR-NotAuthorised', 'ERR-ReferenceConflict', 'ERR-UnknownRoute', 'ERR-RoutePaused',
        'ERR-BelowMinimum', 'ERR-AboveMaximum', 'ERR-WindowExceeded', 'ERR-ExcessPrecision',
        'ERR-SourceNotFound', 'ERR-SourceMismatch', 'ERR-SourceConsumed',
        'ERR-InvalidDestination', 'ERR-Overloaded'
    ));

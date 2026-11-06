-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): amendment 0026 (`d-requester-arrivals`), the declared senders and
-- the binding guard on a deposit's answer. The named errors are restored as 0034 made them.
DROP TRIGGER intake_answers_guard_sender ON intake_answers;
DROP FUNCTION intake_answers_guard_sender();
DROP FUNCTION sender_declared(TEXT, TEXT, TEXT);
DROP TABLE requester_senders;

ALTER DOMAIN refusal_error DROP CONSTRAINT refusal_error_check;
ALTER DOMAIN refusal_error ADD CONSTRAINT refusal_error_check
    CHECK (VALUE IN (
        'ERR-NotAuthorised', 'ERR-ReferenceConflict', 'ERR-UnknownRoute', 'ERR-RoutePaused',
        'ERR-BelowMinimum', 'ERR-AboveMaximum', 'ERR-WindowExceeded', 'ERR-ExcessPrecision',
        'ERR-SourceNotFound', 'ERR-SourceMismatch', 'ERR-SourceConsumed',
        'ERR-InvalidDestination', 'ERR-Overloaded', 'ERR-Malformed', 'ERR-CommitmentMismatch'
    ));

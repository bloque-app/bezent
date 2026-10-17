-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-03: durable intake (REQ-RQ-1). Every request is written here before it is answered.
--
-- A repeat of a request reference is answered from the original row (REQ-ID-1), so the reference
-- is unique per requester: references are scoped to their requester (PLAN DEC-15), so one
-- requester never learns, or collides with, another's (REQ-ID-4). The row is what bezent was asked
-- (REQ-ST-7), and no acknowledged request may be lost (NFR-7): the table is append-only.

CREATE TABLE intake (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    request_reference TEXT NOT NULL CHECK (request_reference <> ''),
    requester         TEXT NOT NULL CHECK (requester <> ''),
    -- The request kinds of SPEC §7.A (REQ-RQ-3, REQ-RQ-4, REQ-RQ-6; REQ-RQ-2 is withdrawn by
    -- amendment 0007).
    kind              TEXT NOT NULL CHECK (kind IN ('deposit', 'withdrawal', 'refund')),
    body              JSONB NOT NULL,
    -- Digest of the canonical body, compared on a repeat (REQ-ID-1).
    body_digest       BYTEA NOT NULL CHECK (length(body_digest) > 0),
    received_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT intake_reference_once_per_requester UNIQUE (requester, request_reference)
);

CREATE TRIGGER intake_append_only
    BEFORE UPDATE OR DELETE ON intake
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-ST-7, NFR-7');
CREATE TRIGGER intake_no_truncate
    BEFORE TRUNCATE ON intake
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-7, NFR-7');

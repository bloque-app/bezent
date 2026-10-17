-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-03/04: signer intents and their recorded bytes (SPEC §8.3 REQ-EO-1, INV-9;
-- PLAN DEC-3, DEC-4).
--
-- The core records an intent before the signer signs anything; the exact signed bytes are stored
-- before they are submitted, and re-sent or queried after a restart, never re-signed until the
-- first is proven not executed (REQ-EO-1). Both tables are append-only: a new signing attempt is a
-- new row, never an overwrite.

CREATE TABLE intents (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id        BIGINT REFERENCES transfers (id),
    network            TEXT NOT NULL CHECK (network <> ''),
    purpose            TEXT NOT NULL CHECK (purpose <> ''),
    on_chain_reference BYTEA CHECK (length(on_chain_reference) > 0),
    -- What the core asks the signer to sign.
    payload            JSONB NOT NULL,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE signed_transactions (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    intent_id   BIGINT NOT NULL REFERENCES intents (id),
    attempt     INTEGER NOT NULL CHECK (attempt >= 1),
    -- The exact signed transaction (REQ-EO-1).
    bytes       BYTEA NOT NULL CHECK (length(bytes) > 0),
    tx_hash     BYTEA CHECK (length(tx_hash) > 0),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT signed_transactions_attempt_once UNIQUE (intent_id, attempt)
);

CREATE TRIGGER intents_append_only
    BEFORE UPDATE OR DELETE ON intents
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');
CREATE TRIGGER intents_no_truncate
    BEFORE TRUNCATE ON intents
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');
CREATE TRIGGER signed_transactions_append_only
    BEFORE UPDATE OR DELETE ON signed_transactions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');
CREATE TRIGGER signed_transactions_no_truncate
    BEFORE TRUNCATE ON signed_transactions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-9, REQ-EO-1');

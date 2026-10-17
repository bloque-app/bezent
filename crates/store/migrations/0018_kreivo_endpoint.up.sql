-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-007 T-007-02 to T-007-04: what the Kreivo endpoint records besides observations and signed
-- bytes (SPEC §8.1 REQ-OB-3, §8.2 REQ-NX-1/2, §8.3 REQ-EO-1/4; PLAN DEC-5).
--
-- The endpoint decides nothing (DEC-3): every row here is a fact read from the chain or a
-- transaction it built, never a transfer state. All three tables are append-only.
--
--   * observation_ranges: the block ranges an endpoint read, and the ones it failed to read (a
--     visible gap, REQ-OB-3, AC-D2.3). A read range never starts past the network's contiguous
--     watermark, so no endpoint can move past a range it failed to read: the store refuses it.
--   * prepared_transactions: a transaction the endpoint built for an order the core recorded, with
--     its signing lane and nonce (REQ-EO-4), its expiry (REQ-HN-3), and the dry run it passed or
--     failed at a final block. The core records the signing intent from it (INV-9: only the core
--     records intents); the endpoint asks for a signature only for one whose dry run passed.
--   * non_execution_proofs: a proof that a signed transaction was not executed, by coordinates
--     anyone can check from public chain data (REQ-NX-1, REQ-NX-2, AC-C2.1).

CREATE TABLE observation_ranges (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network            TEXT NOT NULL CHECK (network <> ''),
    first_block        BIGINT NOT NULL CHECK (first_block >= 0),
    last_block         BIGINT NOT NULL,
    outcome            TEXT NOT NULL CHECK (outcome IN ('read', 'gap')),
    -- The final point the range was read under, or failed at (REQ-OB-5: reads at final blocks).
    final_block_number BIGINT NOT NULL,
    final_block_hash   BYTEA NOT NULL CHECK (length(final_block_hash) > 0),
    -- Why a gap is one: the network's own error text, never a guess (REQ-NX-3).
    reason             TEXT,
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (last_block >= first_block),
    CHECK (final_block_number >= last_block),
    CHECK ((outcome = 'gap') = (reason IS NOT NULL AND reason <> ''))
);

CREATE INDEX observation_ranges_by_network ON observation_ranges (network, outcome, last_block);

-- REQ-OB-3: a read range starts at or below the block after the network's contiguous watermark
-- (the highest block every range up to which was read). Moving past a range that was not read is
-- refused here, whatever the endpoint's code does.
CREATE FUNCTION observation_ranges_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    watermark BIGINT;
BEGIN
    IF NEW.outcome = 'read' THEN
        PERFORM pg_advisory_xact_lock(hashtextextended('observation_ranges:' || NEW.network, 0));
        SELECT max(last_block) INTO watermark
            FROM observation_ranges WHERE network = NEW.network AND outcome = 'read';
        IF watermark IS NOT NULL AND NEW.first_block > watermark + 1 THEN
            RAISE EXCEPTION 'range % to % on % starts past the watermark % (REQ-OB-3)',
                NEW.first_block, NEW.last_block, NEW.network, watermark
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observation_ranges_guard_insert
    BEFORE INSERT ON observation_ranges
    FOR EACH ROW EXECUTE FUNCTION observation_ranges_guard_insert();

-- How far each network is read without a hole, and its gaps. A gap is open until the watermark
-- reaches its last block (AC-D2.3: filled before the bridge moves past it); nothing after the
-- watermark may be decided (REQ-OB-3).
CREATE VIEW observation_watermarks AS
    SELECT network, max(last_block) AS watermark
    FROM observation_ranges WHERE outcome = 'read'
    GROUP BY network;

CREATE VIEW observation_gaps AS
    SELECT g.id, g.network, g.first_block, g.last_block, g.reason, g.recorded_at,
           coalesce(w.watermark >= g.last_block, false) AS filled
    FROM observation_ranges g
    LEFT JOIN observation_watermarks w ON w.network = g.network
    WHERE g.outcome = 'gap';

CREATE TABLE prepared_transactions (
    id                  BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The order the core recorded (a mint or a refund mint).
    order_intent_id     BIGINT NOT NULL REFERENCES intents (id),
    attempt             INTEGER NOT NULL CHECK (attempt >= 1),
    network             TEXT NOT NULL CHECK (network <> ''),
    -- The signing lane: the sender whose nonce sequence the transaction takes (REQ-EO-4).
    lane                BYTEA NOT NULL CHECK (length(lane) > 0),
    key_id              TEXT NOT NULL CHECK (key_id <> ''),
    nonce               BIGINT NOT NULL CHECK (nonce >= 0),
    -- The expiry (REQ-HN-3): valid in blocks valid_from_block to valid_until_block, inclusive;
    -- born at the final block whose hash the transaction binds.
    valid_from_block    BIGINT NOT NULL CHECK (valid_from_block >= 0),
    valid_until_block   BIGINT NOT NULL,
    checkpoint_hash     BYTEA NOT NULL CHECK (length(checkpoint_hash) > 0),
    -- The exact call, the network's signing payload, and what the key signs (the payload, or its
    -- digest where the network signs one).
    call                BYTEA NOT NULL CHECK (length(call) > 0),
    signing_payload     BYTEA NOT NULL CHECK (length(signing_payload) > 0),
    message             BYTEA NOT NULL CHECK (length(message) > 0),
    -- The dry run, at a final block (DEC-5: a dry run before every signature).
    dry_run_block_number BIGINT NOT NULL,
    dry_run_block_hash  BYTEA NOT NULL CHECK (length(dry_run_block_hash) > 0),
    dry_run_passed      BOOLEAN NOT NULL,
    dry_run             JSONB NOT NULL,
    recorded_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (valid_until_block >= valid_from_block),
    CONSTRAINT prepared_transactions_attempt_once UNIQUE (order_intent_id, attempt)
);

CREATE INDEX prepared_transactions_by_lane ON prepared_transactions (network, lane, nonce);

CREATE TABLE non_execution_proofs (
    id                    BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The signed transaction it proves (checked by the trigger below: a foreign key would make a
    -- TRUNCATE of signed_transactions fail on the reference before its append-only refusal).
    signed_transaction_id BIGINT NOT NULL UNIQUE,
    -- What anyone needs to check it (REQ-NX-2, AC-C2.1): network, signer, nonce, expiry, and the
    -- final block that proves it.
    network               TEXT NOT NULL CHECK (network <> ''),
    signer                BYTEA NOT NULL CHECK (length(signer) > 0),
    nonce                 BIGINT NOT NULL CHECK (nonce >= 0),
    valid_from_block      BIGINT NOT NULL,
    valid_until_block     BIGINT NOT NULL,
    final_block_number    BIGINT NOT NULL,
    final_block_hash      BYTEA NOT NULL CHECK (length(final_block_hash) > 0),
    -- The signer's nonce at the final block.
    nonce_at_final        BIGINT NOT NULL CHECK (nonce_at_final >= 0),
    -- nonce-unconsumed: the window ended at a final block, and the nonce was not consumed.
    -- nonce-consumed-by-other: the nonce was consumed at block `at`, by a transaction that is not
    --   this one (its hash is not in that block).
    -- refused: this transaction was included at `at`, position `at_position_index`, and the
    --   network refused it (REQ-NX-1: an on-chain refusal counts once it is final).
    kind                  TEXT NOT NULL CHECK (kind IN ('nonce-unconsumed', 'nonce-consumed-by-other', 'refused')),
    at_block_number       BIGINT,
    at_block_hash         BYTEA,
    at_position_index     BIGINT,
    detail                JSONB NOT NULL DEFAULT '{}'::jsonb,
    recorded_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (valid_until_block >= valid_from_block),
    CHECK (kind <> 'nonce-unconsumed'
           OR (final_block_number >= valid_until_block AND nonce_at_final <= nonce
               AND at_block_number IS NULL)),
    CHECK (kind <> 'nonce-consumed-by-other'
           OR (at_block_number IS NOT NULL AND at_block_hash IS NOT NULL
               AND final_block_number >= at_block_number AND nonce_at_final > nonce)),
    CHECK (kind <> 'refused'
           OR (at_block_number IS NOT NULL AND at_block_hash IS NOT NULL
               AND at_position_index IS NOT NULL AND final_block_number >= at_block_number
               AND at_block_number BETWEEN valid_from_block AND valid_until_block))
);

-- A proof answers a signed transaction of its own network (REQ-NX-2).
CREATE FUNCTION non_execution_proofs_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM signed_transactions st JOIN intents i ON i.id = st.intent_id
                   WHERE st.id = NEW.signed_transaction_id AND i.network = NEW.network) THEN
        RAISE EXCEPTION 'proof for signed transaction % refused: no such signed transaction on % (REQ-NX-2)',
            NEW.signed_transaction_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER non_execution_proofs_guard_insert
    BEFORE INSERT ON non_execution_proofs
    FOR EACH ROW EXECUTE FUNCTION non_execution_proofs_guard_insert();

CREATE TRIGGER observation_ranges_append_only
    BEFORE UPDATE OR DELETE ON observation_ranges
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-OB-3');
CREATE TRIGGER observation_ranges_no_truncate
    BEFORE TRUNCATE ON observation_ranges
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-OB-3');
CREATE TRIGGER prepared_transactions_append_only
    BEFORE UPDATE OR DELETE ON prepared_transactions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-EO-1, REQ-EO-4');
CREATE TRIGGER prepared_transactions_no_truncate
    BEFORE TRUNCATE ON prepared_transactions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-EO-1, REQ-EO-4');
CREATE TRIGGER non_execution_proofs_append_only
    BEFORE UPDATE OR DELETE ON non_execution_proofs
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-NX-1, REQ-NX-2');
CREATE TRIGGER non_execution_proofs_no_truncate
    BEFORE TRUNCATE ON non_execution_proofs
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-NX-1, REQ-NX-2');

REVOKE ALL ON observation_ranges, prepared_transactions, non_execution_proofs,
    observation_watermarks, observation_gaps FROM PUBLIC;
GRANT SELECT, INSERT ON observation_ranges, prepared_transactions, non_execution_proofs
    TO bezent_service;
GRANT SELECT ON observation_watermarks, observation_gaps TO bezent_service;

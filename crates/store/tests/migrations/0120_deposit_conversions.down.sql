-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): issue #10, amendment 0018 (A1b). The functions and the view
-- 0120 replaced are restored as 0003, 0014, 0034 and 0048 made them.
DROP TRIGGER intake_answers_guard_conversion ON intake_answers;
DROP FUNCTION intake_answers_guard_conversion();
DROP INDEX intake_answers_transfer_once;
CREATE UNIQUE INDEX intake_answers_transfer_once
    ON intake_answers (transfer_id) WHERE transfer_id IS NOT NULL;
ALTER TABLE intake_answers DROP COLUMN conversion;

CREATE OR REPLACE VIEW unassociated_arrivals AS
SELECT
    o.id                               AS observation_id,
    o.network,
    o.block_number,
    o.block_hash,
    o.block_time,
    o.position_kind,
    o.position_index,
    o.token,
    o.amount,
    o.recipient,
    a.asset                            AS admitted_for,
    x.decided_at                       AS final_at,
    'operations'::text                 AS owner,
    one_business_day_after(x.decided_at) AS due_at
FROM observations o
JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final'
JOIN networks n ON n.network = o.network AND n.role = 'origin'
JOIN reserves r ON r.network = o.network AND r.address = o.recipient
LEFT JOIN admitted_tokens a ON a.network = o.network AND a.token = o.token
WHERE o.executed
  AND NOT EXISTS (
    SELECT 1 FROM transfers t
    WHERE t.source_network = o.network AND t.source_block_number = o.block_number
      AND t.source_block_hash = o.block_hash AND t.source_position_kind = o.position_kind
      AND t.source_position_index = o.position_index
      AND t.state NOT IN ('Refused', 'Expired', 'Withdrawn'));

CREATE OR REPLACE FUNCTION source_bindable(
    p_purpose TEXT, p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind,
    p_index BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT COALESCE((
        SELECT o.executed
           AND o.token IS NOT NULL AND o.amount IS NOT NULL AND o.recipient IS NOT NULL
           AND o.on_chain_reference IS NULL
           AND EXISTS (SELECT 1 FROM networks n WHERE n.network = o.network AND n.role = 'origin')
           AND EXISTS (SELECT 1 FROM reserves r
                       WHERE r.network = o.network AND r.address = o.recipient)
           AND CASE p_purpose
               WHEN 'deposit' THEN
                   o.kind = 'conversion_output'
                   OR (o.kind = 'arrival'
                       AND NOT (o.sender IS NOT NULL
                                AND EXISTS (SELECT 1 FROM reserves r WHERE r.address = o.sender))
                       AND NOT (o.sender IS NOT NULL AND EXISTS (
                            SELECT 1 FROM routes v WHERE v.venue_addresses @> to_jsonb(o.sender))))
               WHEN 'refund' THEN
                   o.kind IN ('arrival', 'other_arrival', 'conversion_output')
               ELSE FALSE
           END
        FROM observations o
        WHERE o.network = p_network AND o.block_number = p_block AND o.block_hash = p_hash
          AND o.position_kind = p_kind AND o.position_index = p_index
    ), FALSE)
$$;

CREATE OR REPLACE FUNCTION transfers_source_bindable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    purpose TEXT;
    binding BOOLEAN;
    finalising BOOLEAN;
    observed BOOLEAN;
BEGIN
    IF NEW.kind = 'deposit' THEN
        purpose := 'deposit';
    ELSIF NEW.kind = 'refund' AND NEW.refund_of IS NULL THEN
        purpose := 'refund';
    ELSE
        RETURN NEW;
    END IF;
    IF NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    binding := TG_OP = 'INSERT' OR OLD.source_network IS NULL;
    finalising := NEW.source_final_network IS NOT NULL
                  AND (TG_OP = 'INSERT' OR OLD.source_final_network IS NULL);
    IF NOT (binding OR finalising) THEN
        RETURN NEW;
    END IF;
    observed := EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index);
    IF finalising AND NOT observed
       AND EXISTS (SELECT 1 FROM networks WHERE network = NEW.source_network AND role = 'origin') THEN
        RAISE EXCEPTION '% refused: its source on % becomes final with no observation at its coordinate (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, INV-2)',
            NEW.kind, NEW.source_network
            USING ERRCODE = 'BZ001';
    END IF;
    IF observed
       AND NOT source_bindable(purpose, NEW.source_network, NEW.source_block_number,
                               NEW.source_block_hash, NEW.source_position_kind,
                               NEW.source_position_index) THEN
        RAISE EXCEPTION '% refused: the observation at its source coordinate is not of a kind a % may name (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, INV-2)',
            NEW.kind, NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION transfers_observed_amount() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    filling BOOLEAN := TG_ARGV[0] = 'fill';
    o       RECORD;
    family  TEXT;
BEGIN
    IF NOT (NEW.kind = 'deposit' OR (NEW.kind = 'refund' AND NEW.refund_of IS NULL))
       OR NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT ob.network, ob.token, ob.amount
    INTO o
    FROM observations ob
    WHERE ob.network = NEW.source_network AND ob.block_number = NEW.source_block_number
      AND ob.block_hash = NEW.source_block_hash
      AND ob.position_kind = NEW.source_position_kind
      AND ob.position_index = NEW.source_position_index;
    IF NOT FOUND THEN
        -- Bound before it was observed (the ingress refuses that, ERR-SourceNotFound): checked
        -- again when the transfer becomes Source-Final, which needs the observation (0046).
        RETURN NEW;
    END IF;
    SELECT k.asset INTO family
    FROM admitted_tokens k WHERE k.network = o.network AND k.token = o.token;

    IF filling THEN
        IF NEW.amount IS NULL THEN
            NEW.amount := o.amount;
        END IF;
        IF NEW.asset IS NULL AND NEW.token IS NULL THEN
            IF family IS NOT NULL THEN
                NEW.asset := family;
            ELSIF NEW.kind = 'refund' THEN
                NEW.token := o.token;
            END IF;
        END IF;
        RETURN NEW;
    END IF;

    IF o.token IS NULL OR o.amount IS NULL THEN
        RAISE EXCEPTION 'transfer % refused: its source observation carries no token or no amount; a deposit and a refund take theirs from it (REQ-IN-4, REQ-RQ-7)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    -- The amount: the observation's, in the origin token's minor units (the book unit, INV-11).
    IF NEW.amount IS DISTINCT FROM o.amount THEN
        RAISE EXCEPTION 'transfer % refused: amount % is not the % its source observation carries; a transfer''s amount is observed, never a request''s (REQ-IN-4, REQ-CV-2, REQ-RQ-7)',
            NEW.bridge_reference, NEW.amount, o.amount
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.kind = 'deposit' THEN
        -- The same token family (REQ-IN-4): the bridged asset the observed token is admitted for.
        IF family IS NULL THEN
            RAISE EXCEPTION 'deposit % refused: token % on % is not admitted for any bridged asset (REQ-IN-4, REQ-IN-6)',
                NEW.bridge_reference, o.token, o.network
                USING ERRCODE = 'BZ001';
        END IF;
        IF NEW.asset IS DISTINCT FROM family OR NEW.token IS NOT NULL THEN
            RAISE EXCEPTION 'deposit % refused: asset % is not %, the family of the observed token % (REQ-IN-4, REQ-RQ-7)',
                NEW.bridge_reference, coalesce(NEW.asset, NEW.token), family, o.token
                USING ERRCODE = 'BZ001';
        END IF;
    -- The refund of an unassociated arrival names the observed token's asset, or the token itself
    -- (REQ-IN-6; amendment 0010).
    ELSIF NOT ((NEW.token IS NULL AND family IS NOT NULL AND NEW.asset = family)
               OR (NEW.asset IS NULL AND NEW.token = o.token)) THEN
        RAISE EXCEPTION 'refund % refused: % is not the observed token % nor its asset (REQ-IN-4, REQ-IN-6, REQ-RQ-7)',
            NEW.bridge_reference, coalesce(NEW.asset, NEW.token), o.token
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION transfers_guard_update() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF is_terminal_state(OLD.state) THEN
        RAISE EXCEPTION 'transfer % is % (terminal); a terminal state never changes (INV-5)',
            OLD.bridge_reference, OLD.state
            USING ERRCODE = 'BZ001';
    END IF;

    IF ROW(NEW.id, NEW.bridge_reference, NEW.request_reference, NEW.kind, NEW.asset, NEW.amount,
           NEW.destination_network, NEW.destination, NEW.refund_of, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.id, OLD.bridge_reference, OLD.request_reference, OLD.kind, OLD.asset, OLD.amount,
           OLD.destination_network, OLD.destination, OLD.refund_of, OLD.created_at) THEN
        RAISE EXCEPTION 'transfer %: what it was asked never changes (REQ-ST-7)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF num_nonnulls(OLD.refund_destination_network, OLD.refund_destination) > 0
       AND ROW(NEW.refund_destination_network, NEW.refund_destination)
           IS DISTINCT FROM ROW(OLD.refund_destination_network, OLD.refund_destination) THEN
        RAISE EXCEPTION 'transfer %: the refund destination is write-once (REQ-RF-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.expires_at IS NOT NULL AND NEW.expires_at IS DISTINCT FROM OLD.expires_at THEN
        RAISE EXCEPTION 'transfer %: the expiry is write-once (REQ-RQ-4)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.source_network IS NOT NULL
       AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
               NEW.source_position_kind, NEW.source_position_index)
           IS DISTINCT FROM
           ROW(OLD.source_network, OLD.source_block_number, OLD.source_block_hash,
               OLD.source_position_kind, OLD.source_position_index) THEN
        RAISE EXCEPTION 'transfer %: the source binding is write-once (INV-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    IF OLD.on_chain_reference IS NOT NULL
       AND NEW.on_chain_reference IS DISTINCT FROM OLD.on_chain_reference THEN
        RAISE EXCEPTION 'transfer %: the on-chain reference is write-once (INV-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    -- The state, the condition and the milestones change only inside transitions_apply(), the
    -- trigger of an appended transition (nesting depth 2). The service role holds no UPDATE
    -- privilege on these columns either (0009_roles), so it cannot reach this point.
    IF ROW(NEW.state, NEW.condition,
           NEW.source_final_network, NEW.source_final_block_number, NEW.source_final_block_hash,
           NEW.source_final_block_time, NEW.source_final_seen_at,
           NEW.destination_included_network, NEW.destination_included_block_number,
           NEW.destination_included_block_hash, NEW.destination_included_block_time,
           NEW.destination_included_seen_at,
           NEW.destination_final_network, NEW.destination_final_block_number,
           NEW.destination_final_block_hash, NEW.destination_final_block_time,
           NEW.destination_final_seen_at)
       IS DISTINCT FROM
       ROW(OLD.state, OLD.condition,
           OLD.source_final_network, OLD.source_final_block_number, OLD.source_final_block_hash,
           OLD.source_final_block_time, OLD.source_final_seen_at,
           OLD.destination_included_network, OLD.destination_included_block_number,
           OLD.destination_included_block_hash, OLD.destination_included_block_time,
           OLD.destination_included_seen_at,
           OLD.destination_final_network, OLD.destination_final_block_number,
           OLD.destination_final_block_hash, OLD.destination_final_block_time,
           OLD.destination_final_seen_at)
       AND pg_trigger_depth() < 2 THEN
        RAISE EXCEPTION 'transfer %: the state, the condition and the milestones change only by appending a transition (REQ-TR-2)',
            OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    -- A milestone, once written, never changes (REQ-TR-2: no recorded time is overwritten).
    IF (OLD.source_final_network IS NOT NULL
        AND ROW(NEW.source_final_network, NEW.source_final_block_number, NEW.source_final_block_hash,
                NEW.source_final_block_time, NEW.source_final_seen_at)
            IS DISTINCT FROM
            ROW(OLD.source_final_network, OLD.source_final_block_number, OLD.source_final_block_hash,
                OLD.source_final_block_time, OLD.source_final_seen_at))
       OR (OLD.destination_included_network IS NOT NULL
        AND ROW(NEW.destination_included_network, NEW.destination_included_block_number,
                NEW.destination_included_block_hash, NEW.destination_included_block_time,
                NEW.destination_included_seen_at)
            IS DISTINCT FROM
            ROW(OLD.destination_included_network, OLD.destination_included_block_number,
                OLD.destination_included_block_hash, OLD.destination_included_block_time,
                OLD.destination_included_seen_at))
       OR (OLD.destination_final_network IS NOT NULL
        AND ROW(NEW.destination_final_network, NEW.destination_final_block_number,
                NEW.destination_final_block_hash, NEW.destination_final_block_time,
                NEW.destination_final_seen_at)
            IS DISTINCT FROM
            ROW(OLD.destination_final_network, OLD.destination_final_block_number,
                OLD.destination_final_block_hash, OLD.destination_final_block_time,
                OLD.destination_final_seen_at)) THEN
        RAISE EXCEPTION 'transfer %: a milestone is write-once (REQ-TR-2)', OLD.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;

    RETURN NEW;
END
$$;

DROP FUNCTION deposit_conversion_record(BIGINT, BIGINT, TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP FUNCTION coordinate_lock(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP FUNCTION coordinate_recorded_converted(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP TABLE deposit_conversions;

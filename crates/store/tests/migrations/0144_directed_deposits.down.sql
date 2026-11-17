-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): F-018, T-018-06, the directed deposit's store rules. The three
-- functions 0144 replaced are restored as 0120 (source_bindable, transfers_source_bindable) and 0048
-- (transfers_guard_writer) made them. Transfers bound to a directed arrival are removed first by the
-- test that applies this, as the restored guards would not know them.
DROP FUNCTION directed_deposit(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP TRIGGER intake_answers_guard_directed ON intake_answers;
DROP FUNCTION intake_answers_guard_directed();
DROP TRIGGER transfers_guard_directed ON transfers;
DROP FUNCTION transfers_guard_directed();
DROP FUNCTION address_matches(TEXT, TEXT, TEXT);
DROP FUNCTION refund_reference(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP FUNCTION deposit_reference(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP FUNCTION coordinate_digest(TEXT, BIGINT, BYTEA, position_kind, BIGINT);
DROP FUNCTION reference_tagged(TEXT, BYTEA[]);

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
                   OR (o.kind IN ('arrival', 'convertible_arrival')
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

COMMENT ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2: whether the observation at a '
    'coordinate is one a deposit (p_purpose = deposit: an arrival, a convertible arrival or a '
    'conversion output) or the refund of an unassociated arrival (p_purpose = refund) may name. Its '
    'finality, and whether it is consumed, are checked apart.';

CREATE OR REPLACE FUNCTION transfers_source_bindable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    purpose TEXT;
    binding BOOLEAN;
    moving BOOLEAN;
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
    moving := TG_OP = 'UPDATE' AND OLD.source_network IS NOT NULL
              AND ROW(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                      NEW.source_position_kind, NEW.source_position_index)
                  IS DISTINCT FROM
                  ROW(OLD.source_network, OLD.source_block_number, OLD.source_block_hash,
                      OLD.source_position_kind, OLD.source_position_index);
    finalising := NEW.source_final_network IS NOT NULL
                  AND (TG_OP = 'INSERT' OR OLD.source_final_network IS NULL);
    IF NOT (binding OR moving OR finalising) THEN
        RETURN NEW;
    END IF;
    IF binding OR moving THEN
        PERFORM coordinate_lock(NEW.source_network, NEW.source_block_number, NEW.source_block_hash,
                                NEW.source_position_kind, NEW.source_position_index);
        IF coordinate_recorded_converted(NEW.source_network, NEW.source_block_number,
                                         NEW.source_block_hash, NEW.source_position_kind,
                                         NEW.source_position_index) THEN
            RAISE EXCEPTION '% refused: a deposit recorded the arrival at its source coordinate as the convertible arrival its conversion replaced; it is consumed (INV-2, REQ-CV-2)',
                NEW.kind
                USING ERRCODE = 'BZ001';
        END IF;
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
    IF moving AND NOT EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index
          AND o.kind = 'conversion_output') THEN
        RAISE EXCEPTION '% refused: a deposit''s source moves only to a conversion output (REQ-CV-2, INV-2)',
            NEW.kind
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

CREATE OR REPLACE FUNCTION transfers_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    o         RECORD;
    unproven  INTEGER[];
    issued    BIGINT;
    reviewed  transfer_state;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(writer, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;

    -- S8: the id is the one the store's sequence gave this session, never a writer's choice.
    BEGIN
        issued := currval(pg_get_serial_sequence('public.transfers', 'id'));
    EXCEPTION WHEN object_not_in_prerequisite_state THEN
        issued := NULL;
    END;
    IF issued IS DISTINCT FROM NEW.id THEN
        RAISE EXCEPTION 'transfer % refused for %: id % was not drawn from the store''s sequence; no writer chooses a transfer''s identifier, nor claims the one a withdrawal is about to take (REQ-RQ-4, REQ-RQ-7, REQ-ID-4)',
            NEW.bridge_reference, writer, NEW.id
            USING ERRCODE = 'BZ001';
    END IF;

    IF pg_has_role(writer, 'bezent_core', 'USAGE') THEN
        IF NEW.state = 'Received' AND NEW.kind = 'withdrawal' AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NOT NULL AND NEW.on_chain_reference IS NOT NULL
           AND EXISTS (SELECT 1 FROM networks n
                       WHERE n.network = NEW.source_network AND n.role = 'issuing') THEN
            RETURN NEW;
        END IF;
        IF NEW.state = 'Received' AND NEW.kind = 'refund' AND NEW.refund_of IS NOT NULL THEN
            SELECT t.kind, t.state, t.redeemer, t.on_chain_reference, t.asset, t.amount,
                   t.source_network, t.source_block_number, t.source_block_hash,
                   t.source_position_kind, t.source_position_index
            INTO o
            FROM transfers t WHERE t.id = NEW.refund_of;
            IF FOUND AND o.kind = 'withdrawal'
               AND o.state IN ('Delivering', 'Delivered', 'Needs-Review')
               AND o.redeemer IS NOT NULL AND NEW.redeemer = o.redeemer
               AND NEW.destination = o.redeemer
               AND NEW.on_chain_reference IS NOT DISTINCT FROM o.on_chain_reference
               AND NEW.token IS NULL AND NEW.asset IS NOT DISTINCT FROM o.asset
               AND NEW.amount = o.amount
               AND NEW.source_network IS NOT DISTINCT FROM o.source_network
               AND NEW.source_block_number IS NOT DISTINCT FROM o.source_block_number
               AND NEW.source_block_hash IS NOT DISTINCT FROM o.source_block_hash
               AND NEW.source_position_kind IS NOT DISTINCT FROM o.source_position_kind
               AND NEW.source_position_index IS NOT DISTINCT FROM o.source_position_index THEN
                -- S7: a review the withdrawal entered while delivering, not before its first
                -- payout was ordered.
                IF o.state = 'Needs-Review' THEN
                    SELECT x.from_state INTO reviewed
                    FROM transitions x
                    WHERE x.transfer_id = NEW.refund_of AND x.to_state = 'Needs-Review'
                      AND x.from_state <> 'Needs-Review'
                    ORDER BY x.id DESC LIMIT 1;
                    IF reviewed IS NULL OR reviewed NOT IN ('Delivering', 'Delivered') THEN
                        RAISE EXCEPTION 'refund % refused: the withdrawal it replaces entered review from %, not while delivering; nothing was paid out to prove not executed (INV-8, REQ-RF-1)',
                            NEW.bridge_reference, coalesce(reviewed::text, 'nowhere')
                            USING ERRCODE = 'BZ001';
                    END IF;
                END IF;
                -- S7: at least one payout attempt was ordered ...
                IF NOT EXISTS (SELECT 1 FROM intents i
                               WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order') THEN
                    RAISE EXCEPTION 'refund % refused: the withdrawal it replaces never ordered a payout; a refund replaces a leg proven not executed (INV-8, REQ-RF-1)',
                        NEW.bridge_reference
                        USING ERRCODE = 'BZ001';
                END IF;
                -- ... and every attempt is proven not executed at finality.
                unproven := attempts_unproven(NEW.refund_of);
                IF cardinality(unproven) > 0 THEN
                    RAISE EXCEPTION 'refund % refused: attempt % of the withdrawal it replaces is not proven not executed at finality (INV-8, REQ-RF-1)',
                        NEW.bridge_reference, unproven[1]
                        USING ERRCODE = 'BZ001';
                END IF;
                RETURN NEW;
            END IF;
            RAISE EXCEPTION 'refund % refused: a refund of a transfer is of a withdrawal being delivered or in review, to its redeemer, with its on-chain reference, source, asset and amount (REQ-RF-3, REQ-RF-4, INV-8)',
                NEW.bridge_reference
                USING ERRCODE = 'BZ001';
        END IF;
        RAISE EXCEPTION 'transfer % (%, %) refused for %: the core creates withdrawals from redemptions on the issuing network and refunds of withdrawals, in Received; a request''s transfer is the ingress''s (REQ-RQ-4, REQ-RF-3, REQ-TR-2)',
            NEW.bridge_reference, NEW.kind, NEW.state, writer
            USING ERRCODE = 'BZ001';
    END IF;

    IF pg_has_role(writer, 'bezent_ingress', 'USAGE') THEN
        IF NEW.state = 'Received'
           AND (NEW.kind = 'deposit' OR NEW.kind = 'refund')
           AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NULL AND NEW.on_chain_reference IS NULL
           AND NEW.destination_reference IS NULL
           AND NEW.bridge_reference ~ '^bz-[dur]-'
           AND (NEW.source_network IS NULL
                OR EXISTS (SELECT 1 FROM networks n
                           WHERE n.network = NEW.source_network AND n.role = 'origin')) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'transfer % (%, %) refused for %: the ingress creates only what a request makes, a deposit or the refund of an unassociated arrival, in Received, bound to an origin network, with none of the core''s keys; a withdrawal, a refund of one and anything at Source-Final or later rest on chain data and are the core''s (INV-6, INV-8, REQ-RQ-7, REQ-RF-3, REQ-RF-4)',
            NEW.bridge_reference, NEW.kind, NEW.state, writer
            USING ERRCODE = 'BZ001';
    END IF;

    RAISE EXCEPTION 'transfer % refused for %: only the ingress (a request''s) and the core (a withdrawal, a refund of one) create transfers (REQ-RQ-7, REQ-AR-3)',
        NEW.bridge_reference, writer
        USING ERRCODE = 'BZ001';
END
$$;

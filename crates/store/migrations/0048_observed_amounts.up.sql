-- SPDX-License-Identifier: GPL-3.0-or-later
-- Review of PR #56, M1c, S7 and S8 (issues #35, #46): what a transfer's amount rests on, when the
-- core may refund a withdrawal, and who draws a transfer's identifier.
-- Additive: two triggers on `transfers` (one function); `transfers_guard_writer` (0047) is replaced.
--
-- M1c (REQ-IN-4, REQ-CV-2, REQ-RQ-7; §3 "Can the requester make bezent mint value that did not
-- arrive? No"). 0047 pinned the kind, the state, the references and the source network of a row
-- the ingress makes, not its amount nor its asset. A compromised ingress could bind a real, final
-- arrival of 1 unit to a deposit of 10^9, and the core minted the 10^9. A deposit's amount and
-- asset, and those of the refund of an unassociated arrival, are never a request's: they are the
-- bound observation's (the arrival, or the conversion's output, REQ-CV-2). At binding the store
-- sets them from it when they are not given, and refuses any row, whoever writes it, whose amount
-- is not the observation's or whose asset is not the observed token's family. The check runs again
-- whenever the binding, the source-final milestone, the amount, the asset or the token is written,
-- so a binding made before its observation was recorded is checked when it becomes final (0046
-- refuses Source-Final without the observation). The core re-reads the observation when it orders
-- the mint, and mints its amount scaled by the declared ratio, rounded down (REQ-AS-9).
--
-- S7 (INV-8, REQ-RF-1). 0047 let the core create the refund of a withdrawal in review once
-- `attempts_unproven()` was empty, which it also is for a withdrawal that never ordered a payout
-- (reviewed out of `Accepted` or `Source-Final`). A refund now needs at least one ordered attempt,
-- every attempt proven not executed at finality, and a review entered from `Delivering` or
-- `Delivered`. Its source coordinate, asset and amount are the withdrawal's.
--
-- S8. `id` is GENERATED ALWAYS, but `OVERRIDING SYSTEM VALUE` needs only INSERT, so a writer could
-- claim the ids the core's next insert was about to draw. For every writer but the owner, a row's
-- id must be the one the store's sequence just gave this session (`currval`); a session that drew
-- none is refused. An id the sequence gave one session is never given to another, so no row can
-- occupy the core's. The core's inserts name their conflict target (`bridge_reference`), so any
-- other conflict raises instead of passing silently.

-- ---------------------------------------------------------------------------------------------
-- M1c: the amount and asset of a deposit, or of an unassociated arrival's refund, are observed.
--
-- One function, two triggers (triggers run in name order):
--   * `transfers_amount_from_observation` runs first and fills a missing amount, asset or token
--     from the bound observation, so every later rule (the funding rule reads the asset, 0032)
--     sees the observed values;
--   * `transfers_sourced_observed_amount` runs after every other BEFORE trigger, so the binding,
--     funding and finality rules (0034, 0032, 0046) answer first with their own errors, and
--     refuses a row whose amount or asset is not the observation's, whoever writes it.
CREATE FUNCTION transfers_observed_amount() RETURNS trigger
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

CREATE TRIGGER transfers_amount_from_observation
    BEFORE INSERT OR UPDATE OF source_network
    ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_observed_amount('fill');

CREATE TRIGGER transfers_sourced_observed_amount
    BEFORE INSERT OR UPDATE OF source_network, source_final_network, amount, asset, token
    ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_observed_amount('check');

-- ---------------------------------------------------------------------------------------------
-- S7 and S8: 0047's guard, with the refund of a withdrawal resting on an ordered, disproven
-- attempt, and the identifier drawn by the store.
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

REVOKE EXECUTE ON FUNCTION transfers_observed_amount(), transfers_guard_writer() FROM PUBLIC;

-- SPDX-License-Identifier: GPL-3.0-or-later
-- Review of PR #56, M1b (issues #35, #46): who may create a transfer, and which.
-- Additive: one guard on `transfers`; no table changes.
--
-- 0046 guarded the transitions only; INSERT on `transfers` was the ingress's on the whole table.
-- A compromised ingress could insert the refund of a withdrawal being delivered (and get a refund
-- mint beside the payout), take a withdrawal's bridge reference or its source coordinate first (so
-- the core's own insert does nothing and the redemption is never paid), or insert a deposit at
-- Source-Final. Each row now has one creator (§5.4: a transfer begins `Received`, or `Source-Final`
-- for a bound deposit, REQ-IN-3):
--
--   * The ingress creates only what a request makes (REQ-RQ-3, REQ-RQ-6, REQ-RQ-7; guardrails 2
--     and 3): a deposit, or the refund of an unassociated arrival (`refund_of` NULL), in `Received`,
--     never later; carrying no redeemer, on-chain reference or destination reference (those come
--     from a redemption, or from the core); its bridge reference in the request namespaces
--     (`bz-d-` a bound deposit, `bz-u-` an unassociated refund, `bz-r-` a refusal), never the
--     core's (`bz-w-` a withdrawal, `bz-f-` a refund of one); its source, if any, on an origin
--     network (where every arrival a request may name is, `source_bindable()`), never the issuing
--     network a redemption is observed on. So no row it makes holds a key (bridge reference,
--     source binding, on-chain reference, destination reference, `refund_of`) the core's creation
--     path needs.
--   * The core creates the withdrawals (from the redemptions it observes on the issuing network,
--     REQ-RQ-4, REQ-OT-2) and the refunds of withdrawals (REQ-RF-3, REQ-RF-4), in `Received`. A
--     refund of a withdrawal only of a withdrawal being delivered or in review, to its redeemer,
--     with its on-chain reference, once every attempt ordered, prepared or signed is proven not
--     executed at finality (INV-8, REQ-RF-1); the same transaction moves the withdrawal to
--     `Refunding`, which checks it again (0014, 0015).
--   * The migration owner (drills, restores, NFR-7) may insert any row the other rules admit.
--   * Nobody else creates a transfer.
--
-- The writer is `session_user`, the login that connected: the guard is SECURITY DEFINER (it reads
-- the networks and the attempts, which the ingress may not), so `current_user` is the owner inside
-- it. A service login holds its role without SET (#46), so it cannot be another role.

CREATE FUNCTION transfers_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    writer    NAME := session_user;
    owner_oid OID;
    o         RECORD;
    unproven  INTEGER[];
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(writer, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;

    IF pg_has_role(writer, 'bezent_core', 'USAGE') THEN
        IF NEW.state = 'Received' AND NEW.kind = 'withdrawal' AND NEW.refund_of IS NULL
           AND NEW.redeemer IS NOT NULL AND NEW.on_chain_reference IS NOT NULL
           AND EXISTS (SELECT 1 FROM networks n
                       WHERE n.network = NEW.source_network AND n.role = 'issuing') THEN
            RETURN NEW;
        END IF;
        IF NEW.state = 'Received' AND NEW.kind = 'refund' AND NEW.refund_of IS NOT NULL THEN
            SELECT t.kind, t.state, t.redeemer, t.on_chain_reference INTO o
            FROM transfers t WHERE t.id = NEW.refund_of;
            IF FOUND AND o.kind = 'withdrawal'
               AND o.state IN ('Delivering', 'Delivered', 'Needs-Review')
               AND o.redeemer IS NOT NULL AND NEW.redeemer = o.redeemer
               AND NEW.destination = o.redeemer
               AND NEW.on_chain_reference IS NOT DISTINCT FROM o.on_chain_reference THEN
                unproven := attempts_unproven(NEW.refund_of);
                IF cardinality(unproven) > 0 THEN
                    RAISE EXCEPTION 'refund % refused: attempt % of the withdrawal it replaces is not proven not executed at finality (INV-8, REQ-RF-1)',
                        NEW.bridge_reference, unproven[1]
                        USING ERRCODE = 'BZ001';
                END IF;
                RETURN NEW;
            END IF;
            RAISE EXCEPTION 'refund % refused: a refund of a transfer is of a withdrawal being delivered or in review, to its redeemer, with its on-chain reference (REQ-RF-3, REQ-RF-4, INV-8)',
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

CREATE TRIGGER transfers_guard_writer
    BEFORE INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_writer();

REVOKE EXECUTE ON FUNCTION transfers_guard_writer() FROM PUBLIC;

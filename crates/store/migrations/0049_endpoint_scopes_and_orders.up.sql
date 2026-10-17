-- SPDX-License-Identifier: AGPL-3.0-or-later
-- The exhaustive write-path audit of PR #56 (issues #35, #46, #23). Additive: new guards, one
-- column on `signatures`, read grants for the signer; no row is touched.
--
-- Must-fix 1 (REQ-OB-6, INV-8). Only the EVM endpoint's login was held to its own networks on the
-- chain tables (0035); the Kreivo endpoint's login could write an EVM network's final heads, read
-- ranges, prepared and signed transactions and proofs of non-execution. A faked proof for a payout
-- lets the core refund it, and if the payout ran, the redeemer is paid twice. Every chain table now
-- holds each endpoint's login to the networks its registered role covers (`endpoint_scopes`):
-- observations and their outcomes, read ranges, final heads, readings, prepared transactions,
-- signed transactions and proofs. The guard asks of the session's login as well as of the role it
-- acts as, as 0035 does: SET ROLE changes current_user, never session_user.

-- ---------------------------------------------------------------------------------------------
-- Must-fix 1: each endpoint's login writes only its own networks' chain records.
--
-- For each of the session's login and the role it acts as that holds the endpoints' mark
-- (`bezent_endpoint`) or a registered endpoint role, and is neither a superuser nor the table's
-- owner (drills and restores, NFR-7): it must hold at least one registered endpoint role, and every
-- registered endpoint role it holds must cover the row's network (a prepared transaction counts as
-- its order's network, a signed transaction as its intent's, an outcome as its observation's:
-- `bezent_row_network`, 0035). So the Kreivo endpoint writes the issuing network's rows only, the
-- EVM endpoint the EVM origin networks' only, and a login holding both (which the start-up check
-- refuses) writes nothing.
--
-- Runs as the inserting login (no SECURITY DEFINER: it must see who inserts); every relation it
-- reads (`endpoint_scopes`, `networks`, and through `bezent_row_network` `intents` and
-- `observations`) the endpoints' roles read already.
CREATE FUNCTION endpoint_guard_scope() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    target TEXT;
    w      RECORD;
BEGIN
    FOR w IN
        SELECT u.oid, u.rolname FROM pg_roles u
        WHERE u.rolname IN (session_user, current_user)
          AND NOT u.rolsuper
          AND NOT pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')
          AND (pg_has_role(u.oid, 'bezent_endpoint', 'MEMBER')
               OR EXISTS (SELECT 1 FROM endpoint_scopes s JOIN pg_roles r ON r.rolname = s.endpoint_role
                          WHERE pg_has_role(u.oid, r.oid, 'MEMBER')))
    LOOP
        IF target IS NULL THEN
            target := bezent_row_network(TG_RELID, to_jsonb(NEW));
        END IF;
        IF NOT EXISTS (SELECT 1 FROM endpoint_scopes s JOIN pg_roles r ON r.rolname = s.endpoint_role
                       WHERE pg_has_role(w.oid, r.oid, 'MEMBER'))
           OR EXISTS (SELECT 1 FROM endpoint_scopes s JOIN pg_roles r ON r.rolname = s.endpoint_role
                      WHERE pg_has_role(w.oid, r.oid, 'MEMBER')
                        AND NOT EXISTS (SELECT 1 FROM networks n
                                        WHERE n.network = target AND n.role = s.network_role
                                          AND (s.address_format IS NULL
                                               OR s.address_format = n.address_format))) THEN
            RAISE EXCEPTION '% on % refused for %: each endpoint''s login records only its own networks'' rows (REQ-OB-6, INV-8)',
                TG_TABLE_NAME, coalesce(target, 'no network'), w.rolname
                USING ERRCODE = 'BZ001';
        END IF;
    END LOOP;
    RETURN NEW;
END
$$;

CREATE TRIGGER observations_endpoint_scope
    BEFORE INSERT ON observations
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER observation_outcomes_endpoint_scope
    BEFORE INSERT ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER observation_ranges_endpoint_scope
    BEFORE INSERT ON observation_ranges
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER final_heads_endpoint_scope
    BEFORE INSERT ON final_heads
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER reserve_readings_endpoint_scope
    BEFORE INSERT ON reserve_readings
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER prepared_transactions_endpoint_scope
    BEFORE INSERT ON prepared_transactions
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER signed_transactions_endpoint_scope
    BEFORE INSERT ON signed_transactions
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();
CREATE TRIGGER non_execution_proofs_endpoint_scope
    BEFORE INSERT ON non_execution_proofs
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();

REVOKE EXECUTE ON FUNCTION endpoint_guard_scope() FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- Should-fix (INV-6, INV-7, REQ-IN-4, REQ-AS-9, REQ-RF-4): a delivery order rests on a final
-- observation of its transfer's source, and moves what that observation carries.
--
-- The core's `order()` re-reads the observation (0048), but the store took any order the core's
-- login wrote: a compromised core could order a mint not tied to a final arrival, or of another
-- amount. For every writer but the owner (drills, NFR-7), an order (`payload ? 'order'`, with no
-- `message` and no `prepared`) is refused unless:
--
--   * mint: of a deposit, on the issuing network, whose source observation is final and executed
--     (outcome `final`, decided at a recorded final head at or above the observed block, hash
--     matching, and no final head at the observed height with another hash: 0046's rule), whose
--     amount is the observed amount, ordering exactly the observed amount scaled to the asset's
--     precision, rounded down (REQ-AS-9), of the asset's issuing asset;
--   * refund-mint: of the refund of a withdrawal, on the issuing network, to its redeemer, whose
--     source (the redemption) is final and executed, ordering exactly the redeemed amount of the
--     redeemed asset (its issuing identity, which the signer keeps its window cap by);
--   * payout of a withdrawal: on its destination network, to its destination, whose source (the
--     redemption) is final and executed, ordering exactly the redeemed amount scaled to the
--     destination network's admitted token (the withdrawal's own amount, which must be the same);
--   * payout of the refund of an unassociated arrival: on the arrival's network, to its
--     destination, whose source (the arrival) is final and executed, ordering exactly the observed
--     amount;
--   * cancel: of no transfer, moving nothing (amount "0").
--
-- Any other order is refused. SECURITY DEFINER: it reads the final heads and the declarations; the
-- writer is the session's login.
CREATE FUNCTION bezent_final_source(p_transfer BIGINT)
    RETURNS TABLE (network TEXT, amount NUMERIC, token TEXT)
    LANGUAGE sql STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT ob.network, ob.amount, ob.token
    FROM transfers t
    JOIN observations ob
      ON ob.network = t.source_network AND ob.block_number = t.source_block_number
     AND ob.block_hash = t.source_block_hash AND ob.position_kind = t.source_position_kind
     AND ob.position_index = t.source_position_index
    JOIN observation_outcomes x ON x.observation_id = ob.id
    WHERE t.id = p_transfer
      AND ob.executed AND ob.amount IS NOT NULL
      AND x.outcome = 'final'
      AND EXISTS (SELECT 1 FROM final_heads h
                  WHERE h.network = ob.network AND h.block_number = x.final_block_number
                    AND h.block_hash = x.final_block_hash AND h.block_number >= ob.block_number)
      AND NOT EXISTS (SELECT 1 FROM final_heads h
                      WHERE h.network = ob.network AND h.block_number = ob.block_number
                        AND h.block_hash <> ob.block_hash)
$$;

CREATE FUNCTION intents_guard_order() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    t         transfers%ROWTYPE;
    src       RECORD;
    ordered   NUMERIC;
    expected  NUMERIC;
    issuing   TEXT;
    o         JSONB := NEW.payload -> 'order';
BEGIN
    IF NOT (NEW.payload ? 'order') OR NEW.payload ? 'message' OR NEW.payload ? 'prepared' THEN
        RETURN NEW;
    END IF;
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF jsonb_typeof(NEW.payload -> 'amount') IS DISTINCT FROM 'string'
       OR (NEW.payload ->> 'amount') !~ '^(0|[1-9][0-9]*)$' THEN
        RAISE EXCEPTION 'order refused: its amount is not a decimal string of minor units (INV-11)'
            USING ERRCODE = 'BZ001';
    END IF;
    ordered := (NEW.payload ->> 'amount')::numeric;

    IF NEW.purpose = 'cancel' THEN
        IF NEW.transfer_id IS NULL AND ordered = 0 AND jsonb_typeof(o -> 'cancel') = 'number' THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'cancel order refused: a cancel names its signed transaction, of no transfer, and moves nothing (REQ-NX-4)'
            USING ERRCODE = 'BZ001';
    END IF;

    SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
    IF NOT FOUND THEN
        RAISE EXCEPTION '% order refused: it names no transfer (INV-6, INV-7)', NEW.purpose
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT * INTO src FROM bezent_final_source(t.id);
    IF NOT FOUND THEN
        RAISE EXCEPTION '% order for % refused: its source observation is not final and executed at a recorded final head; nothing is minted before the arrival is final, nor paid out before the redemption is (INV-6, INV-7)',
            NEW.purpose, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT n.network INTO issuing FROM networks n WHERE n.role = 'issuing';

    IF NEW.purpose = 'mint' AND t.kind = 'deposit' THEN
        SELECT floor(src.amount / power(10::numeric, k.precision - a.precision))
        INTO expected
        FROM assets a JOIN admitted_tokens k ON k.asset = a.asset
        WHERE a.asset = t.asset AND k.network = src.network AND k.token = src.token
          AND k.precision >= a.precision
          AND o ->> 'asset' IS NOT DISTINCT FROM a.issuing_asset;
        IF expected IS NOT NULL AND NEW.network = issuing AND t.amount = src.amount
           AND ordered = expected THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'mint order for % refused: a mint orders exactly the final arrival''s amount (%), scaled to the asset (%), of the asset''s issuing asset, on the issuing network, for a deposit of that amount; it ordered % (INV-6, REQ-IN-4, REQ-AS-9)',
            t.bridge_reference, src.amount, expected, ordered
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'refund-mint' AND t.kind = 'refund' AND t.refund_of IS NOT NULL THEN
        IF NEW.network = issuing AND src.network = issuing AND ordered = src.amount
           AND o ->> 'redeemer' IS NOT DISTINCT FROM t.redeemer
           AND o ->> 'asset' IS NOT DISTINCT FROM src.token THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'refund-mint order for % refused: it re-mints exactly the final redemption''s amount (%) to its redeemer, on the issuing network; it ordered % (INV-8, REQ-RF-4)',
            t.bridge_reference, src.amount, ordered
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'withdrawal' THEN
        SELECT src.amount * power(10::numeric, k.precision - a.precision)
        INTO expected
        FROM assets a JOIN admitted_tokens k ON k.asset = a.asset
        WHERE a.asset = t.asset AND k.network = t.destination_network
          AND a.issuing_asset = src.token AND k.precision >= a.precision;
        IF expected IS NOT NULL AND src.network = issuing
           AND NEW.network = t.destination_network AND t.amount = expected AND ordered = expected
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: a payout orders exactly the final redemption''s amount (%), scaled to the destination''s token (%), to the withdrawal''s destination on its network; it ordered % (INV-7, REQ-OT-3, REQ-TR-5)',
            t.bridge_reference, src.amount, expected, ordered
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'refund' AND t.refund_of IS NULL THEN
        IF NEW.network = src.network AND t.destination_network = src.network
           AND t.amount = src.amount AND ordered = src.amount
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: the refund of an unassociated arrival pays out exactly the final arrival''s amount (%) on its network, to the refund''s destination; it ordered % (REQ-IN-6, REQ-RQ-7)',
            t.bridge_reference, src.amount, ordered
            USING ERRCODE = 'BZ001';
    END IF;
    RAISE EXCEPTION '% order for % (%) refused: no such delivery (INV-6, INV-7)',
        NEW.purpose, t.bridge_reference, t.kind
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER intents_guard_order
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_order();

REVOKE EXECUTE ON FUNCTION bezent_final_source(BIGINT), intents_guard_order() FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- Should-fix (REQ-TR-6, REQ-RQ-7): no login chooses or pre-claims a transition's id.
--
-- `transitions.id` is GENERATED ALWAYS, but `OVERRIDING SYSTEM VALUE` needs only INSERT: the
-- ingress could insert its decisions under the ids the core's next transitions were about to draw,
-- and stall the core on the conflict. As for transfers (0048, S8): for every writer but the owner,
-- a transition's id is the one the store's sequence just gave this session (`currval`). SECURITY
-- DEFINER: `currval` needs a privilege on the sequence, which no service holds; the session's
-- value is the session's whoever reads it.
CREATE FUNCTION transitions_guard_id() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    issued    BIGINT;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    BEGIN
        issued := currval(pg_get_serial_sequence('public.transitions', 'id'));
    EXCEPTION WHEN object_not_in_prerequisite_state THEN
        issued := NULL;
    END;
    IF issued IS DISTINCT FROM NEW.id THEN
        RAISE EXCEPTION 'transition to % refused for %: id % was not drawn from the store''s sequence; no writer chooses a transition''s identifier, nor claims the one the core''s next transition is about to take (REQ-TR-6, REQ-RQ-7)',
            NEW.to_state, session_user, NEW.id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transitions_guard_id
    BEFORE INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_guard_id();

REVOKE EXECUTE ON FUNCTION transitions_guard_id() FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- Should-fix (AC-I1.2, REQ-IC-4): a key-compromise alert is its detector's, and a row written in
-- advance never suppresses a real one.
--
-- Every detector looked for an earlier alert by a free-form field of the payload (`dedupe`,
-- `requested_intent`, `coordinate`), and any endpoint or the core could append one: an endpoint
-- could write the core's supply-mismatch alert, or another network's, before it was raised, and
-- the real alert and its pause request were never written. Now:
--
--   * Each alert is stamped by the store with its detector, the role of the login that wrote it
--     (`core`, `kreivo-endpoint`, `evm-endpoint`, `signer`), and its detection key, derived from
--     what it names: `dedupe:<dedupe>`, else `intent:<requested_intent>`, else
--     `coordinate:<network>:<block hash>:<position index>`. A row that names none is refused, as
--     is a row from any other role (the owner, for drills, writes any row and keeps what it
--     names).
--   * An endpoint's alert is of a network its registered role covers (its coordinate's, its
--     `network`, or its intent's).
--   * One alert per (detector, detection key): a unique index. Each detector looks only for its
--     own alerts, by detection key, so no other role's row (and no row of a free-form shape)
--     suppresses it.
--   * The signer detects too (must-fix 2): a transaction whose decoded bytes do not answer the
--     core's order raises the alert, so it may append `key-compromise` and `pause-requested`, and
--     nothing else.
CREATE FUNCTION event_log_stamp_key_compromise() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    writer    NAME;
    detector  TEXT;
    roles     TEXT[];
    key       TEXT;
    seen_on   TEXT;
    p         JSONB := NEW.payload;
BEGIN
    IF NEW.kind <> 'key-compromise' THEN
        RETURN NEW;
    END IF;
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    -- The writer: the session's login, unless it is a superuser or the owner (a drill, or a test
    -- acting as a role), when it is the role the session acts as.
    IF (SELECT rolsuper FROM pg_roles WHERE rolname = session_user)
       OR pg_has_role(session_user, owner_oid, 'MEMBER') THEN
        writer := current_user;
    ELSE
        writer := session_user;
    END IF;

    IF jsonb_typeof(p -> 'dedupe') = 'string' THEN
        key := 'dedupe:' || (p ->> 'dedupe');
    ELSIF jsonb_typeof(p -> 'requested_intent') = 'number' THEN
        key := 'intent:' || (p ->> 'requested_intent');
    ELSIF jsonb_typeof(p -> 'coordinate') = 'object'
          AND jsonb_typeof(p -> 'coordinate' -> 'network') = 'string'
          AND jsonb_typeof(p -> 'coordinate' -> 'block_hash') = 'string'
          AND p -> 'coordinate' ? 'position_index' THEN
        key := 'coordinate:' || (p -> 'coordinate' ->> 'network') || ':'
               || (p -> 'coordinate' ->> 'block_hash') || ':'
               || (p -> 'coordinate' ->> 'position_index');
    END IF;

    IF (SELECT rolsuper FROM pg_roles WHERE rolname = writer)
       OR pg_has_role(writer, owner_oid, 'MEMBER') THEN
        -- The owner (drills, restores, NFR-7): stamped when it names something, never refused.
        IF key IS NOT NULL THEN
            NEW.payload := p || jsonb_build_object('detector', coalesce(p ->> 'detector', 'owner'),
                                                   'detection_key', key);
        END IF;
        RETURN NEW;
    END IF;

    roles := ARRAY(SELECT r.d FROM (VALUES ('bezent_core', 'core'),
                                           ('bezent_kreivo_endpoint', 'kreivo-endpoint'),
                                           ('bezent_evm_endpoint', 'evm-endpoint'),
                                           ('bezent_signer', 'signer')) AS r(role, d)
                   WHERE pg_has_role(writer, r.role, 'USAGE'));
    IF cardinality(roles) <> 1 THEN
        RAISE EXCEPTION 'key-compromise refused for %: only the role that detects it records it: the core, an endpoint or the signer, one of them (AC-I1.2, REQ-IC-4)',
            writer
            USING ERRCODE = 'BZ001';
    END IF;
    detector := roles[1];
    IF key IS NULL THEN
        RAISE EXCEPTION 'key-compromise refused for %: it names no detection (a dedupe key, the intent, or the coordinate it was seen at) (AC-I1.2, REQ-IC-4)',
            writer
            USING ERRCODE = 'BZ001';
    END IF;
    IF detector IN ('kreivo-endpoint', 'evm-endpoint') THEN
        seen_on := coalesce(p -> 'coordinate' ->> 'network', p ->> 'network',
                            (SELECT i.network FROM intents i
                             WHERE jsonb_typeof(p -> 'requested_intent') = 'number'
                               AND i.id = (p ->> 'requested_intent')::bigint));
        IF NOT EXISTS (SELECT 1 FROM endpoint_scopes s JOIN networks n ON n.network = seen_on
                       WHERE s.endpoint_role = CASE detector WHEN 'kreivo-endpoint'
                                                   THEN 'bezent_kreivo_endpoint'
                                                   ELSE 'bezent_evm_endpoint' END
                         AND n.role = s.network_role
                         AND (s.address_format IS NULL OR s.address_format = n.address_format)) THEN
            RAISE EXCEPTION 'key-compromise refused for %: an endpoint raises it only on a network it covers, not % (AC-I1.2, REQ-OB-6)',
                writer, coalesce(seen_on, 'no network')
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    NEW.payload := p || jsonb_build_object('detector', detector, 'detection_key', key);
    RETURN NEW;
END
$$;

-- Runs after 0007's sequencing and before the writers' guards (triggers fire in name order).
CREATE TRIGGER event_log_b_stamp_key_compromise
    BEFORE INSERT ON event_log
    FOR EACH ROW EXECUTE FUNCTION event_log_stamp_key_compromise();

CREATE UNIQUE INDEX event_log_one_key_compromise_per_detection
    ON event_log ((payload ->> 'detector'), (payload ->> 'detection_key'))
    WHERE kind = 'key-compromise' AND payload ? 'detection_key';

-- 0020's guard, with the signer: it appends key-compromise and pause-requested (must-fix 2). It
-- asks what `bezent_writer_holds` asks, inline: the signer holds no EXECUTE on that helper.
CREATE OR REPLACE FUNCTION event_log_guard_signal() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NEW.kind IN ('key-compromise', 'pause-requested', 'delivery-anomaly')
       AND NOT (pg_has_role(current_user, 'bezent_endpoint', 'USAGE')
                OR pg_has_role(current_user,
                               (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE')
                OR pg_has_role(current_user, 'bezent_core', 'USAGE')
                OR (NEW.kind IN ('key-compromise', 'pause-requested')
                    AND pg_has_role(current_user, 'bezent_signer', 'USAGE'))) THEN
        RAISE EXCEPTION '% refused: only an endpoint, the core or (an alert and its pause request) the signer records it (AC-I1.2, REQ-IC-4)', NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

GRANT INSERT ON event_log TO bezent_signer;
REVOKE EXECUTE ON FUNCTION event_log_stamp_key_compromise() FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- Should-fix (REQ-RQ-3, REQ-IN-6, REQ-RQ-7): the destination-network rules the ingress applies
-- hold in the store too.
--
-- The ingress refuses (ERR-InvalidDestination) a deposit whose destination is not on the issuing
-- network or whose refund destination is not on the arrival's network, and the refund of an
-- unassociated arrival whose destination is not on the arrival's network. Only its code held them:
-- a compromised ingress could bind a final arrival to a deposit minted to an origin network's
-- address, or a refund paid out on another network. For every writer but the owner, a deposit or
-- an unassociated arrival's refund is refused, when it is bound to a source (by insert) and when it
-- is accepted (Received -> Accepted), unless:
--   * deposit: its destination is on the issuing network, and its refund destination, if any, on
--     its source's network;
--   * refund of an unassociated arrival: its destination is on its source's network.
-- A refused request's transfer binds no source and is never accepted, so it is not held to them.
CREATE FUNCTION transfer_destination_refusal(t transfers) RETURNS TEXT
    LANGUAGE sql STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT CASE
        WHEN t.kind = 'deposit'
             AND NOT EXISTS (SELECT 1 FROM networks n
                             WHERE n.network = t.destination_network AND n.role = 'issuing')
            THEN 'a deposit''s destination is on the issuing network'
        WHEN t.kind = 'deposit' AND t.refund_destination_network IS NOT NULL
             AND t.refund_destination_network IS DISTINCT FROM t.source_network
            THEN 'a deposit''s refund destination is on its arrival''s network'
        WHEN t.kind = 'refund' AND t.refund_of IS NULL
             AND t.destination_network IS DISTINCT FROM t.source_network
            THEN 'the refund of an unassociated arrival is paid out on the arrival''s network'
    END
$$;

CREATE FUNCTION transfers_guard_destination() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    t         transfers%ROWTYPE;
    why       TEXT;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = 'public.transfers'::regclass;
    IF pg_has_role(session_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF TG_TABLE_NAME = 'transfers' THEN
        IF NEW.source_network IS NULL THEN
            RETURN NEW;
        END IF;
        t := NEW;
    ELSE
        IF NOT (NEW.from_state = 'Received' AND NEW.to_state = 'Accepted') THEN
            RETURN NEW;
        END IF;
        SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id;
    END IF;
    why := transfer_destination_refusal(t);
    IF why IS NOT NULL THEN
        RAISE EXCEPTION 'transfer % refused: % (ERR-InvalidDestination, REQ-RQ-3, REQ-IN-6, REQ-RQ-7)',
            t.bridge_reference, why
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_guard_destination
    BEFORE INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_destination();
CREATE TRIGGER transitions_guard_destination
    BEFORE INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transfers_guard_destination();

REVOKE EXECUTE ON FUNCTION transfer_destination_refusal(transfers), transfers_guard_destination()
    FROM PUBLIC;

-- ---------------------------------------------------------------------------------------------
-- Must-fix 2 (REQ-AR-3, INV-9, issue #23): the signer decodes what it signs and compares it with
-- the core's order.
--
-- The signer checked the bytes only against the endpoint's preparation record, and its window cap
-- counted the amount the intent declared. It now decodes the exact bytes itself (the Safe's
-- execTransactionWithRole of a MultiSendCallOnly batch on an EVM network; the extrinsic and the
-- contract call on the issuing network) and compares the recipient, the asset or token, the
-- amount, the reference, the network (chain id, or genesis and contract) and a cancel's nonce with
-- the core's order, which it reads through its own read-only grant (`intents`); any mismatch
-- refuses and raises the key-compromise alert (above). Here:
--
--   * `signatures` records what the signer decoded: the transaction's `nonce` in the key's lane
--     and the `asset` it moves. The window cap sums the decoded amounts per key, network and asset
--     (issue #23), and a nonce the key signed at is signed again only once that transaction is
--     proven not executed (a cancel takes exactly the nonce of what it cancels).
--   * The signer reads the admitted tokens (which token an order's asset is paid in) and which
--     signing intent a signed transaction is of (a cancel's target): declarations and ids, no
--     bytes.
ALTER TABLE signatures ADD COLUMN nonce NUMERIC CHECK (nonce >= 0 AND nonce = trunc(nonce));
ALTER TABLE signatures ADD COLUMN asset TEXT CHECK (asset <> '');

COMMENT ON COLUMN signatures.nonce IS
    'REQ-EO-4, audit of PR #56 must-fix 2: the nonce the signer decoded from the bytes it signed, '
    'in the key''s lane. NULL for a statement, and for signatures made before 0049.';
COMMENT ON COLUMN signatures.asset IS
    'Issue #23: the asset the signer decoded the bytes to move (the bridged asset of a payout, the '
    'issuing asset of a mint or refund mint), which its window cap is kept per. NULL for a cancel '
    'and a statement.';

CREATE INDEX signatures_by_key_network_nonce ON signatures (key_id, network, nonce)
    WHERE nonce IS NOT NULL;

GRANT SELECT ON admitted_tokens TO bezent_signer;
GRANT SELECT (id, intent_id) ON signed_transactions TO bezent_signer;
-- The position of the alert it appends (INSERT ... RETURNING seq needs it); nothing of the log's
-- content.
GRANT SELECT (seq) ON event_log TO bezent_signer;

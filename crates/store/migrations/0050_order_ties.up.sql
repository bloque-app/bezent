-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #36 (from the final review of the #25 work): with only the core compromised, two things an
-- order carries were still the core's to choose. Additive: one helper, and 0049's
-- `intents_guard_order` replaced by itself with two more ties; no row is touched.
--
--   * A payout order's asset and token (INV-7, REQ-AS-2, REQ-AS-3, REQ-OT-4). 0049 tied a payout's
--     amount, network and beneficiary to its redemption, but not the `asset` and `token` the order
--     names, and the signer and the endpoints pay in the token they read from them. A payout in
--     another asset's token, for an amount scaled for the transfer's own, multiplies the value paid
--     out (a 6-decimal amount paid in an 18-decimal token). From here, for every writer but the
--     owner, a payout order's `asset` is its transfer's, and its `token`, when it names one, is the
--     token admitted for that asset on the destination network. The refund of an unassociated
--     arrival, paid out in the arrival's own minor units, is ordered in the arrival's token only:
--     the asset it names and that asset's admitted token, or, when its token is not admitted
--     (REQ-IN-6), no asset and that token itself.
--
--   * A mint order's recipient (INV-6, REQ-IN-4, REQ-RQ-7). 0049 tied a mint's amount, asset and
--     network to the final arrival, not to whom it mints: the core could mint a real, final
--     arrival to an account of its own. From here a mint order's `to` is the 32-byte account the
--     deposit's destination decodes to, written as the core writes it (`0x` and 64 hex digits, read
--     without regard to case). A destination is `0x` and 64 hex digits or an SS58 address (a one- or two-byte
--     prefix, the account, a two-byte checksum): the ingress verified the checksum when it took the
--     request (ERR-InvalidDestination), so the decode here reads the account and need not check it
--     again (the store has no BLAKE2).
--
-- The signer's decoded comparison of the bytes it signs against the order stays as it is; this
-- ties the order itself, so a compromised core cannot make the order the signer is held to.

-- The 32-byte account an issuing-network address names: `0x` and 64 hex digits, or SS58 (base58 of
-- a one-byte prefix below 64 and 35 bytes, or a two-byte prefix in 64..127 and 36 bytes; the last
-- two bytes are the checksum). NULL for anything else. 0043's decode, as 0043's ledger accounts read
-- an address.
CREATE FUNCTION issuing_account(p_address TEXT) RETURNS BYTEA
    LANGUAGE plpgsql IMMUTABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    raw BYTEA;
BEGIN
    IF p_address ~ '^0x[0-9a-fA-F]{64}$' THEN
        RETURN decode(lower(substr(p_address, 3)), 'hex');
    END IF;
    raw := ledger_accounts_base58(p_address);
    IF raw IS NULL OR NOT (
           (length(raw) = 35 AND get_byte(raw, 0) < 64)
        OR (length(raw) = 36 AND get_byte(raw, 0) BETWEEN 64 AND 127)) THEN
        RETURN NULL;
    END IF;
    RETURN substring(raw FROM length(raw) - 33 FOR 32);
END
$$;

REVOKE EXECUTE ON FUNCTION issuing_account(TEXT) FROM PUBLIC;

-- 0049's guard, with the two ties above. SECURITY DEFINER as before: it reads the final heads and
-- the declarations; the writer is the session's login.
CREATE OR REPLACE FUNCTION intents_guard_order() RETURNS trigger
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
    account   BYTEA;
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
        -- The recipient is the deposit's destination (issue #36): the account it decodes to.
        account := issuing_account(t.destination);
        IF expected IS NOT NULL AND NEW.network = issuing AND t.amount = src.amount
           AND ordered = expected
           AND account IS NOT NULL
           AND lower(o ->> 'to') IS NOT DISTINCT FROM '0x' || encode(account, 'hex') THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'mint order for % refused: a mint orders exactly the final arrival''s amount (%), scaled to the asset (%), of the asset''s issuing asset, to the account the deposit''s destination names, on the issuing network, for a deposit of that amount; it ordered % to % (INV-6, REQ-IN-4, REQ-AS-9)',
            t.bridge_reference, src.amount, expected, ordered, coalesce(o ->> 'to', 'no recipient')
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
        -- The asset is the transfer's, and a token, if the order names one, the asset's admitted
        -- token on the destination network (issue #36): the amount above is scaled for that one.
        IF expected IS NOT NULL AND src.network = issuing
           AND NEW.network = t.destination_network AND t.amount = expected AND ordered = expected
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination
           AND o ->> 'asset' IS NOT DISTINCT FROM t.asset
           AND (o ->> 'token' IS NULL
                OR EXISTS (SELECT 1 FROM admitted_tokens k
                           WHERE k.network = t.destination_network AND k.asset = t.asset
                             AND lower(k.token) = lower(o ->> 'token'))) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: a payout orders exactly the final redemption''s amount (%), scaled to the destination''s token (%), to the withdrawal''s destination on its network, of the withdrawal''s own asset (%) and the token admitted for it; it ordered % of asset %, token % (INV-7, REQ-OT-3, REQ-OT-4, REQ-AS-2, REQ-AS-3, REQ-TR-5)',
            t.bridge_reference, src.amount, expected, t.asset, ordered,
            coalesce(o ->> 'asset', 'none'), coalesce(o ->> 'token', 'none')
            USING ERRCODE = 'BZ001';
    ELSIF NEW.purpose = 'payout' AND t.kind = 'refund' AND t.refund_of IS NULL THEN
        -- Paid out in the arrival's own minor units: in the arrival's own token (issue #36). The
        -- refund names the arrival's asset (and the token admitted for it is the arrival's), or,
        -- its token not being admitted, that token and no asset (REQ-IN-6; 0048 holds the refund's
        -- own asset and token to the observation).
        IF NEW.network = src.network AND t.destination_network = src.network
           AND t.amount = src.amount AND ordered = src.amount
           AND o ->> 'to' IS NOT DISTINCT FROM t.destination
           AND o ->> 'asset' IS NOT DISTINCT FROM t.asset
           AND (t.asset IS NOT NULL
                AND (o ->> 'token' IS NULL OR lower(o ->> 'token') = lower(src.token))
                OR t.asset IS NULL
                AND lower(o ->> 'token') = lower(t.token) AND lower(t.token) = lower(src.token)) THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'payout order for % refused: the refund of an unassociated arrival pays out exactly the final arrival''s amount (%) on its network, to the refund''s destination, in the arrival''s own token (asset %, token %); it ordered % of asset %, token % (REQ-IN-6, REQ-RQ-7, REQ-AS-3)',
            t.bridge_reference, src.amount, coalesce(t.asset, 'none'), src.token, ordered,
            coalesce(o ->> 'asset', 'none'), coalesce(o ->> 'token', 'none')
            USING ERRCODE = 'BZ001';
    END IF;
    RAISE EXCEPTION '% order for % (%) refused: no such delivery (INV-6, INV-7)',
        NEW.purpose, t.bridge_reference, t.kind
        USING ERRCODE = 'BZ001';
END
$$;

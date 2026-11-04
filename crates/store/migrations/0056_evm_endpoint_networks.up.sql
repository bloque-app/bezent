-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-009 / M-2 (issue #30; PLAN DEC-8; F-009 NOTES, "Review of PR #39: left for later", S7): one
-- EVM endpoint instance per network, so one login writes one network's records.
--
-- 0035 narrowed `bezent_evm_endpoint` to EVM networks, all of them: with Base and BNB Chain beside
-- Polygon, a compromised Polygon instance could write Base's observations, final heads, prepared
-- transactions and proofs. Each instance runs under a login of its own; this table says which
-- EVM networks that login records. The guard of 0035 now refuses a row of an EVM network the
-- session's login is not mapped to (asked of `session_user` and `current_user`, as 0035's writer test is). A login holding the role with no row here records nothing: fail closed. The migration owner
-- and a superuser write everything (NFR-7), as before.
--
-- Existing deployments keep working: every login that holds the role when this runs is mapped to
-- every EVM origin network declared at that moment (today, the one Polygon instance's login and
-- its network). A network or a login added later needs its own row, inserted by the migration
-- owner. The EVM endpoint can read the table (to be told why it was refused) and write nothing in it.
--
-- Additive: one table, and two functions replaced (the EVM endpoint's guard, and, at the end, the
-- order guard's mint expectation: REQ-AS-9).

CREATE TABLE evm_endpoint_networks (
    login   NAME NOT NULL,
    network TEXT NOT NULL REFERENCES networks (network),
    PRIMARY KEY (login, network)
);

COMMENT ON TABLE evm_endpoint_networks IS
    'F-009 S7: the EVM networks each login holding bezent_evm_endpoint may record. A login with no row records nothing.';

-- Today's deployments: each login that holds the role records the EVM networks declared now.
INSERT INTO evm_endpoint_networks (login, network)
SELECT u.rolname, n.network
FROM pg_auth_members m
JOIN pg_roles g ON g.oid = m.roleid AND g.rolname = 'bezent_evm_endpoint'
JOIN pg_roles u ON u.oid = m.member
CROSS JOIN networks n
WHERE u.rolcanlogin AND (m.inherit_option OR m.set_option)
  AND n.role = 'origin' AND n.address_format = 'evm';

GRANT SELECT ON evm_endpoint_networks TO bezent_evm_endpoint;

CREATE OR REPLACE FUNCTION evm_endpoint_guard_network() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    target TEXT;
BEGIN
    IF bezent_evm_endpoint_writer(TG_RELID) THEN
        target := bezent_row_network(TG_RELID, to_jsonb(NEW));
        IF NOT EXISTS (SELECT 1 FROM networks WHERE network = target AND address_format = 'evm') THEN
            RAISE EXCEPTION '% on % refused: the EVM endpoint records EVM networks only (REQ-OB-6, INV-8)',
                TG_TABLE_NAME, coalesce(target, 'no network')
                USING ERRCODE = 'BZ001';
        END IF;
        -- S7: and only the networks its own login is mapped to (DEC-8, one instance per network).
        -- Asked of the session's login and of the role it acts as, like the writer test itself:
        -- each of the two that is a login and an EVM writer must be mapped to the row's network.
        -- The role `bezent_evm_endpoint` itself cannot log in; it is acted as only by the owner
        -- (a drill), and carries no mapping.
        IF EXISTS (SELECT 1 FROM pg_roles u
                   WHERE u.rolname IN (session_user, current_user)
                     AND pg_has_role(u.oid, 'bezent_evm_endpoint', 'MEMBER')
                     AND u.rolcanlogin AND NOT u.rolsuper
                     AND NOT pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')
                     AND NOT EXISTS (SELECT 1 FROM evm_endpoint_networks e
                                     WHERE e.login = u.rolname AND e.network = target)) THEN
            RAISE EXCEPTION '% on % refused: the login is not an instance of that network; each EVM endpoint records its own network only (DEC-8, REQ-OB-6, INV-8)',
                TG_TABLE_NAME, target
                USING ERRCODE = 'BZ001';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- REQ-AS-9 (found by the M-2 boundary tests, issue #30): the mint order's expected amount was
-- floor(arrival / ratio) in NUMERIC, and PostgreSQL's NUMERIC division rounds its quotient to a
-- scale chosen for about sixteen significant digits. An arrival of 9999999999999999999999 BTCB
-- units against a ratio of 10^10 divides to 999999999999.9999999999999, which is held as
-- 1000000000000.000 and floors to 10^12, one more than the exact floor 999999999999. The store then
-- refused the core's correct mint (the deposit stuck, retried every cycle) and would have accepted
-- an order one minor unit over. The expectation is now the exact integer quotient, `div`, which
-- truncates and never rounds. 0050's guard otherwise, unchanged (ACL kept by CREATE OR REPLACE).
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
        SELECT div(src.amount, power(10::numeric, k.precision - a.precision))
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

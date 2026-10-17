-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-009 T-009-01 to T-009-06: the EVM endpoint's own role (SPEC §8.1 REQ-OB-6, §8.2 REQ-NX-1/2,
-- §8.3 REQ-EO-1; §9 INV-8; PLAN DEC-3, DEC-8; F-007 NOTES 27, issue #33).
--
-- Numbered 0035, after main's 0034 (issue #30, PR #44), so nothing of F-009 runs before main's
-- migrations on a fresh database; it was 0022 before PR #44 merged. Additive.
--
-- The EVM endpoint writes the same records as the Kreivo endpoint, in the same tables: observations
-- and their outcomes, read ranges and gaps, final heads, prepared transactions, signed bytes and
-- proofs of non-execution. Its login holds `bezent_evm_endpoint`, a member of `bezent_endpoint`
-- that inherits its grants (0015, 0019, 0021) and nothing more, and that cannot SET ROLE into it
-- (PR #39 review M1): the membership is granted WITH INHERIT TRUE, SET FALSE (PostgreSQL 16), and a
-- grant of it with SET fails the migration. The guard below asks of the session's login as well as
-- of the role it acts as, so a login that reached `bezent_endpoint` some other way (another grant,
-- or a server older than 16, where every member may SET ROLE) is still held to EVM networks.
--
-- One endpoint role served every network until now, so an endpoint's login could write any
-- network's records (F-007 NOTES 27). The EVM endpoint is the one that reads two providers over the
-- internet, so it is narrowed first: a login holding `bezent_evm_endpoint` may write the records of
-- EVM networks only (`networks.address_format = 'evm'`). It cannot write the issuing network's
-- observations, which tell the Kreivo prover a mint executed (INV-8), nor its proofs. The other
-- direction is 0034's: the Kreivo endpoint's role (`bezent_kreivo_endpoint`) records the issuing
-- network's observations only.
--
-- 0034 registers each endpoint role's scope in `endpoint_scopes`: a login holding
-- `bezent_endpoint` and no registered role records no observation or outcome. The EVM endpoint's
-- scope is the origin networks of address format `evm` (registered below).
--
-- The migration owner and a superuser keep writing everything, for drills and restores (NFR-7).
--
-- Conventions of the EVM endpoint, kept by its code, stated here for readers of the tables: an EVM
-- transaction does not expire (REQ-NX-1: on an origin network, non-execution is the nonce consumed
-- by a different transaction, or a final refusal), so its `prepared_transactions` row is valid from
-- its dry-run block to block 9223372036854775807, and it never has a `nonce-unconsumed` proof. The
-- lane is the delivery key's 20-byte address.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_evm_endpoint') THEN
        BEGIN
            CREATE ROLE bezent_evm_endpoint NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;

    IF current_user = 'bezent_evm_endpoint'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_evm_endpoint' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_evm_endpoint (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_evm_endpoint' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_evm_endpoint must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    -- The signer, the core and the services' role never hold it (REQ-AR-3).
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname = 'bezent_evm_endpoint'
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_signer', 'bezent_endpoint')) THEN
        RAISE EXCEPTION 'bezent_service, bezent_core, bezent_signer and bezent_endpoint must not be members of bezent_evm_endpoint'
            USING ERRCODE = 'BZ001';
    END IF;

    -- M1 (PR #39 review): inherit bezent_endpoint's grants, never SET ROLE into it. Re-granting
    -- updates the options of this grantor's grant; any grant of it with SET, by any grantor,
    -- fails the migration. Before PostgreSQL 16 every member may SET ROLE, and only the guard's
    -- session check below holds.
    IF current_setting('server_version_num')::int >= 160000 THEN
        BEGIN
            EXECUTE 'GRANT bezent_endpoint TO bezent_evm_endpoint WITH INHERIT TRUE, SET FALSE';
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- granted concurrently by another database's migration on the same server
        END;
        IF EXISTS (SELECT 1 FROM pg_auth_members m
                   JOIN pg_roles g ON g.oid = m.roleid
                   JOIN pg_roles u ON u.oid = m.member
                   WHERE g.rolname = 'bezent_endpoint' AND u.rolname = 'bezent_evm_endpoint'
                     AND (m.set_option OR NOT m.inherit_option)) THEN
            RAISE EXCEPTION 'bezent_evm_endpoint must hold bezent_endpoint WITH INHERIT TRUE, SET FALSE only (PR #39 M1, INV-8)'
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NOT EXISTS (SELECT 1 FROM pg_auth_members m
                      JOIN pg_roles g ON g.oid = m.roleid
                      JOIN pg_roles u ON u.oid = m.member
                      WHERE g.rolname = 'bezent_endpoint' AND u.rolname = 'bezent_evm_endpoint') THEN
        BEGIN
            GRANT bezent_endpoint TO bezent_evm_endpoint;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;
        END;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO bezent_evm_endpoint;

-- 0034: the networks whose observations and outcomes the EVM endpoint's login records.
INSERT INTO endpoint_scopes (endpoint_role, network_role, address_format)
    VALUES ('bezent_evm_endpoint', 'origin', 'evm');

-- Whether the insert is an EVM endpoint's: the session's login or the role it acts as holds
-- bezent_evm_endpoint, and that one is neither a superuser nor the table's owner (who write
-- everything, NFR-7). The session's login is asked too (M1): SET ROLE changes current_user, never
-- session_user, so a login holding bezent_evm_endpoint stays held to EVM networks whatever role it
-- switches to.
-- Every function here pins its search_path (PR #39 second review): a temporary table named
-- `networks` or `intents` must never stand in for the real one.
CREATE FUNCTION bezent_evm_endpoint_writer(relation OID) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT coalesce(bool_or(pg_has_role(u.oid, 'bezent_evm_endpoint', 'MEMBER')
                            AND NOT u.rolsuper
                            AND NOT pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = relation), 'MEMBER')),
                    false)
    FROM pg_roles u
    WHERE u.rolname IN (session_user, current_user)
$$;

-- The network a row of each guarded table is about. A prepared transaction is about its order's
-- network, not the network it names (PR #39 second review, MF1): a row labelled `polygon` for a
-- Kreivo order is a Kreivo row. 0036 also refuses any row whose network is not its order's.
CREATE FUNCTION bezent_row_network(relation OID, row_data JSONB) RETURNS TEXT
    LANGUAGE sql STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT CASE (SELECT relname FROM pg_class WHERE oid = relation)
        WHEN 'observation_outcomes' THEN
            (SELECT network FROM observations WHERE id = (row_data ->> 'observation_id')::bigint)
        WHEN 'signed_transactions' THEN
            (SELECT network FROM intents WHERE id = (row_data ->> 'intent_id')::bigint)
        WHEN 'prepared_transactions' THEN
            (SELECT network FROM intents WHERE id = (row_data ->> 'order_intent_id')::bigint)
        ELSE row_data ->> 'network'
    END
$$;

CREATE FUNCTION evm_endpoint_guard_network() RETURNS trigger
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
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observations_evm_endpoint_guard
    BEFORE INSERT ON observations
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER observation_outcomes_evm_endpoint_guard
    BEFORE INSERT ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER observation_ranges_evm_endpoint_guard
    BEFORE INSERT ON observation_ranges
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER final_heads_evm_endpoint_guard
    BEFORE INSERT ON final_heads
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER prepared_transactions_evm_endpoint_guard
    BEFORE INSERT ON prepared_transactions
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER signed_transactions_evm_endpoint_guard
    BEFORE INSERT ON signed_transactions
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();
CREATE TRIGGER non_execution_proofs_evm_endpoint_guard
    BEFORE INSERT ON non_execution_proofs
    FOR EACH ROW EXECUTE FUNCTION evm_endpoint_guard_network();

-- No function here is PUBLIC's (0017's F1). The trigger function fires without an EXECUTE grant;
-- the two helpers run as the inserting login inside it, so every service keeps them (as 0021 does
-- for bezent_writer_holds): a login refused an insert is refused by its grants or a guard, never by
-- a missing EXECUTE.
REVOKE EXECUTE ON FUNCTION
    bezent_evm_endpoint_writer(OID),
    bezent_row_network(OID, JSONB),
    evm_endpoint_guard_network()
FROM PUBLIC;
GRANT EXECUTE ON FUNCTION bezent_evm_endpoint_writer(OID), bezent_row_network(OID, JSONB)
    TO bezent_service;

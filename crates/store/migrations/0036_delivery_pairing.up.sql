-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Numbered 0036, after main's 0034 and F-009's 0035.
-- PR #39 second review MF1 and issue #38: the two steps of a delivery are paired in the store
-- (SPEC §9 INV-9, §8.3 REQ-EO-1, AC-I1.2; PLAN DEC-3, two-step delivery).
--
-- The core records a delivery order; an endpoint prepares it (prepared_transactions); the core then
-- records the signing intent over the prepared bytes, which the signer signs with the key bound to
-- the intent's network. Until now only code tied the three together, so a login that may insert a
-- prepared transaction could pair bytes with an order of another network: an EVM endpoint's login
-- could insert a `polygon` row for a Kreivo order, and the core would record a Kreivo signing
-- intent over bytes the EVM endpoint chose, for the Kreivo key to sign (MF1). And nothing in the
-- store kept an intent for order A from naming the prepared bytes of order B (#38).
--
-- Here, whoever inserts:
--
--   * a prepared transaction answers a delivery order (an intent with an `order`), on that order's
--     network;
--   * an endpoint prepares only the orders of its kind of network: a login holding
--     bezent_evm_endpoint the orders of EVM networks only (0035 asks the same through the order's
--     network), and any other endpoint login (the Kreivo endpoint's) never an EVM network's;
--   * a signing intent names a prepared transaction of its own order: the same network (the
--     intent's, the order's and the prepared row's), transfer, attempt, purpose, on-chain reference
--     and reference scope.
--
-- The migration owner and a superuser are held to the prepared transaction's pairing too, and
-- exempt from the endpoint-kind check, as in 0035. A session of the owner or a superuser is exempt
-- from the signing intent's check: a restore (NFR-7) loads `intents` before
-- `prepared_transactions`, which references it, so a restored signing intent would otherwise name
-- a prepared row not loaded yet. Every service's login, the core's included, is held to it.
-- No row already in a table is touched or checked: the tables are append-only, and these rules
-- hold from here on.

-- Whether the inserting session acts as an endpoint other than the EVM endpoint: its login or the
-- role it acts as holds bezent_endpoint, and is neither a superuser nor the table's owner. Asked of
-- the session's login as well as of the current role, as 0035 does (M1 of the first review).
CREATE FUNCTION bezent_endpoint_writer(relation OID) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT coalesce(bool_or(pg_has_role(u.oid, 'bezent_endpoint', 'MEMBER')
                            AND NOT u.rolsuper
                            AND NOT pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = relation), 'MEMBER')),
                    false)
    FROM pg_roles u
    WHERE u.rolname IN (session_user, current_user)
$$;

CREATE FUNCTION prepared_transactions_guard_order() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    order_network TEXT;
    is_order      BOOLEAN;
    evm           BOOLEAN;
BEGIN
    SELECT i.network, i.payload ? 'order' AND NOT i.payload ? 'prepared'
        INTO order_network, is_order
        FROM intents i WHERE i.id = NEW.order_intent_id;
    IF NOT coalesce(is_order, false) THEN
        RAISE EXCEPTION 'prepared transaction refused: intent % is not a delivery order (INV-9, DEC-3)',
            NEW.order_intent_id
            USING ERRCODE = 'BZ001';
    END IF;
    IF NEW.network IS DISTINCT FROM order_network THEN
        RAISE EXCEPTION 'prepared transaction refused: it names network %, and its order % is on % (INV-9)',
            NEW.network, NEW.order_intent_id, order_network
            USING ERRCODE = 'BZ001';
    END IF;
    evm := EXISTS (SELECT 1 FROM networks WHERE network = order_network AND address_format = 'evm');
    IF bezent_evm_endpoint_writer(TG_RELID) THEN
        IF NOT evm THEN
            RAISE EXCEPTION 'prepared transaction refused: the EVM endpoint prepares the orders of EVM networks only, and order % is on % (INV-9, REQ-OB-6)',
                NEW.order_intent_id, order_network
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF bezent_endpoint_writer(TG_RELID) AND evm THEN
        RAISE EXCEPTION 'prepared transaction refused: this endpoint does not prepare the orders of EVM network % (INV-9)',
            order_network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER prepared_transactions_guard_order
    BEFORE INSERT ON prepared_transactions
    FOR EACH ROW EXECUTE FUNCTION prepared_transactions_guard_order();

-- #38: a signing intent names a prepared transaction of its own order. It runs as the owner, so a
-- core login that may not read prepared_transactions is checked, not refused for privilege; who
-- inserts is the session's login (session_user), which SECURITY DEFINER does not change.
CREATE FUNCTION intents_guard_prepared() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NEW.payload ? 'prepared'
       AND NOT EXISTS (SELECT 1 FROM pg_roles u
                       WHERE u.rolname = session_user
                         AND (u.rolsuper
                              OR pg_has_role(u.oid, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')))
       AND NOT EXISTS (
        SELECT 1 FROM prepared_transactions p
        JOIN intents o ON o.id = p.order_intent_id
        WHERE p.id::text = NEW.payload ->> 'prepared'
          AND o.payload ? 'order'
          AND p.network = NEW.network AND o.network = NEW.network
          AND o.transfer_id IS NOT DISTINCT FROM NEW.transfer_id
          AND o.attempt = NEW.attempt
          AND o.purpose = NEW.purpose
          AND o.on_chain_reference IS NOT DISTINCT FROM NEW.on_chain_reference
          AND o.reference_scope IS NOT DISTINCT FROM NEW.reference_scope) THEN
        RAISE EXCEPTION 'signing intent refused: prepared transaction % is not one of its own order on % (INV-9, issue #38)',
            NEW.payload ->> 'prepared', NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_guard_prepared
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION intents_guard_prepared();

-- No function here is PUBLIC's (0017's F1). The trigger functions fire without an EXECUTE grant;
-- the role check runs as the inserting login inside one, so every service keeps it (as 0021 and
-- 0035 do).
REVOKE EXECUTE ON FUNCTION
    bezent_endpoint_writer(OID),
    prepared_transactions_guard_order(),
    intents_guard_prepared()
FROM PUBLIC;
GRANT EXECUTE ON FUNCTION bezent_endpoint_writer(OID) TO bezent_service;

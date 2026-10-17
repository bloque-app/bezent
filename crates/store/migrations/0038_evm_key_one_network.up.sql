-- SPDX-License-Identifier: GPL-3.0-or-later
-- PR #39 third review SF-B (SPEC REQ-KY-4, INV-9): one EVM key binds to exactly one network.
--
-- An EVM key is the same key on every EVM chain, and its address the same lane. Bound to two
-- networks, one lane key could sign, for a Polygon order, bytes another chain takes, and the
-- transaction would execute there, from a lane that may hold the delivery role there too. The
-- signer refuses such a configuration and reads every EVM intent's chain id; here the store holds
-- it as well, whoever inserts:
--
--   * a signature by an EVM key (`secp256k1-evm`) is refused when the same key, by its id or by
--     its public key, already signed for another network;
--   * a prepared transaction on an EVM network is refused when the same lane, by its address or
--     by its key id, already holds a prepared transaction on another EVM network.
--
-- Concurrent inserts for the same key or lane are serialised on an advisory lock, so two
-- networks cannot both pass. No row already in a table is touched or checked: the tables are
-- append-only, and the rule holds from here on.

-- Fires after signatures_from_intent (triggers fire in name order), which copies the intent's
-- network onto the row.
CREATE FUNCTION signatures_one_network_per_evm_key() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NEW.scheme IS DISTINCT FROM 'secp256k1-evm' THEN
        RETURN NEW;
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('bezent-evm-key', 0));
    IF EXISTS (SELECT 1 FROM signatures s
               WHERE s.scheme = 'secp256k1-evm'
                 AND (s.public_key = NEW.public_key OR s.key_id = NEW.key_id)
                 AND s.network IS DISTINCT FROM NEW.network) THEN
        RAISE EXCEPTION 'signature refused: EVM key % already signs for another network than % (REQ-KY-4, INV-9)',
            NEW.key_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER signatures_one_network_per_evm_key
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION signatures_one_network_per_evm_key();

CREATE FUNCTION prepared_transactions_one_network_per_evm_lane() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM networks WHERE network = NEW.network AND address_format = 'evm') THEN
        RETURN NEW;
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('bezent-evm-lane', 0));
    IF EXISTS (SELECT 1 FROM prepared_transactions p
               JOIN networks n ON n.network = p.network AND n.address_format = 'evm'
               WHERE (p.lane = NEW.lane OR p.key_id = NEW.key_id)
                 AND p.network <> NEW.network) THEN
        RAISE EXCEPTION 'prepared transaction refused: EVM lane % (key %) already prepares for another network than % (REQ-KY-4, INV-9)',
            encode(NEW.lane, 'hex'), NEW.key_id, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER prepared_transactions_one_network_per_evm_lane
    BEFORE INSERT ON prepared_transactions
    FOR EACH ROW EXECUTE FUNCTION prepared_transactions_one_network_per_evm_lane();

-- No function here is PUBLIC's (0017's F1); the trigger functions fire without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION
    signatures_one_network_per_evm_key(),
    prepared_transactions_one_network_per_evm_lane()
FROM PUBLIC;

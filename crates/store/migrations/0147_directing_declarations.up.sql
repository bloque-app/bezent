-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-11 (amendment 0029; SPEC REQ-AS-11, REQ-BK-5, §7.E): the declaration of directing code
-- for a reserve, which the statement publishes with the reserve addresses, and the one observation
-- kind the alert on an origin token's upgrade needs.
--
--   * `directing_code`: the explicit, recorded declaration REQ-AS-11 asks for: the network, the
--     contract's address, the admitted token it moves, the reserve it moves it to (the reserve of the
--     asset on that network, `reserves`), and the limits it enforces, with the first block its events
--     count from and the operator's reference. Written by the migration owner, as every declaration
--     is; append-only, since the code it declares cannot change (it has no owner, no upgrade and no
--     proxy: features/F-018-directed-deposits/plan.md, "No owner, no upgrade, no proxy"). A new
--     contract is a new row. The core reads it for the daily statement (REQ-BK-5, §7.E). The EVM
--     endpoint still reads its own `[[directing]]` configuration to watch the contract; the two are
--     the same declaration written twice, and the deployment keeps them equal.
--   * `token_upgrade`, an origin network's observation kind: an admitted origin token's
--     `Upgraded(address)` event (EIP-1967), its implementation changed. Recorded by the EVM endpoint,
--     bindable by nothing (`source_bindable()` names no such kind), and paged by the core. Whether
--     directed intake should also hold on it is an open ruling (`d-f018-usdc-upgrade`); nothing here
--     holds anything.

CREATE TABLE directing_code (
    asset       TEXT NOT NULL,
    network     TEXT NOT NULL,
    address     TEXT NOT NULL CHECK (address <> ''),
    token       TEXT NOT NULL,
    -- The limits it enforces, in the token's minor units (REQ-AS-11, REQ-LM-2).
    minimum     minor_units NOT NULL CHECK (minimum >= 1),
    maximum     minor_units NOT NULL,
    -- The first block whose events count (the contract's deployment block, or later).
    from_block  BIGINT NOT NULL CHECK (from_block >= 0),
    -- The operator's reference for the declaration, as for surplus_funding_addresses (REQ-BK-5).
    reference   TEXT NOT NULL CHECK (reference <> ''),
    declared_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (network, address),
    FOREIGN KEY (asset, network) REFERENCES reserves (asset, network),
    FOREIGN KEY (network, token) REFERENCES admitted_tokens (network, token),
    CONSTRAINT directing_code_limits CHECK (maximum >= minimum)
);

COMMENT ON TABLE directing_code IS
    'REQ-AS-11: directing code declared for a reserve: its network, address, the admitted token it '
    'moves, the reserve (asset, network) it moves it to, and its limits. It holds no value. '
    'Published with the reserve addresses in every statement (REQ-BK-5, §7.E).';

-- The token is the asset's on that network, and the code is not the reserve itself.
CREATE FUNCTION directing_code_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM admitted_tokens k
                   WHERE k.network = NEW.network AND k.token = NEW.token AND k.asset = NEW.asset) THEN
        RAISE EXCEPTION 'directing code % on % refused: % is not an origin token admitted for % there (REQ-AS-11)',
            NEW.address, NEW.network, NEW.token, NEW.asset
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM reserves r JOIN networks n ON n.network = r.network
               WHERE r.asset = NEW.asset AND r.network = NEW.network
                 AND address_matches(n.address_format, r.address, NEW.address)) THEN
        RAISE EXCEPTION 'directing code % on % refused: it is the reserve address; directing code holds no value (REQ-AS-11)',
            NEW.address, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER directing_code_guard
    BEFORE INSERT ON directing_code
    FOR EACH ROW EXECUTE FUNCTION directing_code_guard();
CREATE TRIGGER directing_code_append_only
    BEFORE UPDATE OR DELETE ON directing_code
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-AS-11');
CREATE TRIGGER directing_code_no_truncate
    BEFORE TRUNCATE ON directing_code
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-AS-11');

GRANT SELECT ON directing_code TO bezent_core;

-- ---------------------------------------------------------------------------------------------
-- The origin token's upgrade: one more kind of an origin network.

ALTER DOMAIN observation_kind DROP CONSTRAINT observation_kind_check;
ALTER DOMAIN observation_kind ADD CONSTRAINT observation_kind_check
    CHECK (VALUE IN (
        'arrival', 'other_arrival', 'convertible_arrival', 'conversion_output',
        'conversion_input', 'payout_leg', 'outflow', 'self_transfer',
        'directed_arrival', 'unpaired_direction', 'token_upgrade',
        'mint', 'redemption', 'refund_mint', 'control',
        'transaction', 'other'
    ));

CREATE OR REPLACE FUNCTION observation_kind_on_network(p_role TEXT, p_kind TEXT) RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE p_role
        WHEN 'origin' THEN p_kind IN (
            'arrival', 'other_arrival', 'convertible_arrival', 'conversion_output',
            'conversion_input', 'payout_leg', 'outflow', 'self_transfer',
            'directed_arrival', 'unpaired_direction', 'token_upgrade', 'transaction', 'other')
        WHEN 'issuing' THEN p_kind IN (
            'mint', 'redemption', 'refund_mint', 'control', 'transaction', 'other')
        ELSE p_kind = 'other'
    END
$$;

CREATE OR REPLACE FUNCTION observation_kind_for_event(p_event TEXT) RETURNS TEXT
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE p_event
        WHEN 'Arrival' THEN 'arrival'
        WHEN 'DirectedArrival' THEN 'directed_arrival'
        WHEN 'UnpairedDirection' THEN 'unpaired_direction'
        WHEN 'TokenUpgraded' THEN 'token_upgrade'
        WHEN 'OtherArrival' THEN 'other_arrival'
        WHEN 'ConvertibleArrival' THEN 'convertible_arrival'
        WHEN 'Conversion' THEN 'conversion_output'
        WHEN 'ConversionInput' THEN 'conversion_input'
        WHEN 'Payout' THEN 'payout_leg'
        WHEN 'Outflow' THEN 'outflow'
        WHEN 'SelfTransfer' THEN 'self_transfer'
        WHEN 'Minted' THEN 'mint'
        WHEN 'Redeemed' THEN 'redemption'
        WHEN 'RefundMinted' THEN 'refund_mint'
        WHEN 'Paused' THEN 'control'
        WHEN 'Unpaused' THEN 'control'
        WHEN 'BoundsSet' THEN 'control'
        ELSE 'other'
    END
$$;

-- ---------------------------------------------------------------------------------------------
-- What the core's F-018 alerts read each minute (services/core/src/directed_alerts.rs), each by an
-- index that holds only what they read: the mints of the trailing day (a transfer's `Delivered`), the
-- two observation kinds that page one by one, and the alerts' own memory in the log (one entry per
-- page or clearance, a handful a day).
--
-- Deploying it: plain `CREATE INDEX`es, each taking a SHARE lock on its table for the whole scan of
-- it, as 0100's does on `event_log`; the migrator runs with the services stopped, so they block
-- nobody, but on a large store the build time is time the bridge is down.
CREATE INDEX transitions_delivered_seen ON transitions (seen_at) WHERE to_state = 'Delivered';
CREATE INDEX observations_paged_kinds ON observations (id)
    WHERE kind IN ('unpaired_direction', 'token_upgrade');
CREATE INDEX event_log_directed_alerts ON event_log ((payload ->> 'directed_alert'), seq DESC)
    WHERE (payload ->> 'directed_alert') IS NOT NULL;

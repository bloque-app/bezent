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
--     the same declaration written twice, and the deployment keeps them equal. On an `evm` network
--     its address is kept in lower case (review L5 of PR #139), so the key (network, address) is
--     unique without case there, as the network compares addresses; it is none of the network's
--     reserve addresses and none of its admitted tokens. (That it is not the reference emitter, or
--     any other of the vault's contracts, the EVM endpoint checks of its `[[directing]]` at start:
--     the store does not hold those.) Its `declared_at` is the clock's when the row is written, not
--     the transaction's start (review L4 of PR #139): the statement lists the code declared by its
--     evaluation's time, and a declaration whose transaction began before that time but committed
--     after the statement read the table would otherwise appear in a statement re-derived from the
--     same evaluation, and not in the first.
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
    -- The clock's time of the write (directing_code_guard() stamps it), never the transaction's
    -- start: see the head of this file (review L4 of PR #139).
    declared_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (network, address),
    FOREIGN KEY (asset, network) REFERENCES reserves (asset, network),
    FOREIGN KEY (network, token) REFERENCES admitted_tokens (network, token),
    CONSTRAINT directing_code_limits CHECK (maximum >= minimum)
);

COMMENT ON TABLE directing_code IS
    'REQ-AS-11: directing code declared for a reserve: its network, address, the admitted token it '
    'moves, the reserve (asset, network) it moves it to, and its limits. It holds no value. '
    'Published with the reserve addresses in every statement (REQ-BK-5, §7.E).';

-- The token is the asset's on that network; on an `evm` network the address is kept in lower case;
-- and the code is none of the network's reserve addresses, whichever asset's, and none of its
-- admitted tokens (review L5 of PR #139). Its time is the clock's (review L4 of PR #139): a
-- declaration states when it was written, not when its transaction began. A time not yet past when
-- the transaction began (the default, `now()`, or later) is replaced by the clock's; one already past
-- is kept, as a restore (NFR-7) writes each row's own.
CREATE FUNCTION directing_code_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
DECLARE
    fmt TEXT;
BEGIN
    SELECT n.address_format INTO fmt FROM networks n WHERE n.network = NEW.network;
    IF fmt = 'evm' THEN
        NEW.address := lower(NEW.address);
    END IF;
    IF NEW.declared_at IS NULL OR NEW.declared_at >= now() THEN
        NEW.declared_at := clock_timestamp();
    END IF;
    IF NOT EXISTS (SELECT 1 FROM admitted_tokens k
                   WHERE k.network = NEW.network AND k.token = NEW.token AND k.asset = NEW.asset) THEN
        RAISE EXCEPTION 'directing code % on % refused: % is not an origin token admitted for % there (REQ-AS-11)',
            NEW.address, NEW.network, NEW.token, NEW.asset
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM reserves r
               WHERE r.network = NEW.network AND address_matches(fmt, r.address, NEW.address)) THEN
        RAISE EXCEPTION 'directing code % on % refused: it is a reserve address there; directing code holds no value (REQ-AS-11)',
            NEW.address, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM admitted_tokens k
               WHERE k.network = NEW.network AND address_matches(fmt, k.token, NEW.address)) THEN
        RAISE EXCEPTION 'directing code % on % refused: it is an origin token admitted there, not code that moves one (REQ-AS-11)',
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

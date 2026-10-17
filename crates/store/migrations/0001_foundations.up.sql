-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-001 T-001-03/04: shared domains and guard functions (PLAN DEC-4).
--
-- Every rule below is enforced by the store, not by application code: that is the layer SPEC §9
-- names for INV-5, INV-10, INV-11 and INV-17 (REQ-AR-5 forbids a weaker one).
-- Every refusal raises SQLSTATE 'BZ001' with the spec id it enforces in the message.

-- INV-11: an exact, non-negative integer count of minor units, at most u128::MAX. A value with a
-- fractional part, a scale (2.0), NaN or infinity is refused, never rounded: NUMERIC(39,0) would
-- round 1.5 to 2 silently, so the domain is plain NUMERIC with a scale check instead.
CREATE DOMAIN minor_units AS NUMERIC
    CHECK (
        VALUE >= 0
        AND VALUE <= 340282366920938463463374607431768211455
        AND scale(VALUE) = 0
    );

-- Where in a block a movement is (bezent_core::Position).
CREATE DOMAIN position_kind AS TEXT CHECK (VALUE IN ('transaction', 'log'));

-- The states of SPEC §5.4.
CREATE DOMAIN transfer_state AS TEXT
    CHECK (VALUE IN (
        'Received', 'Refused', 'Accepted', 'Source-Final', 'Delivering', 'Delivered',
        'Completed', 'Refunding', 'Refunded', 'Expired', 'Withdrawn', 'Needs-Review'
    ));

-- The named conditions of SPEC §10 (CND-AwaitingInstruction is withdrawn by amendment 0007).
CREATE DOMAIN transfer_condition AS TEXT
    CHECK (VALUE IN (
        'CND-Unassociated', 'CND-ReserveShort', 'CND-FeeShort',
        'CND-MintWindow', 'CND-DestinationRefused', 'CND-AmountMismatch', 'CND-Withdrawn',
        'CND-Paused', 'CND-AwaitingConversion'
    ));

-- The named errors of SPEC §10.
CREATE DOMAIN refusal_error AS TEXT
    CHECK (VALUE IN (
        'ERR-NotAuthorised', 'ERR-ReferenceConflict', 'ERR-UnknownRoute', 'ERR-RoutePaused',
        'ERR-BelowMinimum', 'ERR-AboveMaximum', 'ERR-WindowExceeded', 'ERR-ExcessPrecision',
        'ERR-SourceNotFound', 'ERR-SourceMismatch', 'ERR-SourceConsumed',
        'ERR-InvalidDestination', 'ERR-Overloaded'
    ));

-- The terminal states of SPEC §5.4 (INV-5).
CREATE FUNCTION is_terminal_state(s transfer_state) RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$ SELECT s IN ('Refused', 'Completed', 'Refunded', 'Expired', 'Withdrawn') $$;

-- Refuses UPDATE, DELETE and TRUNCATE on an append-only table. TG_ARGV[0] names the spec ids.
CREATE FUNCTION refuse_append_only_change() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    RAISE EXCEPTION '% on % is refused: the table is append-only (%)', TG_OP, TG_TABLE_NAME, TG_ARGV[0]
        USING ERRCODE = 'BZ001';
END
$$;

COMMENT ON DOMAIN minor_units IS
    'INV-11: an exact integer count of minor units, 0..=u128::MAX. Each amount column says whose '
    'minor units it counts (REQ-AS-4).';

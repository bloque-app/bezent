-- Copyright (c) 2026 Pagos Cube LLC
-- SPDX-License-Identifier: GPL-3.0-or-later
-- Reversal for tests only (T-001-03): F-018, T-018-11, the declaration of directing code and the
-- origin token's upgrade kind, back to 0143's. Rows of that kind are removed first by the test that
-- applies this, as the domain would refuse them.
DROP INDEX event_log_directed_alerts;
DROP INDEX observations_paged_kinds;
DROP INDEX transitions_delivered_seen;
DROP TABLE directing_code;
DROP FUNCTION directing_code_guard();

ALTER DOMAIN observation_kind DROP CONSTRAINT observation_kind_check;
ALTER DOMAIN observation_kind ADD CONSTRAINT observation_kind_check
    CHECK (VALUE IN (
        'arrival', 'other_arrival', 'convertible_arrival', 'conversion_output',
        'conversion_input', 'payout_leg', 'outflow', 'self_transfer',
        'directed_arrival', 'unpaired_direction',
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
            'directed_arrival', 'unpaired_direction', 'transaction', 'other')
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

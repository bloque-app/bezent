-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-018, T-018-05 (amendment 0029, proposed; Pablo approved building F-018 on 2026-11-08): the two
-- observation kinds the EVM endpoint records for directing code, and nothing else.
--
--   directed_arrival     an admitted origin token's transfer into the reserve, paired with a declared
--                        directing contract's `Directed` log right after it, in the same transaction
--   unpaired_direction   a declared directing contract's `Directed` log that pairs with no such transfer:
--                        an anomaly, recorded and paged on
--
-- Both are kinds of an origin network. `source_bindable()` is untouched, so neither kind binds a
-- deposit or a refund, whoever writes it: its two carve-outs (the core's directed deposit, the
-- core's return) and `directed_deposit()` are T-018-06's.

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

-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-014 (issue #14; REQ-ST-6, AC-H3.1; PLAN DEC-18; amendment 0025, `d-fee-position-scope`): the Kreivo
-- endpoint records the issuing network's fee position.
--
-- 0110 made fee positions observable for the EVM networks and left the issuing network's open: which amount
-- of a Substrate account is its fee position. It is ruled (2026-11-06): the delivery account's free balance
-- less its frozen balance, at the final head the endpoint records, read at the same head as the supply
-- readings. The table, its guards (REQ-OB-5: a recorded final head; REQ-OB-6 and INV-8: the writer's own
-- networks only, by `endpoint_guard_scope`, which holds the Kreivo endpoint to the issuing network) and
-- `ops_observables()` are 0110's and do not change: the amount is in the native token's smallest unit
-- (planck for KSM), exact, NULL when unknown. What this migration adds is the Kreivo endpoint's grant to
-- read and append, so that its row of `fee_position_amount` stops being unknown for want of a writer.
--
-- The EVM endpoint's login is still held to EVM networks (`evm_endpoint_guard_network`) and the Kreivo
-- endpoint's to the issuing network (`endpoint_guard_scope`): each refuses the other's network with BZ001.

GRANT SELECT, INSERT ON fee_position_readings TO bezent_kreivo_endpoint;

COMMENT ON TABLE fee_position_readings IS
    'REQ-ST-6, AC-H3.1, DEC-18: the native balance of a network''s delivery account at a final head '
    'its endpoint recorded (an EVM network: the balance; the issuing network: free less frozen, 0130). '
    'NULL amount is unknown, with its reason. Append-only; only the network''s endpoint records it.';

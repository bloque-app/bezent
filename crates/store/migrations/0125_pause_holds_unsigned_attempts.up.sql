-- SPDX-License-Identifier: GPL-3.0-or-later
-- Amendment 0022, A1b (`d-pause-0022`, ruled 2026-11-06; `REQ-GV-1`, `INV-15`, `NFR-8`, `AC-I2.1`,
-- `AC-I1.2`): new work is everything before an attempt's first recorded signature, so a signing
-- intent the core recorded before a pause, and that no signature answers yet, is held by the pause
-- and is not signed. Until now the core held the transfer (`gate`) but the signer read no pause, so
-- such an intent was signed at the endpoint's next poll, or, on an origin network, whenever a
-- stopped endpoint returned, with no bound.
--
-- One function, for the signer, which reads neither `pauses` nor `transfers` and gains no privilege
-- on either: it learns only held or not, for the intent it is asked to sign. Additive; every other
-- grant stays as it was.
--
--   * Held: an intent of purpose `mint` or `payout`, of a deposit or a withdrawal, whose route a row
--     of `pauses` covers (a row silent on a dimension matches every value of it, as the core's
--     `route_paused` reads it: the route is the asset, the deposit's source network or the
--     withdrawal's destination network, and the direction, the transfer's kind), and no intent of
--     the same transfer and attempt has a signature. Every pause, not only a compromise's: an
--     operator's, the reserve check's and the store's.
--   * Not held: an attempt with a recorded signature (in flight), a replay of a recorded signature
--     (the signer returns it before it asks), a refund mint or a refund's payout (a refund completes
--     work in flight, F-006 NOTES 1), a cancel (`REQ-NX-4`), a statement (`REQ-PR-1`), and an intent
--     with no transfer.
--   * Read at `READ COMMITTED` inside the signer's own transaction, under its advisory lock, so a
--     pause that commits before the read holds every signature after it (`NFR-8`).

CREATE FUNCTION pause_holds_intent(p_intent BIGINT) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1
        FROM intents i
        JOIN transfers t ON t.id = i.transfer_id
        WHERE i.id = p_intent
          AND i.purpose IN ('mint', 'payout')
          AND t.kind IN ('deposit', 'withdrawal')
          AND NOT EXISTS (
              SELECT 1 FROM intents j JOIN signatures s ON s.intent_id = j.id
              WHERE j.transfer_id = i.transfer_id AND j.attempt = i.attempt)
          AND EXISTS (
              SELECT 1 FROM pauses p
              WHERE (p.asset IS NULL OR p.asset = t.asset)
                AND (p.network IS NULL OR p.network =
                     CASE t.kind WHEN 'deposit' THEN t.source_network
                                 ELSE t.destination_network END)
                AND (p.direction IS NULL OR p.direction = t.kind)))
$$;

COMMENT ON FUNCTION pause_holds_intent(BIGINT) IS
    'Amendment 0022 A1b (REQ-GV-1, INV-15): whether a pause holds signing intent p_intent, a mint or '
    'payout of a deposit or withdrawal whose attempt has no recorded signature. The signer''s only read '
    'of pauses and transfers.';

REVOKE EXECUTE ON FUNCTION pause_holds_intent(BIGINT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION pause_holds_intent(BIGINT) TO bezent_signer;

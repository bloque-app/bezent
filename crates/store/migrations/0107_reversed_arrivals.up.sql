-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-011: an arrival already reversed. WORKING ASSUMPTION, amendment 0016 A3, proposed and NOT ruled:
-- while a reversal of it stands unrestored, an arrival binds nothing, and a deposit bound before the
-- reversal holds unminted (the core's gate). Kept in its own migration so that a contrary ruling
-- removes this file and the core's and the ingress's one call each.

-- Whether a reversal of the arrival report at this coordinate stands unrestored. False at any other
-- coordinate, on any backend. A reversal counts as soon as it is taken in, recorded or not: a row that
-- is not verified can only hold. A restoration counts only once the core has recorded it as an
-- observation, which is after it checked its signature: a row that is not verified must not release
-- what a reversal holds. Runs as its caller: the ingress and the core read the reports.
CREATE FUNCTION attested_arrival_reversed(p_network TEXT, p_block BIGINT, p_hash BYTEA)
    RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM attested_reports a
        JOIN attested_reports v ON v.network = a.network AND v.kind = 'reversal'
                               AND v.body ->> 'reverses' = a.report_id
        WHERE a.network = p_network AND a.seq = p_block AND a.digest = p_hash
          AND a.kind = 'arrival'
          AND NOT EXISTS (SELECT 1 FROM attested_reports r
                          JOIN observations o ON o.network = r.network
                                             AND o.block_number = r.seq
                                             AND o.block_hash = r.digest
                          WHERE r.network = v.network AND r.kind = 'restoration'
                            AND r.body ->> 'restores' = v.report_id))
$$;

-- The binding, whoever writes it: a deposit, or the refund of an unassociated arrival.
CREATE FUNCTION transfers_refuse_reversed_source() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NEW.source_network IS NOT NULL
       AND (NEW.kind = 'deposit' OR (NEW.kind = 'refund' AND NEW.refund_of IS NULL))
       AND attested_arrival_reversed(NEW.source_network, NEW.source_block_number,
                                     NEW.source_block_hash) THEN
        RAISE EXCEPTION '% refused: its arrival was reversed and is not restored (REQ-BE-6, amendment 0016 A3)',
            NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_refuse_reversed_source
    BEFORE INSERT ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_refuse_reversed_source();

REVOKE EXECUTE ON FUNCTION
    attested_arrival_reversed(TEXT, BIGINT, BYTEA),
    transfers_refuse_reversed_source()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION attested_arrival_reversed(TEXT, BIGINT, BYTEA)
    TO bezent_core, bezent_ingress;

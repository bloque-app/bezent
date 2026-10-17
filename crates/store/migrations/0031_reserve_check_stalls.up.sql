-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-004, review of PR #40: the reserve check never stops running without anyone knowing (INV-1,
-- NFR-5, REQ-IC-2), and surplus contributions are derived from chain data and the operator's
-- declarations, never from the book alone (REQ-BK-5, REQ-BK-8). Additive to 0030.
--
--   * book_quarantine: a final observation the book cannot post (the store refused its entry) is
--     set aside, once, with a page and a pause of its asset, so it never stops the core's cycle or
--     the reserve check again (review M1). Append-only; the operator resolves it (a correction, or
--     lifting the pause once the network and the book agree again).
--   * networks.reserve_point_lag_seconds: how far, in block time, the point the reserve check reads
--     a network at may lag the network's latest recorded final head (one minute plus the network's
--     finality time). Beyond it the evaluation diverges; an evaluation that cannot be made for
--     longer than it pauses the asset's routes too (review M2, REQ-IC-2: a finality stall, an
--     observation gap).
--   * pauses: the core may also pause an asset's routes under REQ-IC-2, naming either the
--     evaluation that could not be made for longer than the bound or the quarantined observation.
--     Still only the asset's routes, still never lifted by the core.
--   * surplus_funding_addresses: the addresses the operator declares it funds a reserve from. A
--     final arrival at the reserve from one of them is a surplus contribution, from chain data
--     anyone can read; every other arrival is owed mint (§5.3; review S1). Declared by the owner
--     (no service login writes it) and published in every statement (REQ-BK-5, REQ-BK-6).

-- ---------------------------------------------------------------------------------------------
-- Quarantine (review M1).

CREATE TABLE book_quarantine (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    observation_id BIGINT NOT NULL UNIQUE REFERENCES observations (id),
    -- The asset the observation moves, when it can be told; NULL pauses nothing but still pages.
    asset          TEXT REFERENCES assets (asset),
    reason         TEXT NOT NULL CHECK (reason <> ''),
    recorded_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE book_quarantine IS
    'REQ-IC-2, INV-1: final observations the book could not post. Each is set aside once, pages a '
    'human and pauses its asset; the core and the reserve check carry on with everything else.';

CREATE TRIGGER book_quarantine_append_only
    BEFORE UPDATE OR DELETE ON book_quarantine
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-IC-2');
CREATE TRIGGER book_quarantine_no_truncate
    BEFORE TRUNCATE ON book_quarantine
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-IC-2');

-- ---------------------------------------------------------------------------------------------
-- The lag bound per network (review M2; NFR-5, NFR-6, REQ-IC-2).

ALTER TABLE networks
    ADD COLUMN reserve_point_lag_seconds INTEGER NOT NULL DEFAULT 60
        CHECK (reserve_point_lag_seconds > 0);

COMMENT ON COLUMN networks.reserve_point_lag_seconds IS
    'NFR-5, REQ-IC-2: the most, in block time, the final point the reserve check reads this network '
    'at may lag its latest recorded final head: one minute plus the network''s finality time. '
    'Beyond it the evaluation diverges; an evaluation that cannot be made for longer pauses too.';

-- ---------------------------------------------------------------------------------------------
-- The core's pauses (REQ-BK-1, REQ-IC-2).

ALTER TABLE pauses
    ADD COLUMN quarantine_id BIGINT REFERENCES book_quarantine (id),
    DROP CONSTRAINT pauses_cause_check,
    ADD CONSTRAINT pauses_cause_known CHECK (cause IN ('REQ-BK-1', 'REQ-IC-2')),
    DROP CONSTRAINT pauses_cause_names_its_evaluation,
    ADD CONSTRAINT pauses_cause_names_its_reason CHECK (
        (cause IS NULL AND evaluation_id IS NULL AND quarantine_id IS NULL)
        OR (cause = 'REQ-BK-1' AND evaluation_id IS NOT NULL AND quarantine_id IS NULL)
        OR (cause = 'REQ-IC-2' AND (evaluation_id IS NULL) <> (quarantine_id IS NULL)));

CREATE OR REPLACE FUNCTION pauses_guard_insert() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF NEW.asset IS NULL OR NEW.network IS NOT NULL OR NEW.direction IS NOT NULL
       OR NOT (
           -- A divergence of this asset (REQ-BK-1).
           (NEW.cause = 'REQ-BK-1' AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'diverges'))
           -- An evaluation of this asset that could not be made, or an observation of it the book
           -- could not post (REQ-IC-2).
           OR (NEW.cause = 'REQ-IC-2' AND NEW.quarantine_id IS NULL AND EXISTS (
               SELECT 1 FROM reserve_evaluations v
               WHERE v.id = NEW.evaluation_id AND v.asset = NEW.asset AND v.outcome = 'incomplete'))
           OR (NEW.cause = 'REQ-IC-2' AND NEW.evaluation_id IS NULL AND EXISTS (
               SELECT 1 FROM book_quarantine q
               WHERE q.id = NEW.quarantine_id AND q.asset = NEW.asset))) THEN
        RAISE EXCEPTION 'pause refused: the core pauses only an asset''s routes, on a divergence of that asset, an evaluation of it that could not be made, or an observation of it the book could not post (REQ-BK-1, REQ-IC-2)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- Operator funding addresses (review S1; REQ-BK-5, REQ-BK-6, REQ-BK-8).

CREATE TABLE surplus_funding_addresses (
    asset       TEXT NOT NULL REFERENCES assets (asset),
    network     TEXT NOT NULL REFERENCES networks (network),
    address     TEXT NOT NULL CHECK (address <> ''),
    -- The operator's reference for the declaration (REQ-BK-5: each with a reference).
    reference   TEXT NOT NULL CHECK (reference <> ''),
    declared_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (asset, network, address)
);

COMMENT ON TABLE surplus_funding_addresses IS
    'REQ-BK-5: the addresses the operator funds a reserve from. A final arrival at the reserve from '
    'one is a surplus contribution, from chain data; any other arrival is owed mint (§5.3). '
    'Published in every statement so anyone can recompute owed mint (REQ-BK-6).';

CREATE TRIGGER surplus_funding_addresses_append_only
    BEFORE UPDATE OR DELETE ON surplus_funding_addresses
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-BK-5');
CREATE TRIGGER surplus_funding_addresses_no_truncate
    BEFORE TRUNCATE ON surplus_funding_addresses
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-BK-5');

-- ---------------------------------------------------------------------------------------------
-- Grants. No function here is PUBLIC's; the trigger function fires without an EXECUTE grant.

REVOKE EXECUTE ON FUNCTION pauses_guard_insert() FROM PUBLIC;

GRANT SELECT, INSERT ON book_quarantine TO bezent_core;
GRANT SELECT ON book_quarantine TO bezent_service, bezent_operator;
GRANT SELECT ON surplus_funding_addresses TO bezent_service, bezent_operator;
GRANT INSERT (quarantine_id) ON pauses TO bezent_core;

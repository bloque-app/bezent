-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #26 (review of PR #72): the core's late-mint check (`AC-I1.2`, `REQ-IC-4`, `INV-8`) read
-- every final `Minted` and `RefundMinted` each cycle, and re-evaluated those a later attempt
-- explains for ever, scanning observations and the event log by expression with no index.
-- Additive: one work table, its trigger, its grants, and the indexes the check still needs.
--
--   * `late_mint_checks`: one row per final `Minted` or `RefundMinted` observation of an issuing
--     network, queued in the same transaction that records the observation's `final` outcome (a
--     trigger, so an observation that is finalised late, or committed after a later one, is queued
--     when it is finalised: no id or time ordering is relied on). The core reads the open rows
--     only (a partial index), and settles a row once its verdict cannot change: raised, explained
--     by an attempt executed in the mint's block, or not a late mint at all. A row it cannot
--     settle yet (an attempt still undecided inside its era) stays open and is looked at again.
--     The core may update the check's own columns only: it cannot queue, unqueue or edit what an
--     observation says.
--   * Indexes for what each look still does: the inclusion observations of a signed transaction,
--     and the delivery anomalies that name one.
--
-- Rows that exist when this runs are queued once: the first walk of the core over them raises
-- nothing for a history that was delivered by bezent.

CREATE TABLE late_mint_checks (
    observation_id BIGINT PRIMARY KEY REFERENCES observations (id),
    queued_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- The last time the core looked at it (open rows are looked at least recently first).
    checked_at     TIMESTAMPTZ,
    -- How many times in a row looking failed (a persistent failure pages a human).
    failures       INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    settled_at     TIMESTAMPTZ,
    verdict        TEXT CHECK (verdict IN ('alerted', 'explained', 'not-late')),
    CONSTRAINT late_mint_checks_settled_with_verdict
        CHECK ((settled_at IS NULL) = (verdict IS NULL))
);

COMMENT ON TABLE late_mint_checks IS
    'Issue #26, AC-I1.2, REQ-IC-4: the final Minted and RefundMinted observations of an issuing '
    'network the core has still to settle. Queued by the outcome''s own transaction; the core '
    'updates only its verdict columns.';

CREATE INDEX late_mint_checks_open ON late_mint_checks (checked_at NULLS FIRST, observation_id)
    WHERE settled_at IS NULL;

CREATE FUNCTION late_mint_enqueue() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    INSERT INTO late_mint_checks (observation_id)
    SELECT o.id
    FROM observations o JOIN networks n ON n.network = o.network AND n.role = 'issuing'
    WHERE o.id = NEW.observation_id AND o.executed AND o.on_chain_reference IS NOT NULL
      AND o.detail ->> 'event' IN ('Minted', 'RefundMinted')
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END
$$;

REVOKE EXECUTE ON FUNCTION late_mint_enqueue() FROM PUBLIC;

CREATE TRIGGER observation_outcomes_late_mint_enqueue
    AFTER INSERT ON observation_outcomes
    FOR EACH ROW WHEN (NEW.outcome = 'final')
    EXECUTE FUNCTION late_mint_enqueue();

INSERT INTO late_mint_checks (observation_id)
SELECT o.id
FROM observations o
JOIN observation_outcomes x ON x.observation_id = o.id AND x.outcome = 'final'
JOIN networks n ON n.network = o.network AND n.role = 'issuing'
WHERE o.executed AND o.on_chain_reference IS NOT NULL
  AND o.detail ->> 'event' IN ('Minted', 'RefundMinted')
ON CONFLICT DO NOTHING;

CREATE TRIGGER late_mint_checks_no_delete
    BEFORE DELETE ON late_mint_checks
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('AC-I1.2');
CREATE TRIGGER late_mint_checks_no_truncate
    BEFORE TRUNCATE ON late_mint_checks
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('AC-I1.2');

-- The look at one signed transaction: its inclusion observations, and the anomalies that name it.
CREATE INDEX observations_by_signed_transaction ON observations ((detail ->> 'signed_transaction'))
    WHERE position_kind = 'transaction';
CREATE INDEX event_log_anomaly_by_signed_transaction ON event_log ((payload ->> 'signed_transaction'))
    WHERE kind = 'delivery-anomaly';

GRANT SELECT ON late_mint_checks TO bezent_core;
GRANT UPDATE (checked_at, failures, settled_at, verdict) ON late_mint_checks TO bezent_core;

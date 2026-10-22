-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #25 (F-003 follow-up, review note F3 of the verification): a withdrawal whose next attempt
-- was ordered but never signed could never be refunded, not even by an operator. Additive: one
-- table, three guards, and `attempts_unproven` replaced by itself.
--
-- The deadlock. After attempt n is proven not executed at finality (REQ-RF-5, REQ-NX-1), the core
-- orders attempt n+1. If its preparation never passes (a dry run that keeps failing) nothing is
-- ever signed for it, so there is no signed transaction to prove not
-- executed, and `attempts_unproven()` (0015) lists the attempt for good: it has an intent and no
-- proof. The refund edge (0015, 0048) needs every attempt with an intent proven, so the refund is
-- closed to everyone, the approved operator's resolution included (INV-8, REQ-RF-1).
--
-- The way out is a record, not a relaxation. An order that nothing was ever signed for can be
-- cancelled, once, and the store then refuses to sign it, ever:
--
--   * `unsigned_attempt_cancellations`: append-only, one row per (transfer, attempt), made by the
--     core under a recorded review resolution into `Refunding` that has its second approver
--     (REQ-GV-5, REQ-RV-2): the resolution that moves the value is what authorises it, and it
--     names the row in its own transaction.
--   * The insert is refused unless, under the transfer's row lock (the one the signer's inserts
--     share, 0015, so the two are serial): the transfer is in `Needs-Review`; the resolution is an
--     approved, unrun resolution of this transfer into `Refunding`; the attempt is the transfer's
--     next one (the transfers' own attempts end at the last attempt that reached `Delivering`),
--     ordered, and its predecessor is proven not executed at finality; and nothing was signed for
--     it: no signature and no signed transaction on any intent of the attempt. That is the evidence.
--   * From then on no intent, signature or signed transaction is recorded for that attempt, whoever
--     writes it. Without this a signature could still land after the refund decision's lock was
--     released, and the redeemer would be paid twice (INV-8).
--   * `attempts_unproven()` does not list a cancelled attempt, so the refund edge opens exactly
--     when every attempt that could have moved value is proven, and the rest were never signed.
--
-- Nothing is refunded on a timer or on an unknown (REQ-RF-1): the refund still rests on the final
-- proof of the last attempt that was signed, and on an operator's approved resolution.
--
-- Out of scope: an attempt whose preparation never happens at all (the endpoint is down, so not even
-- a refused preparation is written) waits for the endpoint and has no deadline edge, so nothing
-- brings it to review; issue #48 follows it up.

CREATE TABLE unsigned_attempt_cancellations (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id      BIGINT NOT NULL REFERENCES transfers (id),
    attempt          INTEGER NOT NULL CHECK (attempt >= 2),
    -- The review resolution into `Refunding` that authorises it (checked by the insert guard).
    manual_action_id BIGINT NOT NULL,
    recorded_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT unsigned_attempt_cancellations_once UNIQUE (transfer_id, attempt)
);

COMMENT ON TABLE unsigned_attempt_cancellations IS
    'Issue #25, INV-8, REQ-RF-1: an attempt that was ordered and never signed, cancelled under an '
    'approved review resolution into Refunding. Nothing is recorded for it afterwards; it is not '
    'listed by attempts_unproven(). Append-only; written by the core.';

CREATE TRIGGER unsigned_attempt_cancellations_append_only
    BEFORE UPDATE OR DELETE ON unsigned_attempt_cancellations
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('INV-8, REQ-RF-1');
CREATE TRIGGER unsigned_attempt_cancellations_no_truncate
    BEFORE TRUNCATE ON unsigned_attempt_cancellations
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('INV-8, REQ-RF-1');

CREATE FUNCTION unsigned_attempt_cancellations_guard() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    t       transfers%ROWTYPE;
    act     manual_actions%ROWTYPE;
    reached INTEGER;
BEGIN
    -- The refund decision's lock: the signer's inserts take it FOR SHARE (0015), so a signature
    -- either lands before this row (and is seen below) or waits for it (and is then refused).
    SELECT * INTO t FROM transfers WHERE id = NEW.transfer_id FOR UPDATE;
    IF NOT FOUND OR t.state <> 'Needs-Review' THEN
        RAISE EXCEPTION 'attempt % of transfer % is cancelled only in review, under an approved resolution into Refunding (INV-8, REQ-RF-1, REQ-RV-2)',
            NEW.attempt, NEW.transfer_id
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT * INTO act FROM manual_actions WHERE id = NEW.manual_action_id;
    IF NOT FOUND OR act.kind <> 'review-resolution' OR act.transfer_id <> t.id
       OR act.to_state IS DISTINCT FROM 'Refunding'
       OR NOT EXISTS (SELECT 1 FROM manual_approvals WHERE action_id = act.id)
       OR EXISTS (SELECT 1 FROM transitions x WHERE x.manual_action_id = act.id) THEN
        RAISE EXCEPTION 'attempt % of transfer % is cancelled only by an approved, unrun review resolution of this transfer into Refunding (REQ-GV-5, REQ-RV-2, INV-8)',
            NEW.attempt, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    SELECT COALESCE(max(attempt), 0) INTO reached FROM transitions WHERE transfer_id = t.id;
    IF NEW.attempt <> reached + 1
       OR NOT EXISTS (SELECT 1 FROM intents WHERE transfer_id = t.id AND attempt = NEW.attempt)
       OR (SELECT p.network FROM attempt_proof(t.id, reached) p) IS NULL THEN
        RAISE EXCEPTION 'attempt % of transfer % is not the next attempt after one proven not executed at finality, ordered and never entered (INV-8, REQ-EO-6)',
            NEW.attempt, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM signatures s JOIN intents i ON i.id = s.intent_id
               WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt)
       OR EXISTS (SELECT 1 FROM signed_transactions st JOIN intents i ON i.id = st.intent_id
                  WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt) THEN
        RAISE EXCEPTION 'attempt % of transfer % was signed, or has signed bytes recorded; it is proven not executed, never cancelled (INV-8, REQ-EO-6, REQ-NX-1)',
            NEW.attempt, t.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER unsigned_attempt_cancellations_guard
    BEFORE INSERT ON unsigned_attempt_cancellations
    FOR EACH ROW EXECUTE FUNCTION unsigned_attempt_cancellations_guard();

-- Nothing is recorded for a cancelled attempt: no order, no signing intent, no signature, no
-- signed bytes. Whoever writes it, the owner included (a restore replays rows in order, so the
-- cancellation follows what it cancels). SECURITY DEFINER: the signer cannot read the table.
CREATE FUNCTION refuse_cancelled_attempt() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = pg_catalog, public, pg_temp
    AS $$
DECLARE
    owner   BIGINT;
    n       INTEGER;
BEGIN
    IF TG_TABLE_NAME = 'intents' THEN
        owner := NEW.transfer_id;
        n := NEW.attempt;
    ELSE
        SELECT i.transfer_id, i.attempt INTO owner, n FROM intents i WHERE i.id = NEW.intent_id;
    END IF;
    IF owner IS NOT NULL AND EXISTS (SELECT 1 FROM unsigned_attempt_cancellations c
                                     WHERE c.transfer_id = owner AND c.attempt = n) THEN
        RAISE EXCEPTION '% for attempt % of transfer % refused: the attempt was cancelled unsigned and nothing is recorded for it (INV-8, REQ-RF-1)',
            TG_TABLE_NAME, n, owner
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intents_refuse_cancelled_attempt
    BEFORE INSERT ON intents
    FOR EACH ROW EXECUTE FUNCTION refuse_cancelled_attempt();
CREATE TRIGGER signatures_refuse_cancelled_attempt
    BEFORE INSERT ON signatures
    FOR EACH ROW EXECUTE FUNCTION refuse_cancelled_attempt();
CREATE TRIGGER signed_transactions_refuse_cancelled_attempt
    BEFORE INSERT ON signed_transactions
    FOR EACH ROW EXECUTE FUNCTION refuse_cancelled_attempt();

-- 0015's function, with the cancelled attempts left out: the attempts of a transfer that have an
-- intent and neither a recorded proof of non-execution nor a recorded cancellation.
CREATE OR REPLACE FUNCTION attempts_unproven(p_transfer BIGINT) RETURNS INTEGER[]
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
SELECT COALESCE(array_agg(k.attempt ORDER BY k.attempt), '{}')
FROM (SELECT DISTINCT attempt FROM intents WHERE transfer_id = p_transfer) k
WHERE (SELECT p.network FROM attempt_proof(p_transfer, k.attempt) p) IS NULL
  AND NOT EXISTS (SELECT 1 FROM unsigned_attempt_cancellations c
                  WHERE c.transfer_id = p_transfer AND c.attempt = k.attempt)
$$;

REVOKE EXECUTE ON FUNCTION unsigned_attempt_cancellations_guard(), refuse_cancelled_attempt()
    FROM PUBLIC;

-- The core records it and reads it (the table is its own); no other role holds anything on it.
GRANT SELECT, INSERT ON unsigned_attempt_cancellations TO bezent_core;

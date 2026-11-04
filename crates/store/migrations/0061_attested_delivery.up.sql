-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-011: an attested backend's delivery. WORKING ASSUMPTION, amendment 0016 A1, proposed and NOT
-- ruled: on an attested backend `Delivering` also means the relayed instructions were accepted
-- (§5.4), and a party's report of non-execution is the proof (REQ-NX-1, REQ-BE-5). Kept in its own
-- migration so that a contrary ruling removes this file and nothing else.
--
--   * A new kind of report: `payout-not-executed`, naming the transfer and its attempt.
--   * bezent_attested_hand_over_open(): the one rule a delivery attempt on an attested backend passes
--     in place of "signed and recorded": a withdrawal whose instructions were accepted, or the refund
--     of an unassociated arrival, which names its destination in its request.
--   * attempt_proof() also reads a recorded report of non-execution, so `Refunding` and attempt n+1
--     rest on it as on any proof, at its own final point, unless a payout was reported for the
--     transfer: then it proves nothing (INV-8).
--   * attempts_unproven() also counts the attempts a transfer entered, which on an attested backend
--     have no intent: no refund of one without its proof, whoever writes it (INV-8).
--
-- The two long functions are changed where they say, by replacing their text: what changes is what
-- this file shows, and a text that is not there stops the migration.

ALTER TABLE attested_reports DROP CONSTRAINT attested_reports_kind_check;
ALTER TABLE attested_reports ADD CONSTRAINT attested_reports_kind_check
    CHECK (kind IN ('arrival', 'payout', 'payout-not-executed', 'positions', 'reversal',
                    'restoration'));

-- ---------------------------------------------------------------------------------------------
-- A report is the observation: the refund of an unassociated arrival is paid by report too, and a
-- report of non-execution is recorded as what did not execute.

DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('bezent_record_attested_observation(bigint)'::regprocedure);
    edits TEXT[][] := ARRAY[
        ARRAY[$old$              AND x.kind = 'withdrawal' AND x.destination_network = r.network$old$,
              $new$              AND (x.kind = 'withdrawal' OR (x.kind = 'refund' AND x.refund_of IS NULL))
              AND x.destination_network = r.network$new$],
        ARRAY[$old$                RAISE EXCEPTION 'report % of %: its payout is of no withdrawal of % there (REQ-OT-2)',$old$,
              $new$                RAISE EXCEPTION 'report % of %: its payout is of no transfer of % there (REQ-OT-2)',$new$],
        ARRAY[$old$            o_ref := t.destination_reference;$old$,
              $new$            o_ref := COALESCE(t.destination_reference, t.on_chain_reference);
            IF o_ref IS NULL THEN
                RAISE EXCEPTION 'report % of %: its transfer has no reference to carry yet (REQ-OT-2)',
                    r.report_id, r.network
                    USING ERRCODE = 'BZ001';
            END IF;
        WHEN 'payout-not-executed' THEN
            o_kind := 'other';
            o_event := 'PayoutNotExecuted';
            o_amount := NULL;
            o_detail := o_detail || jsonb_build_object(
                'bridge_reference', r.body -> 'bridge_reference', 'attempt', r.body -> 'attempt');$new$],
        ARRAY[$old$o_sender, o_to, o_ref, TRUE, o_detail, o_kind)$old$,
              $new$o_sender, o_to, o_ref, r.kind <> 'payout-not-executed', o_detail, o_kind)$new$]
    ];
    i INT;
BEGIN
    FOR i IN 1 .. array_length(edits, 1) LOOP
        IF position(edits[i][1] IN src) = 0 THEN
            RAISE EXCEPTION 'bezent_record_attested_observation: not found: %', edits[i][1];
        END IF;
        src := replace(src, edits[i][1], edits[i][2]);
    END LOOP;
    EXECUTE src;
END
$migration$;

-- ---------------------------------------------------------------------------------------------
-- The hand-over.

CREATE FUNCTION bezent_attested_hand_over_open(p_transfer BIGINT) RETURNS BOOLEAN
    LANGUAGE sql
    STABLE
    SET search_path = pg_catalog, public, pg_temp
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM transfers t
        JOIN networks n ON n.network = t.destination_network AND n.backend LIKE 'attested:%'
        WHERE t.id = p_transfer
          AND ((t.kind = 'withdrawal'
                AND EXISTS (SELECT 1 FROM relayed_instructions r WHERE r.transfer_id = t.id))
               OR (t.kind = 'refund' AND t.refund_of IS NULL)))
$$;

COMMENT ON FUNCTION bezent_attested_hand_over_open(BIGINT) IS
    'Amendment 0016 A1 (proposed): whether a delivery attempt of this transfer may enter Delivering on an '
    'attested backend, where bezent signs nothing (REQ-RQ-9): its instructions were accepted, or it is the '
    'refund of an unassociated arrival. False on any public backend.';

DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('transitions_apply_lifecycle()'::regprocedure);
    old TEXT := $old$WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt) THEN$old$;
BEGIN
    IF position(old IN src) = 0 THEN
        RAISE EXCEPTION 'transitions_apply_lifecycle: not found: %', old;
    END IF;
    EXECUTE replace(src, old, $new$WHERE i.transfer_id = t.id AND i.attempt = NEW.attempt)
           AND NOT bezent_attested_hand_over_open(t.id) THEN$new$);
END
$migration$;

-- A refund replaces a leg proven not executed (S7 of 0048): "at least one payout attempt was ordered"
-- reads, on an attested backend, "at least one attempt was handed over".
DO $migration$
DECLARE
    src TEXT := pg_get_functiondef('transfers_guard_writer()'::regprocedure);
    old TEXT := $old$WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order') THEN$old$;
BEGIN
    IF position(old IN src) = 0 THEN
        RAISE EXCEPTION 'transfers_guard_writer: not found: %', old;
    END IF;
    EXECUTE replace(src, old, $new$WHERE i.transfer_id = NEW.refund_of AND i.payload ? 'order')
                   AND NOT (bezent_attested_hand_over_open(NEW.refund_of)
                            AND EXISTS (SELECT 1 FROM transitions x
                                        WHERE x.transfer_id = NEW.refund_of
                                          AND x.attempt IS NOT NULL)) THEN$new$);
END
$migration$;

-- ---------------------------------------------------------------------------------------------
-- The proof. As 0021's, with a recorded report of non-execution of this attempt beside it: recorded,
-- so the core checked its signature; a row alone proves nothing. Only an attested backend has one.

CREATE OR REPLACE FUNCTION attempt_proof(
    p_transfer BIGINT, p_attempt INTEGER,
    OUT network TEXT, OUT final_block_number BIGINT, OUT final_block_hash BYTEA
)
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    (SELECT n.network, n.final_block_number, n.final_block_hash
     FROM non_execution_proofs n
     JOIN signed_transactions st ON st.id = n.signed_transaction_id
     JOIN intents i ON i.id = st.intent_id
     WHERE i.transfer_id = p_transfer AND i.attempt = p_attempt
       AND NOT EXISTS (SELECT 1 FROM signed_transactions st2
                       JOIN intents i2 ON i2.id = st2.intent_id
                       WHERE i2.transfer_id = p_transfer AND i2.attempt = p_attempt
                         AND NOT EXISTS (SELECT 1 FROM non_execution_proofs n2
                                         WHERE n2.signed_transaction_id = st2.id))
       AND NOT EXISTS (SELECT 1 FROM signatures s3
                       JOIN intents i3 ON i3.id = s3.intent_id
                       WHERE i3.transfer_id = p_transfer AND i3.attempt = p_attempt
                         AND NOT EXISTS (SELECT 1 FROM signed_transactions st3
                                         WHERE st3.intent_id = s3.intent_id))
     ORDER BY n.final_block_number DESC, n.id DESC
     LIMIT 1)
    UNION ALL
    (SELECT r.network, r.seq, r.digest
     FROM attested_reports r
     JOIN transfers t ON t.bridge_reference = r.body ->> 'bridge_reference'
                     AND t.destination_network = r.network
     JOIN observations o ON o.network = r.network AND o.block_number = r.seq
                        AND o.block_hash = r.digest
     WHERE t.id = p_transfer AND r.kind = 'payout-not-executed'
       AND r.body ->> 'attempt' = p_attempt::text
       -- A payout reported for the transfer, recorded or not, voids the proof (INV-8): the two
       -- reports can be taken in side by side, and a refund mint beside a payout is the loss.
       -- The ingress refuses the second one; this is the store's own rule, whoever writes.
       AND NOT EXISTS (SELECT 1 FROM attested_reports p
                       WHERE p.network = r.network AND p.kind = 'payout'
                         AND p.body ->> 'bridge_reference' = r.body ->> 'bridge_reference')
     ORDER BY r.seq
     LIMIT 1)
    LIMIT 1
$$;

-- As 0051's, with the attempts the transfer entered: on a public backend each has its intents.
CREATE OR REPLACE FUNCTION attempts_unproven(p_transfer BIGINT) RETURNS INTEGER[]
    LANGUAGE sql
    STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
SELECT COALESCE(array_agg(k.attempt ORDER BY k.attempt), '{}')
FROM (SELECT attempt FROM intents WHERE transfer_id = p_transfer
      UNION
      SELECT attempt FROM transitions WHERE transfer_id = p_transfer AND attempt IS NOT NULL) k
WHERE (SELECT p.network FROM attempt_proof(p_transfer, k.attempt) p) IS NULL
  AND NOT EXISTS (SELECT 1 FROM unsigned_attempt_cancellations c
                  WHERE c.transfer_id = p_transfer AND c.attempt = k.attempt)
$$;

REVOKE EXECUTE ON FUNCTION bezent_attested_hand_over_open(BIGINT) FROM PUBLIC;

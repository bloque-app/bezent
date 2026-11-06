-- SPDX-License-Identifier: GPL-3.0-or-later
-- Amendment 0026 (`d-requester-arrivals`, ruled by @pandres95 on the decision form, 2026-11-06: "Bind by
-- sender"): each requester declares its senders, and a deposit binds only an arrival whose sender is one of
-- the requesting requester's declared senders (`REQ-PN-3`, `REQ-IN-3`, `REQ-RQ-3`, `AC-A2.5`). Until now
-- any requester holding the deposit grant bound any final, unconsumed arrival at a reserve, and the rightful
-- request was then refused `ERR-SourceConsumed` (amendment 0018, "The exposure the binding rules do not
-- close").
--
--   * requester_senders: the migration owner's declaration, like the other requester declarations (0010):
--     one row per (requester, network, address). No service login writes it; the ingress reads it.
--   * sender_declared(): whether a sender is one a requester declared on a network. Compared exactly, or
--     case-insensitively on a network of address format `evm` (as funding_arrival(), 0032, compares). A
--     movement with no sender matches nothing.
--   * intake_answers_guard_sender(): the binding guard, whoever writes, the owner included. A transfer
--     carries no requester; what ties a bound deposit to the requester that made it is its request's
--     answer (0010). So an answer that binds a deposit (no error, not a conversion's answer) is refused
--     unless the observation at the deposit's source was sent from a sender the request's requester
--     declared on that network. The ingress checks the same first and refuses with
--     `ERR-SenderNotDeclared`; this is the store's backstop (BZ001).
--     Which requester made a request is the ingress's to authenticate (`REQ-PN-1`): the store records the
--     requester the ingress names (`intake.requester`). So this guard keeps the ingress's own rule from
--     being skipped by a fault in it, and binds every writer of an answer, the owner too; it is not a
--     defence against a login that holds the ingress's role, which could name any requester. A transfer
--     written with no answer (the owner's drills, NFR-7, and the store tests) is not checked.
--   * refusal_error gains `ERR-SenderNotDeclared` (SPEC §10).
--
-- A1b (amendment 0018): a deposit that names a convertible arrival is answered when it binds the arrival,
-- so the arrival's sender is what is checked. The conversion's output is sent from the venue's pool; it is
-- bound only through deposit_conversion_record() (0120), by the deposit's own requester, and its answer is
-- the conversion's (`conversion`), which this guard does not check again. A deposit that names a conversion
-- output directly (amendment 0018, Open 2) is checked like any other: its sender is the pool, so it binds
-- only for a requester that declared the pool.
--
-- The refund of an unassociated arrival (`REQ-RQ-6`) is not a deposit and is unchanged: it needs a second
-- approver at the operator.

-- ---------------------------------------------------------------------------------------------
-- Named errors (§10).

ALTER DOMAIN refusal_error DROP CONSTRAINT refusal_error_check;
ALTER DOMAIN refusal_error ADD CONSTRAINT refusal_error_check
    CHECK (VALUE IN (
        'ERR-NotAuthorised', 'ERR-ReferenceConflict', 'ERR-UnknownRoute', 'ERR-RoutePaused',
        'ERR-BelowMinimum', 'ERR-AboveMaximum', 'ERR-WindowExceeded', 'ERR-ExcessPrecision',
        'ERR-SourceNotFound', 'ERR-SourceMismatch', 'ERR-SourceConsumed',
        'ERR-InvalidDestination', 'ERR-Overloaded', 'ERR-Malformed', 'ERR-CommitmentMismatch',
        'ERR-SenderNotDeclared'
    ));

-- ---------------------------------------------------------------------------------------------
-- The declaration.

CREATE TABLE requester_senders (
    requester TEXT NOT NULL REFERENCES requesters (requester),
    network   TEXT NOT NULL REFERENCES networks (network),
    -- As the network's endpoint spells a movement's sender (`observations.sender`); on a network of
    -- address format `evm` it is compared case-insensitively.
    address   TEXT NOT NULL CHECK (address <> ''),
    PRIMARY KEY (requester, network, address)
);

COMMENT ON TABLE requester_senders IS
    'REQ-PN-3, REQ-IN-3 (amendment 0026): the senders each requester declares, per origin network. A '
    'deposit binds only an arrival sent from one of its requester''s declared senders. The migration '
    'owner''s declaration; no service login writes it.';

CREATE FUNCTION sender_declared(p_requester TEXT, p_network TEXT, p_sender TEXT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT p_requester IS NOT NULL AND p_sender IS NOT NULL AND EXISTS (
        SELECT 1 FROM requester_senders s JOIN networks n ON n.network = s.network
        WHERE s.requester = p_requester AND s.network = p_network
          AND (s.address = p_sender
               OR (n.address_format = 'evm' AND lower(s.address) = lower(p_sender))))
$$;

COMMENT ON FUNCTION sender_declared(TEXT, TEXT, TEXT) IS
    'REQ-PN-3, REQ-IN-3 (amendment 0026): whether p_sender is a sender p_requester declared on '
    'p_network. A NULL sender is never declared.';

-- ---------------------------------------------------------------------------------------------
-- The binding guard, on the answer that ties a bound deposit to its requester.

CREATE FUNCTION intake_answers_guard_sender() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    t      RECORD;
    asker  TEXT;
    sender TEXT;
BEGIN
    IF NEW.transfer_id IS NULL OR NEW.conversion OR NEW.error IS NOT NULL THEN
        RETURN NEW;
    END IF;
    SELECT tr.kind, tr.refund_of, tr.bridge_reference, tr.source_network, tr.source_block_number,
           tr.source_block_hash, tr.source_position_kind, tr.source_position_index
    INTO t
    FROM transfers tr WHERE tr.id = NEW.transfer_id;
    IF NOT FOUND OR t.kind <> 'deposit' OR t.refund_of IS NOT NULL OR t.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT i.requester INTO asker FROM intake i WHERE i.id = NEW.intake_id;
    SELECT o.sender INTO sender
    FROM observations o
    WHERE o.network = t.source_network AND o.block_number = t.source_block_number
      AND o.block_hash = t.source_block_hash AND o.position_kind = t.source_position_kind
      AND o.position_index = t.source_position_index;
    IF NOT sender_declared(asker, t.source_network, sender) THEN
        RAISE EXCEPTION 'answer refused: deposit %''s source was not sent from a sender requester % declared on % (REQ-PN-3, REQ-IN-3, ERR-SenderNotDeclared)',
            t.bridge_reference, coalesce(asker, 'unknown'), t.source_network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER intake_answers_guard_sender
    BEFORE INSERT ON intake_answers
    FOR EACH ROW EXECUTE FUNCTION intake_answers_guard_sender();

-- ---------------------------------------------------------------------------------------------
-- Grants. Nothing here is PUBLIC's. The ingress reads the declaration and calls the check before it binds
-- a deposit; nobody writes the declaration but the owner.

REVOKE ALL ON requester_senders FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION
    sender_declared(TEXT, TEXT, TEXT),
    intake_answers_guard_sender()
FROM PUBLIC;

GRANT SELECT ON requester_senders TO bezent_ingress;
GRANT EXECUTE ON FUNCTION sender_declared(TEXT, TEXT, TEXT) TO bezent_ingress;

-- SPDX-License-Identifier: AGPL-3.0-or-later
-- Issue #30 (F-002 follow-up), amendments 0010 and 0011, with the review of PR #44. Additive.
--
--   * refusal_error gains `ERR-Malformed` and `ERR-CommitmentMismatch` (SPEC §10), which the
--     ingress now records and answers in place of its own codes.
--   * observations.kind: what an observation is, written by the endpoint that saw it, from a
--     closed set (review M1, M2, S1). The endpoint is the only party that sees the whole
--     transaction (a payout's reference, a conversion's pool legs, a self transfer), so it says the
--     kind once and the store's rules read it. Rows recorded before this migration are backfilled
--     from `detail.event`; a row whose kind cannot be told is `other`, which nothing binds.
--   * Who writes what kind: each endpoint's login records only its own network's observations and
--     outcomes (`endpoint_scopes`), and only the kinds that network's role produces.
--   * source_bindable(): the kinds a deposit and the refund of an unassociated arrival may name
--     (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2). Fail closed: an
--     observation without an explicit bindable kind binds nothing. The store enforces it on the
--     binding, by insert or by update, and again when the source becomes final (review S2).

-- ---------------------------------------------------------------------------------------------
-- Named errors (§10).

ALTER DOMAIN refusal_error DROP CONSTRAINT refusal_error_check;
ALTER DOMAIN refusal_error ADD CONSTRAINT refusal_error_check
    CHECK (VALUE IN (
        'ERR-NotAuthorised', 'ERR-ReferenceConflict', 'ERR-UnknownRoute', 'ERR-RoutePaused',
        'ERR-BelowMinimum', 'ERR-AboveMaximum', 'ERR-WindowExceeded', 'ERR-ExcessPrecision',
        'ERR-SourceNotFound', 'ERR-SourceMismatch', 'ERR-SourceConsumed',
        'ERR-InvalidDestination', 'ERR-Overloaded', 'ERR-Malformed', 'ERR-CommitmentMismatch'
    ));

-- ---------------------------------------------------------------------------------------------
-- The kind of an observation.
--
-- On an origin network (at a reserve address):
--   arrival              an admitted origin token arriving at the reserve; a deposit may bind it
--   other_arrival        a token that is not admitted arriving at the reserve (REQ-IN-6)
--   convertible_arrival  an admitted convertible arriving at the reserve; never minted against
--                        (REQ-CV-1), held until converted
--   conversion_output    the origin token arriving at the reserve from a declared pool, in the
--                        transaction where the reserve paid that pool a convertible (REQ-CV-2)
--   conversion_input     the reserve's convertible paid to the pool in such a transaction
--   payout_leg           the reserve's payment carrying a redemption's or a refund's reference
--   outflow              any other movement out of the reserve
--   self_transfer        the reserve to itself: nothing arrived
-- On the issuing network (the on-chain component's events):
--   mint, redemption, refund_mint, control (a pause, an unpause, a bounds change)
-- On either:
--   transaction          a signed transaction of bezent's own, at its position (REQ-EO-1)
--   other                anything else, or a kind that cannot be told; nothing binds it

CREATE DOMAIN observation_kind AS TEXT
    CHECK (VALUE IN (
        'arrival', 'other_arrival', 'convertible_arrival', 'conversion_output',
        'conversion_input', 'payout_leg', 'outflow', 'self_transfer',
        'mint', 'redemption', 'refund_mint', 'control',
        'transaction', 'other'
    ));

-- Whether a network of role `p_role` ('origin', 'issuing', or NULL for one not declared)
-- produces observations of kind `p_kind`.
CREATE FUNCTION observation_kind_on_network(p_role TEXT, p_kind TEXT) RETURNS BOOLEAN
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE p_role
        WHEN 'origin' THEN p_kind IN (
            'arrival', 'other_arrival', 'convertible_arrival', 'conversion_output',
            'conversion_input', 'payout_leg', 'outflow', 'self_transfer', 'transaction', 'other')
        WHEN 'issuing' THEN p_kind IN (
            'mint', 'redemption', 'refund_mint', 'control', 'transaction', 'other')
        ELSE p_kind = 'other'
    END
$$;

-- The kind an endpoint's `detail.event` names, for the backfill (and the tests' fixtures). An
-- event this does not know is `other`.
CREATE FUNCTION observation_kind_for_event(p_event TEXT) RETURNS TEXT
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE p_event
        WHEN 'Arrival' THEN 'arrival'
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

-- New rows say their kind; one that does not is `other`, which nothing binds (review M1).
ALTER TABLE observations ADD COLUMN kind observation_kind NOT NULL DEFAULT 'other';

COMMENT ON COLUMN observations.kind IS
    'REQ-OB-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2: what the observation is, as the '
    'endpoint that recorded it classified it. Only arrival and conversion_output bind a deposit; '
    'other binds nothing.';

-- Backfill (observations are append-only, so the migration owner lifts the rule for this one
-- statement). Only the issuing network's rows take a kind from their `detail`: a row of bezent's
-- own signed transaction is `transaction`; any other takes the kind its event names, when the
-- issuing network produces that kind. Every row of an origin network, and any row whose kind
-- cannot be told, is `other`: before this migration any login holding `bezent_endpoint` could
-- write an origin row, and none of its `detail` is verifiable, so none of it may become
-- bindable (review S2). An origin network's endpoint writes `kind` itself.
ALTER TABLE observations DISABLE TRIGGER observations_append_only;
UPDATE observations o
SET kind = k.kind
FROM (
    SELECT o2.id,
           CASE WHEN n.role IS DISTINCT FROM 'issuing' THEN 'other'
                WHEN o2.position_kind = 'transaction' AND o2.detail ? 'signed_transaction'
                     THEN 'transaction'
                WHEN observation_kind_on_network(n.role,
                                                 observation_kind_for_event(o2.detail ->> 'event'))
                     THEN observation_kind_for_event(o2.detail ->> 'event')
                ELSE 'other'
           END AS kind
    FROM observations o2
    LEFT JOIN networks n ON n.network = o2.network
) k
WHERE k.id = o.id AND k.kind <> 'other';
ALTER TABLE observations ENABLE TRIGGER observations_append_only;

-- ---------------------------------------------------------------------------------------------
-- Each endpoint's login records only its own network's observations and outcomes.
--
-- One endpoint role served every network (F-007 NOTES 27): any login holding `bezent_endpoint`
-- could record an arrival on an origin network as readily as a mint on the issuing network. Now
-- an endpoint's login holds a role of its own, registered here with the networks it records; a
-- login that holds `bezent_endpoint` and no registered role records no observation at all. The
-- migration owner and a superuser keep writing everything, for drills and restores (NFR-7).
--
-- The Kreivo endpoint's role is `bezent_kreivo_endpoint`: the issuing network. Another endpoint
-- (the EVM endpoint, PR #39) registers its own role in its own migration, numbered after this one.

CREATE TABLE endpoint_scopes (
    endpoint_role  TEXT PRIMARY KEY CHECK (endpoint_role <> ''),
    -- The role of the networks it records.
    network_role   TEXT NOT NULL CHECK (network_role IN ('issuing', 'origin')),
    -- And their address format, when it records only some of them.
    address_format TEXT CHECK (address_format IN ('evm', 'ss58', 'opaque'))
);

COMMENT ON TABLE endpoint_scopes IS
    'REQ-OB-6, INV-2, INV-8: the endpoint roles, and the networks each one''s login records '
    'observations and outcomes of. Written by migrations only.';

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_kreivo_endpoint') THEN
        BEGIN
            CREATE ROLE bezent_kreivo_endpoint NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;

    IF current_user = 'bezent_kreivo_endpoint'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_kreivo_endpoint' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_kreivo_endpoint (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_kreivo_endpoint' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_kreivo_endpoint must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    -- The signer, the core and the services' role never hold it (REQ-AR-3).
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname = 'bezent_kreivo_endpoint'
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_signer', 'bezent_endpoint')) THEN
        RAISE EXCEPTION 'bezent_service, bezent_core, bezent_signer and bezent_endpoint must not be members of bezent_kreivo_endpoint'
            USING ERRCODE = 'BZ001';
    END IF;

    -- It inherits bezent_endpoint's grants and cannot SET ROLE into it (as PR #39 does for its
    -- role): SET ROLE bezent_endpoint would leave every registered scope behind, and the guard
    -- below refuses such a session anyway. Before PostgreSQL 16 every member may SET ROLE.
    IF current_setting('server_version_num')::int >= 160000 THEN
        BEGIN
            EXECUTE 'GRANT bezent_endpoint TO bezent_kreivo_endpoint WITH INHERIT TRUE, SET FALSE';
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- granted concurrently by another database's migration on the same server
        END;
        IF EXISTS (SELECT 1 FROM pg_auth_members m
                   JOIN pg_roles g ON g.oid = m.roleid
                   JOIN pg_roles u ON u.oid = m.member
                   WHERE g.rolname = 'bezent_endpoint' AND u.rolname = 'bezent_kreivo_endpoint'
                     AND (m.set_option OR NOT m.inherit_option)) THEN
            RAISE EXCEPTION 'bezent_kreivo_endpoint must hold bezent_endpoint WITH INHERIT TRUE, SET FALSE only (INV-8)'
                USING ERRCODE = 'BZ001';
        END IF;
    ELSIF NOT EXISTS (SELECT 1 FROM pg_auth_members m
                      JOIN pg_roles g ON g.oid = m.roleid
                      JOIN pg_roles u ON u.oid = m.member
                      WHERE g.rolname = 'bezent_endpoint' AND u.rolname = 'bezent_kreivo_endpoint') THEN
        BEGIN
            GRANT bezent_endpoint TO bezent_kreivo_endpoint;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;
        END;
    END IF;
END
$$;

GRANT USAGE ON SCHEMA public TO bezent_kreivo_endpoint;

INSERT INTO endpoint_scopes (endpoint_role, network_role, address_format)
    VALUES ('bezent_kreivo_endpoint', 'issuing', NULL);

GRANT SELECT ON endpoint_scopes TO bezent_service;

-- Whether the role the insert runs as may record observations of `p_network`: it is the table's
-- owner (or a superuser), or it holds a registered endpoint role whose scope covers the network.
CREATE FUNCTION observation_writer_covers(p_network TEXT, relation OID) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = relation), 'USAGE')
        OR EXISTS (
            SELECT 1 FROM endpoint_scopes s
            JOIN pg_roles r ON r.rolname = s.endpoint_role
            JOIN networks n ON n.network = p_network
            WHERE pg_has_role(current_user, r.oid, 'USAGE')
              AND n.role = s.network_role
              AND (s.address_format IS NULL OR s.address_format = n.address_format))
$$;

CREATE FUNCTION observations_guard_kind() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    network_role TEXT;
BEGIN
    SELECT role INTO network_role FROM networks WHERE network = NEW.network;
    IF NOT observation_kind_on_network(network_role, NEW.kind) THEN
        RAISE EXCEPTION 'observation on % refused: a % network records no observation of kind % (REQ-OB-6, INV-2)',
            NEW.network, coalesce(network_role, 'undeclared'), NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    IF jsonb_typeof(NEW.detail -> 'event') = 'string'
       AND NEW.kind <> observation_kind_for_event(NEW.detail ->> 'event') THEN
        RAISE EXCEPTION 'observation on % refused: kind % disagrees with its event % (REQ-OB-6, INV-2)',
            NEW.network, NEW.kind, NEW.detail ->> 'event'
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT observation_writer_covers(NEW.network, TG_RELID) THEN
        RAISE EXCEPTION 'observation on % refused: only that network''s endpoint records its observations (REQ-OB-6, INV-2, INV-8)',
            NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observations_guard_kind
    BEFORE INSERT ON observations
    FOR EACH ROW EXECUTE FUNCTION observations_guard_kind();

CREATE FUNCTION observation_outcomes_guard_scope() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    observed_on TEXT;
BEGIN
    SELECT network INTO observed_on FROM observations WHERE id = NEW.observation_id;
    IF NOT observation_writer_covers(observed_on, TG_RELID) THEN
        RAISE EXCEPTION 'outcome of observation % refused: only the endpoint of % records its outcomes (REQ-OB-6, INV-2, INV-8)',
            NEW.observation_id, coalesce(observed_on, 'no network')
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observation_outcomes_guard_scope
    BEFORE INSERT ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION observation_outcomes_guard_scope();

-- ---------------------------------------------------------------------------------------------
-- What a request may bind.
--
-- Both purposes: an executed movement of a token, with an amount and no on-chain reference (a
-- payout leg carries one), to an address that is a reserve of the origin network it was seen on.
-- Then:
--
--   deposit  kind `arrival` or `conversion_output` (REQ-RQ-3, REQ-IN-3, REQ-CV-2: the deposit is
--            minted against the conversion's output); an `arrival` not from a
--            reserve address (a rebalance) or a venue address of a route (a venue delivery). A
--            conversion output's sender is the venue by design, so those exclusions leave it alone
--            (review S1). Never a `convertible_arrival` (REQ-CV-1).
--   refund   any unassociated arrival (REQ-RQ-6, REQ-IN-6): kind `arrival` (whatever its amount
--            against the route's bounds, whoever sent it), `other_arrival` (a token not admitted)
--            or `conversion_output`. Never a `convertible_arrival` (REQ-CV-1, REQ-CV-6: it is
--            held until converted, its value released only through its conversion), a payout leg,
--            an outflow, a conversion's input or a self transfer. A surplus contribution from a declared funding
--            address is the operator's to release, not a refund (0033's rule).
--
-- False when no observation is at the coordinate, or for any other purpose.

CREATE FUNCTION source_bindable(
    p_purpose TEXT, p_network TEXT, p_block BIGINT, p_hash BYTEA, p_kind position_kind,
    p_index BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT COALESCE((
        SELECT o.executed
           AND o.token IS NOT NULL AND o.amount IS NOT NULL AND o.recipient IS NOT NULL
           AND o.on_chain_reference IS NULL
           AND EXISTS (SELECT 1 FROM networks n WHERE n.network = o.network AND n.role = 'origin')
           AND EXISTS (SELECT 1 FROM reserves r
                       WHERE r.network = o.network AND r.address = o.recipient)
           AND CASE p_purpose
               WHEN 'deposit' THEN
                   o.kind = 'conversion_output'
                   OR (o.kind = 'arrival'
                       AND NOT (o.sender IS NOT NULL
                                AND EXISTS (SELECT 1 FROM reserves r WHERE r.address = o.sender))
                       AND NOT (o.sender IS NOT NULL AND EXISTS (
                            SELECT 1 FROM routes v WHERE v.venue_addresses @> to_jsonb(o.sender))))
               WHEN 'refund' THEN
                   o.kind IN ('arrival', 'other_arrival', 'conversion_output')
               ELSE FALSE
           END
        FROM observations o
        WHERE o.network = p_network AND o.block_number = p_block AND o.block_hash = p_hash
          AND o.position_kind = p_kind AND o.position_index = p_index
    ), FALSE)
$$;

COMMENT ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT) IS
    'REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, REQ-CV-2, INV-2: whether the observation at a '
    'coordinate is one a deposit (p_purpose = deposit) or the refund of an unassociated arrival '
    '(p_purpose = refund) may name. Its finality is checked apart.';

-- The binding rule, on the insert of a transfer, on a binding made by an update, and again when
-- the source becomes final (review S2), as 0033's funding rule is. A coordinate with no
-- observation yet may be bound (the ingress refuses that, ERR-SourceNotFound); it may not become
-- final with no observation on a declared origin network.
CREATE FUNCTION transfers_source_bindable() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    purpose TEXT;
    binding BOOLEAN;
    finalising BOOLEAN;
    observed BOOLEAN;
BEGIN
    IF NEW.kind = 'deposit' THEN
        purpose := 'deposit';
    ELSIF NEW.kind = 'refund' AND NEW.refund_of IS NULL THEN
        purpose := 'refund';
    ELSE
        RETURN NEW;
    END IF;
    IF NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    binding := TG_OP = 'INSERT' OR OLD.source_network IS NULL;
    finalising := NEW.source_final_network IS NOT NULL
                  AND (TG_OP = 'INSERT' OR OLD.source_final_network IS NULL);
    IF NOT (binding OR finalising) THEN
        RETURN NEW;
    END IF;
    observed := EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index);
    IF finalising AND NOT observed
       AND EXISTS (SELECT 1 FROM networks WHERE network = NEW.source_network AND role = 'origin') THEN
        RAISE EXCEPTION '% refused: its source on % becomes final with no observation at its coordinate (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, INV-2)',
            NEW.kind, NEW.source_network
            USING ERRCODE = 'BZ001';
    END IF;
    IF observed
       AND NOT source_bindable(purpose, NEW.source_network, NEW.source_block_number,
                               NEW.source_block_hash, NEW.source_position_kind,
                               NEW.source_position_index) THEN
        RAISE EXCEPTION '% refused: the observation at its source coordinate is not of a kind a % may name (REQ-RQ-3, REQ-RQ-6, REQ-IN-3, REQ-IN-6, REQ-CV-1, INV-2)',
            NEW.kind, NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_source_bindable
    BEFORE INSERT OR UPDATE OF source_network, source_final_network ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_source_bindable();

-- ---------------------------------------------------------------------------------------------
-- Grants. No function here is PUBLIC's (0017's F1). The trigger functions fire without an EXECUTE
-- grant; observation_writer_covers, observation_kind_on_network and observation_kind_for_event run as the inserting role
-- inside them, so every service keeps them (as 0021 does for bezent_writer_holds): a login refused
-- an insert is refused by a guard, never by a missing EXECUTE.

REVOKE EXECUTE ON FUNCTION
    observation_kind_on_network(TEXT, TEXT),
    observation_kind_for_event(TEXT),
    observation_writer_covers(TEXT, OID),
    observations_guard_kind(),
    observation_outcomes_guard_scope(),
    source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT),
    transfers_source_bindable()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION observation_kind_on_network(TEXT, TEXT), observation_kind_for_event(TEXT),
    observation_writer_covers(TEXT, OID)
    TO bezent_service;
GRANT EXECUTE ON FUNCTION source_bindable(TEXT, TEXT, BIGINT, BYTEA, position_kind, BIGINT)
    TO bezent_service, bezent_core, bezent_operator;

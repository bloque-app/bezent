-- SPDX-License-Identifier: GPL-3.0-or-later
-- F-007 second review, should-fix S-a and S-b: who may write the signals and the issuing
-- network's observations (SPEC §8.2 REQ-NX-1/3, REQ-OB-6, REQ-IC-4, AC-I1.2, INV-8).
--
-- Numbered 0020, after this feature's 0018_kreivo_endpoint and 0019_endpoint_role, which follow
-- F-003's 0014, 0015 and 0017 (PR #32, merged first); it is additive.
--
-- S-a: the endpoint treats a `key-compromise` or `delivery-anomaly` row as already raised, so a
-- row written in advance would suppress the real one, and its `pause-requested` partner. Every
-- service logs in as a member of `bezent_service`, the internet-facing ingress included; so these
-- kinds may be appended only by a login that holds `bezent_endpoint` (an endpoint seeing the key
-- used on chain) or `bezent_core` (the core's own check of a signature the signer returned). Other
-- kinds (the ingress's `refusal`, …) are unchanged.
--
-- S-b: an observation of the issuing network at kind `transaction` tells the prover a signed
-- transaction executed, and stops its re-sends and its compromise check. Observations, and their
-- outcomes, of any network that is not a declared origin network may be written only by a login
-- that holds `bezent_endpoint`. Origin networks are unchanged until their endpoint's role is
-- settled (issue #33).
--
-- The migration owner, and a superuser, keep writing both, for drills and restores (NFR-7).

CREATE FUNCTION bezent_writer_holds(role_name TEXT, relation OID) RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT pg_has_role(current_user, role_name, 'USAGE')
        OR pg_has_role(current_user, (SELECT relowner FROM pg_class WHERE oid = relation), 'USAGE')
$$;

CREATE FUNCTION event_log_guard_signal() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.kind IN ('key-compromise', 'pause-requested', 'delivery-anomaly')
       AND NOT (bezent_writer_holds('bezent_endpoint', TG_RELID)
                OR pg_has_role(current_user, 'bezent_core', 'USAGE')) THEN
        RAISE EXCEPTION '% refused: only an endpoint or the core records it (AC-I1.2, REQ-IC-4)', NEW.kind
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER event_log_guard_signal
    BEFORE INSERT ON event_log
    FOR EACH ROW EXECUTE FUNCTION event_log_guard_signal();

CREATE FUNCTION observations_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM networks WHERE network = NEW.network AND role = 'origin')
       AND NOT bezent_writer_holds('bezent_endpoint', TG_RELID) THEN
        RAISE EXCEPTION 'observation on % refused: only an endpoint records the issuing network''s observations (REQ-OB-6, INV-8)', NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observations_guard_writer
    BEFORE INSERT ON observations
    FOR EACH ROW EXECUTE FUNCTION observations_guard_writer();

CREATE FUNCTION observation_outcomes_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM observations o JOIN networks n ON n.network = o.network
                   WHERE o.id = NEW.observation_id AND n.role = 'origin')
       AND NOT bezent_writer_holds('bezent_endpoint', TG_RELID) THEN
        RAISE EXCEPTION 'outcome of observation % refused: only an endpoint records the issuing network''s outcomes (REQ-OB-6, INV-8)', NEW.observation_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER observation_outcomes_guard_writer
    BEFORE INSERT ON observation_outcomes
    FOR EACH ROW EXECUTE FUNCTION observation_outcomes_guard_writer();

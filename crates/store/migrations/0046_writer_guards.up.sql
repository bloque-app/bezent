-- SPDX-License-Identifier: GPL-3.0-or-later
-- Review of PR #56 (issues #35, #46): who may record what a transfer's state and the log rest on.
-- Additive: new guards and one function; no table changes.
--
--   * M1 (INV-6, REQ-RQ-7; CLAUDE.md guardrails 2 and 3). The ingress, which faces the internet,
--     records only what a request causes: a request's own transfer, and its transition out of
--     `Received` (to `Accepted` or `Refused`), resting on no chain data. Every other transition
--     (Source-Final, Expired, Withdrawn, Delivering, Delivered, Completed, Refunding, Refunded, a
--     condition taken or cleared, a review and its exit) rests on what the endpoints observed and
--     is the core's alone. And whoever writes it, a transfer reaches `Source-Final` (by a
--     transition, or by an insert at Source-Final, REQ-IN-3) only when the observation it is bound
--     to is final: an outcome `final` (so not `withdrawn`) decided at a recorded final head at or
--     above the observed block, hash matching, and no recorded final head at the observed height
--     with another hash (REQ-IN-2: the block at that height still has the hash observed). Until
--     now the store only checked that *an* observation was at the coordinate.
--   * S2 (AC-I1.2, REQ-ST-4). The log's entries are written only by the role whose events they
--     record: `transfer` and `transition` by the store itself (the owner's triggers); `refusal` by
--     the ingress; the signals by an endpoint or the core (0020); every other kind (conditions,
--     reviews, the reserve check's pages) by the core. No service forges another's entry.
--   * S4 (#46). `bezent_applied_migrations()` lists the migrations applied to the store, so a
--     service compares the whole set with the one it was built with, not only its grants.
--   * M2 (#46). No store role holds a membership of another with ADMIN OPTION.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE m.admin_option
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_ingress',
                                   'bezent_endpoint', 'bezent_kreivo_endpoint',
                                   'bezent_evm_endpoint', 'bezent_webhooks', 'bezent_signer',
                                   'bezent_operator')) THEN
        RAISE EXCEPTION 'a store role must not hold another with ADMIN OPTION (#46)'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

-- ---------------------------------------------------------------------------------------------
-- M1: who records a transition.
--
-- Runs as the inserting login. The core (and the migration owner, for drills and restores, NFR-7)
-- records any edge the lifecycle admits. The ingress records only a request's decision: Received ->
-- Accepted or Refused of a transfer a request makes (a deposit, or the refund of an unassociated
-- arrival), carrying no coordinate, attempt or manual action. Anyone else records nothing.
CREATE FUNCTION transitions_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
    t_kind    TEXT;
    t_refund  BIGINT;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(current_user, owner_oid, 'USAGE')
       OR pg_has_role(current_user, 'bezent_core', 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF pg_has_role(current_user, 'bezent_ingress', 'USAGE')
       AND NEW.from_state = 'Received' AND NEW.to_state IN ('Accepted', 'Refused')
       AND NEW.network IS NULL AND NEW.attempt IS NULL AND NEW.manual_action_id IS NULL THEN
        SELECT kind, refund_of INTO t_kind, t_refund FROM transfers WHERE id = NEW.transfer_id;
        IF t_kind = 'deposit' OR (t_kind = 'refund' AND t_refund IS NULL) THEN
            RETURN NEW;
        END IF;
    END IF;
    RAISE EXCEPTION 'transition % -> % refused for %: the ingress records only a request''s decision out of Received; every transition that rests on chain data is the core''s alone (INV-6, REQ-RQ-7, REQ-TR-6)',
        NEW.from_state, NEW.to_state, current_user
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER transitions_guard_writer
    BEFORE INSERT ON transitions
    FOR EACH ROW EXECUTE FUNCTION transitions_guard_writer();

-- M1: Source-Final only on a final, unwithdrawn observation of the transfer's source.
--
-- On the insert of a transfer at Source-Final and on the write of its source-final milestone (which
-- only the lifecycle's trigger makes, for Accepted -> Source-Final and a review's exit into it).
-- SECURITY DEFINER: it reads the final heads, which the ingress does not; it writes nothing.
CREATE FUNCTION transfers_source_verified_final() RETURNS trigger
    LANGUAGE plpgsql
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
DECLARE
    o RECORD;
BEGIN
    IF NEW.source_final_network IS NULL
       OR (TG_OP = 'UPDATE' AND OLD.source_final_network IS NOT NULL) THEN
        RETURN NEW;
    END IF;
    SELECT ob.network, ob.block_number, ob.block_hash, x.outcome, x.final_block_number,
           x.final_block_hash
    INTO o
    FROM observations ob
    LEFT JOIN observation_outcomes x ON x.observation_id = ob.id
    WHERE ob.network = NEW.source_network AND ob.block_number = NEW.source_block_number
      AND ob.block_hash = NEW.source_block_hash
      AND ob.position_kind = NEW.source_position_kind
      AND ob.position_index = NEW.source_position_index;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'transfer %: Source-Final needs the observation at its source coordinate, final (INV-6, REQ-IN-2, REQ-RQ-7)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF o.outcome IS DISTINCT FROM 'final' THEN
        RAISE EXCEPTION 'transfer %: its source observation is %, not final; nothing reaches Source-Final before its arrival is final (INV-6, REQ-IN-2, REQ-IN-7, REQ-RQ-7)',
            NEW.bridge_reference, coalesce(o.outcome, 'undecided')
            USING ERRCODE = 'BZ001';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM final_heads h
                   WHERE h.network = o.network AND h.block_number = o.final_block_number
                     AND h.block_hash = o.final_block_hash AND h.block_number >= o.block_number) THEN
        RAISE EXCEPTION 'transfer %: its source observation''s final point is not a recorded final head at or above the observed block (INV-6, REQ-IN-2, REQ-OB-2)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM final_heads h
               WHERE h.network = o.network AND h.block_number = o.block_number
                 AND h.block_hash <> o.block_hash) THEN
        RAISE EXCEPTION 'transfer %: the final block at its source''s height has another hash than the one observed (INV-6, REQ-IN-2, REQ-IN-7)',
            NEW.bridge_reference
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_source_verified_final
    BEFORE INSERT OR UPDATE OF source_final_network ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_source_verified_final();

-- ---------------------------------------------------------------------------------------------
-- S2: who writes which entry of the log. Runs as the inserting login; the owner's triggers
-- (`transfers_log_created`, `transitions_apply_lifecycle`) write as the owner.
CREATE FUNCTION event_log_guard_writer() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
DECLARE
    owner_oid OID;
BEGIN
    SELECT relowner INTO owner_oid FROM pg_class WHERE oid = TG_RELID;
    IF pg_has_role(current_user, owner_oid, 'USAGE') THEN
        RETURN NEW;
    END IF;
    IF NEW.kind IN ('transfer', 'transition') THEN
        RAISE EXCEPTION '% refused: only the store records a transfer''s creation and its transitions (REQ-ST-4)',
            NEW.kind
            USING ERRCODE = 'BZ001';
    ELSIF NEW.kind = 'refusal' THEN
        IF pg_has_role(current_user, 'bezent_ingress', 'USAGE') THEN
            RETURN NEW;
        END IF;
        RAISE EXCEPTION 'refusal refused: only the ingress records a request''s refusal (REQ-ST-4, REQ-RQ-7)'
            USING ERRCODE = 'BZ001';
    ELSIF NEW.kind IN ('key-compromise', 'pause-requested', 'delivery-anomaly') THEN
        -- event_log_guard_signal (0020): an endpoint or the core.
        RETURN NEW;
    ELSIF pg_has_role(current_user, 'bezent_core', 'USAGE') THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION '% refused: only the core records it (AC-I1.2, REQ-ST-4, REQ-RQ-7)', NEW.kind
        USING ERRCODE = 'BZ001';
END
$$;

CREATE TRIGGER event_log_guard_writer
    BEFORE INSERT ON event_log
    FOR EACH ROW EXECUTE FUNCTION event_log_guard_writer();

-- ---------------------------------------------------------------------------------------------
-- S4: the migrations applied to the store, for the services' start-up check. SECURITY DEFINER:
-- the migrations' own table is the owner's.
CREATE FUNCTION bezent_applied_migrations()
    RETURNS TABLE (version BIGINT, checksum BYTEA)
    LANGUAGE sql STABLE
    SECURITY DEFINER
    SET search_path = public, pg_temp
    AS $$
    SELECT m.version, m.checksum FROM _sqlx_migrations m WHERE m.success ORDER BY m.version
$$;

REVOKE EXECUTE ON FUNCTION
    transitions_guard_writer(),
    transfers_source_verified_final(),
    event_log_guard_writer(),
    bezent_applied_migrations()
FROM PUBLIC;

GRANT EXECUTE ON FUNCTION bezent_applied_migrations()
    TO bezent_core, bezent_ingress, bezent_kreivo_endpoint, bezent_evm_endpoint, bezent_webhooks,
       bezent_signer, bezent_operator;

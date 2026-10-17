-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-004, verification of the review fixes of PR #40 (MF1, SF2, SF3, SF5). Additive to 0031.
--
--   * funding_arrival(): whether an arrival is from an address the operator declared it funds the
--     asset's reserve from, from the block of the declaration on. One rule for the store, the
--     ingress and the core.
--   * transfers: a deposit never binds such an arrival, and never reaches Source-Final on one. An
--     operator top-up is a surplus contribution; a requester can never have it minted (MF1; INV-1,
--     INV-2, REQ-IN-3).
--   * surplus_funding_addresses.from_block: a declaration applies only from the block it is
--     declared at, past every block recorded on the network, never to an arrival already observed
--     (SF3). Declarations made before this migration carry none and apply to nothing.
--   * networks.reserve_point_lag_seconds: at most an hour, and, when it is set, at most the
--     completion budget of every route the network serves (SF2; NFR-6: beyond the route's budget it
--     is REQ-IC-2).
--   * reserve_evaluations.book_entry_cut: the last book entry an evaluation read, so the statement
--     lists exactly the movements of surplus the evaluation's book holds (SF5).

-- ---------------------------------------------------------------------------------------------
-- Funding declarations apply from their block (SF3).

ALTER TABLE surplus_funding_addresses
    ADD COLUMN from_block BIGINT CHECK (from_block >= 0);

COMMENT ON COLUMN surplus_funding_addresses.from_block IS
    'REQ-BK-5, INV-1: the first block of the network the declaration applies to; past every block '
    'recorded on the network when it is declared, so no arrival already observed is reclassified. '
    'NULL (a declaration made before migration 0032) applies to nothing. A funding address must be '
    'controlled by the operator alone: never a custodial or shared address, such as an exchange '
    'hot wallet, whose arrivals may be customer deposits.';

CREATE FUNCTION surplus_funding_from_block() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    latest BIGINT;
BEGIN
    SELECT GREATEST((SELECT max(o.block_number) FROM observations o WHERE o.network = NEW.network),
                    (SELECT max(h.block_number) FROM final_heads h WHERE h.network = NEW.network))
      INTO latest;
    IF NEW.from_block IS NULL THEN
        NEW.from_block := COALESCE(latest + 1, 0);
    ELSIF NEW.from_block <= COALESCE(latest, -1) THEN
        RAISE EXCEPTION 'funding declaration refused: it applies from block %, at or before block %, already recorded on %; a declaration is never retroactive (REQ-BK-5, INV-1)',
            NEW.from_block, latest, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER surplus_funding_addresses_from_block
    BEFORE INSERT ON surplus_funding_addresses
    FOR EACH ROW EXECUTE FUNCTION surplus_funding_from_block();

-- One rule for whether an arrival is the operator's own. EVM addresses compare without regard to
-- case; any other format exactly (SS58 is case-significant).
CREATE FUNCTION funding_arrival(p_asset TEXT, p_network TEXT, p_sender TEXT, p_block BIGINT)
    RETURNS BOOLEAN
    LANGUAGE sql STABLE
    AS $$
    SELECT p_sender IS NOT NULL AND EXISTS (
        SELECT 1 FROM surplus_funding_addresses f JOIN networks n ON n.network = f.network
        WHERE f.asset = p_asset AND f.network = p_network
          AND f.from_block IS NOT NULL AND p_block >= f.from_block
          AND (f.address = p_sender
               OR (n.address_format = 'evm' AND lower(f.address) = lower(p_sender))))
$$;

COMMENT ON FUNCTION funding_arrival(TEXT, TEXT, TEXT, BIGINT) IS
    'REQ-BK-5, REQ-IN-3: whether an arrival at a reserve from p_sender at p_block is a surplus '
    'contribution of the operator''s (never owed mint, never bound by a deposit).';

-- ---------------------------------------------------------------------------------------------
-- A deposit never binds an operator top-up (MF1).

CREATE FUNCTION transfers_refuse_funding_source() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF NEW.kind <> 'deposit' OR NEW.source_network IS NULL THEN
        RETURN NEW;
    END IF;
    IF TG_OP = 'UPDATE'
       AND NOT (OLD.source_final_network IS NULL AND NEW.source_final_network IS NOT NULL) THEN
        RETURN NEW;
    END IF;
    IF EXISTS (
        SELECT 1 FROM observations o
        WHERE o.network = NEW.source_network AND o.block_number = NEW.source_block_number
          AND o.block_hash = NEW.source_block_hash
          AND o.position_kind = NEW.source_position_kind
          AND o.position_index = NEW.source_position_index
          AND funding_arrival(NEW.asset, o.network, o.sender, o.block_number)) THEN
        RAISE EXCEPTION 'deposit refused: the arrival at its source coordinate is from an operator funding address, a surplus contribution no deposit binds (INV-1, INV-2, REQ-IN-3)'
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER transfers_source_not_funding
    BEFORE INSERT OR UPDATE OF source_final_network ON transfers
    FOR EACH ROW EXECUTE FUNCTION transfers_refuse_funding_source();

-- ---------------------------------------------------------------------------------------------
-- The lag bound's ceiling (SF2).

ALTER TABLE networks
    ADD CONSTRAINT networks_reserve_point_lag_at_most_an_hour
        CHECK (reserve_point_lag_seconds <= 3600);

CREATE FUNCTION reserve_point_lag_within_budget() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
DECLARE
    budget BIGINT;
BEGIN
    SELECT min(r.completion_budget_seconds) INTO budget FROM routes r
    WHERE r.network = NEW.network OR NEW.role = 'issuing';
    IF NEW.reserve_point_lag_seconds > budget THEN
        RAISE EXCEPTION 'lag bound refused: % s exceeds the completion budget % s of a route % serves (NFR-5, NFR-6, REQ-IC-2)',
            NEW.reserve_point_lag_seconds, budget, NEW.network
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER networks_reserve_point_lag_within_budget
    BEFORE UPDATE OF reserve_point_lag_seconds ON networks
    FOR EACH ROW EXECUTE FUNCTION reserve_point_lag_within_budget();

COMMENT ON COLUMN networks.reserve_point_lag_seconds IS
    'NFR-5, REQ-IC-2: the most, in block time, the final point the reserve check reads this network '
    'at may lag its latest recorded final head, and the most the latest recorded final head may lag '
    'the clock: one minute plus the network''s finality time. At most an hour, and set no higher '
    'than the completion budget of any route the network serves. Beyond it the evaluation diverges; an '
    'evaluation that cannot be made for longer pauses too.';

-- ---------------------------------------------------------------------------------------------
-- The book an evaluation read (SF5).

ALTER TABLE reserve_evaluations
    ADD COLUMN book_entry_cut BIGINT CHECK (book_entry_cut >= 0);

COMMENT ON COLUMN reserve_evaluations.book_entry_cut IS
    'REQ-BK-1, REQ-PR-1: the last book entry the evaluation read; the book''s terms and the '
    'statement''s movements of surplus include no later entry.';

-- ---------------------------------------------------------------------------------------------
-- Grants. No function here is PUBLIC's; the trigger functions fire without an EXECUTE grant.

REVOKE EXECUTE ON FUNCTION surplus_funding_from_block() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION funding_arrival(TEXT, TEXT, TEXT, BIGINT) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION transfers_refuse_funding_source() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION reserve_point_lag_within_budget() FROM PUBLIC;

GRANT EXECUTE ON FUNCTION funding_arrival(TEXT, TEXT, TEXT, BIGINT)
    TO bezent_service, bezent_core, bezent_operator;

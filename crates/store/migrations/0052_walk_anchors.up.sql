-- SPDX-License-Identifier: GPL-3.0-or-later
-- Issue #49 (review of PR #54): a first start walks about 81,000 headers from the finalized head
-- down to the contract's instantiation before it can read a single block, and nothing of that walk
-- survived an error or a restart. Additive: one table, its guards and its grants.
--
--   * `observation_walk_anchors`: blocks an endpoint reached from a final head by parent hash,
--     one every few thousand headers of such a walk. A later walk resumes from the lowest anchor
--     above the watermark instead of from the head; and each rung between two anchors is read and
--     recorded on its own, so the watermark advances as the walk goes.
--
-- An anchor is the endpoint's own record of a block it verified to be on the final chain; it rests
-- on the final head it names (`final_block_*`, itself checked against the chain), and a final block
-- is never replaced, so an anchor never goes stale. It is not a final head: `final_heads` is read
-- as "the newest row is the network's head" and carries a block time, which a mid-chain anchor
-- does not have, so an anchor there would move the head backwards.
--
-- Append-only, one hash per height (two at one height is an error to surface, never to overwrite),
-- and each endpoint's login records its own networks' rows only (0049).

CREATE TABLE observation_walk_anchors (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    network            TEXT NOT NULL CHECK (network <> ''),
    block_number       BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash         BYTEA NOT NULL CHECK (length(block_hash) = 32),
    -- The final head the walk started from, which the block was reached from.
    final_block_number BIGINT NOT NULL,
    final_block_hash   BYTEA NOT NULL CHECK (length(final_block_hash) > 0),
    recorded_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (final_block_number >= block_number),
    CONSTRAINT observation_walk_anchors_one_hash_per_height UNIQUE (network, block_number)
);

COMMENT ON TABLE observation_walk_anchors IS
    'Issue #49, REQ-OB-3: blocks reached from a final head by parent hash, so a long walk resumes '
    'from the last one and is read in rungs. Not a final head. Append-only; written by an endpoint.';

CREATE TRIGGER observation_walk_anchors_append_only
    BEFORE UPDATE OR DELETE ON observation_walk_anchors
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-OB-3');
CREATE TRIGGER observation_walk_anchors_no_truncate
    BEFORE TRUNCATE ON observation_walk_anchors
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-OB-3');
CREATE TRIGGER observation_walk_anchors_endpoint_scope
    BEFORE INSERT ON observation_walk_anchors
    FOR EACH ROW EXECUTE FUNCTION endpoint_guard_scope();

GRANT SELECT, INSERT ON observation_walk_anchors TO bezent_kreivo_endpoint;

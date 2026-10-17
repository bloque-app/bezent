-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-002 T-002-02 to T-002-05: what the request and query surfaces read and record (SPEC §3, §5.1,
-- §7.A, §7.B, §8.3, §8.4; PLAN DEC-15).
--
-- Declarations (written by the operator, read by the services; the operator surface that edits
-- them is F-014):
--   * networks: the issuing network and the origin networks, with the address format a
--     destination on each must have (ERR-InvalidDestination);
--   * assets and admitted_tokens: each bridged asset with its assurance class (REQ-BE-2), and each
--     origin token admitted for it on a network, with its precision (REQ-AS-2, REQ-AS-4);
--   * reserves: the reserve address of each asset on its network (REQ-AS-8, REQ-BK-5);
--   * routes: the machine-readable route table with its limits (REQ-AS-5, REQ-LM-1);
--   * pauses: a pause per route, per asset, per direction or global (REQ-GV-1);
--   * requesters, requester_grants, requester_credentials: who may call, for which kind of
--     request, with which credential, hashed at rest (REQ-PN-1, REQ-PN-2).
--
-- Records (append-only, written by the ingress):
--   * intake_answers: what bezent answered each request, once, in the request's own transaction
--     (REQ-RQ-1, REQ-ST-7). It links the request to the transfer it made or reached, so a
--     requester reads only its own transfers (REQ-ID-4, DEC-15);
--   * relayed_instructions: an attested backend's payout instructions, relayed for a redemption's
--     commitment and accepted only when they hash to it (REQ-RQ-9).

CREATE TABLE networks (
    network        TEXT PRIMARY KEY CHECK (network <> ''),
    role           TEXT NOT NULL CHECK (role IN ('issuing', 'origin')),
    -- How a destination on this network is checked (ERR-InvalidDestination).
    address_format TEXT NOT NULL CHECK (address_format IN ('evm', 'ss58', 'opaque'))
);

-- One issuing network (SPEC §0, "Networks and backends").
CREATE UNIQUE INDEX networks_one_issuing ON networks (role) WHERE role = 'issuing';

CREATE TABLE assets (
    asset           TEXT PRIMARY KEY CHECK (asset <> ''),
    -- The bridged asset's precision on the issuing network (REQ-AS-4).
    precision       SMALLINT NOT NULL CHECK (precision BETWEEN 0 AND 38),
    assurance_class TEXT NOT NULL CHECK (assurance_class IN ('public', 'attested'))
);

CREATE TABLE admitted_tokens (
    network   TEXT NOT NULL REFERENCES networks (network),
    token     TEXT NOT NULL CHECK (token <> ''),
    asset     TEXT NOT NULL REFERENCES assets (asset),
    -- At least the asset's precision (REQ-AS-4); the ratio is ten to the difference.
    precision SMALLINT NOT NULL CHECK (precision BETWEEN 0 AND 38),
    PRIMARY KEY (network, token)
);

CREATE TABLE reserves (
    asset   TEXT NOT NULL REFERENCES assets (asset),
    network TEXT NOT NULL REFERENCES networks (network),
    address TEXT NOT NULL CHECK (address <> ''),
    PRIMARY KEY (asset, network),
    UNIQUE (network, address)
);

-- REQ-LM-1: per route, minimum and maximum per transfer, window limits, cut-offs, capacity, any
-- venue minimum, the venue addresses, and the completion budget. Amounts are in the origin token's
-- minor units (REQ-LM-1: "expressed in the origin token").
CREATE TABLE routes (
    asset                     TEXT NOT NULL REFERENCES assets (asset),
    network                   TEXT NOT NULL REFERENCES networks (network),
    direction                 TEXT NOT NULL CHECK (direction IN ('deposit', 'withdrawal')),
    minimum                   minor_units NOT NULL CHECK (minimum >= 1),
    maximum                   minor_units NOT NULL,
    window_limit              minor_units NOT NULL,
    window_seconds            BIGINT NOT NULL CHECK (window_seconds > 0),
    -- Transfers the route carries at once, not yet terminal.
    capacity                  BIGINT NOT NULL CHECK (capacity >= 0),
    cutoffs                   JSONB NOT NULL DEFAULT '[]'::jsonb,
    venue_minimum             minor_units,
    venue_addresses           JSONB NOT NULL DEFAULT '[]'::jsonb,
    completion_budget_seconds BIGINT NOT NULL CHECK (completion_budget_seconds > 0),
    PRIMARY KEY (asset, network, direction),
    CONSTRAINT routes_bounds_ordered CHECK (minimum <= maximum AND maximum <= window_limit)
);

COMMENT ON COLUMN routes.minimum IS
    'INV-11, REQ-LM-1: exact integer count of the origin token''s minor units.';

-- REQ-GV-1: a NULL column matches every value, so (NULL, NULL, NULL) pauses the bridge.
CREATE TABLE pauses (
    id        BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    asset     TEXT REFERENCES assets (asset),
    network   TEXT REFERENCES networks (network),
    direction TEXT CHECK (direction IN ('deposit', 'withdrawal'))
);

CREATE TABLE requesters (
    requester TEXT PRIMARY KEY CHECK (requester <> '')
);

-- REQ-PN-2: one grant per kind. `attest` is for an attested backend's reports (F-011).
CREATE TABLE requester_grants (
    requester TEXT NOT NULL REFERENCES requesters (requester),
    grant_kind TEXT NOT NULL CHECK (grant_kind IN ('deposit', 'withdrawal', 'refund', 'attest')),
    PRIMARY KEY (requester, grant_kind)
);

-- REQ-PN-1: a credential is an id and a secret; only the secret's SHA-256 is stored.
CREATE TABLE requester_credentials (
    credential_id TEXT PRIMARY KEY CHECK (credential_id <> '' AND position('.' IN credential_id) = 0),
    requester     TEXT NOT NULL REFERENCES requesters (requester),
    secret_sha256 BYTEA NOT NULL CHECK (length(secret_sha256) = 32)
);

CREATE TABLE intake_answers (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    intake_id        BIGINT NOT NULL UNIQUE REFERENCES intake (id),
    -- The transfer the request made or reached; NULL when it was refused before any transfer
    -- could be named (ERR-SourceNotFound, and a withdrawal's extras that bound nothing).
    transfer_id      BIGINT REFERENCES transfers (id),
    bridge_reference TEXT NOT NULL CHECK (bridge_reference <> ''),
    error            refusal_error,
    answered_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT intake_answers_names_something CHECK (transfer_id IS NOT NULL OR error IS NOT NULL)
);

CREATE INDEX intake_answers_transfer ON intake_answers (transfer_id) WHERE transfer_id IS NOT NULL;
CREATE UNIQUE INDEX intake_answers_bridge_reference
    ON intake_answers (bridge_reference) WHERE transfer_id IS NULL;

CREATE TABLE relayed_instructions (
    id           BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    transfer_id  BIGINT NOT NULL UNIQUE REFERENCES transfers (id),
    intake_id    BIGINT NOT NULL REFERENCES intake (id),
    instructions BYTEA NOT NULL CHECK (length(instructions) > 0),
    commitment   BYTEA NOT NULL CHECK (length(commitment) = 32)
);

CREATE INDEX observations_recipient ON observations (network, recipient);
CREATE INDEX transfers_source_coordinate
    ON transfers (source_network, source_block_number, source_block_hash,
                  source_position_kind, source_position_index)
    WHERE source_network IS NOT NULL;
CREATE INDEX transfers_route_created ON transfers (asset, kind, created_at);

CREATE TRIGGER intake_answers_append_only
    BEFORE UPDATE OR DELETE ON intake_answers
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-RQ-1, REQ-ST-7');
CREATE TRIGGER intake_answers_no_truncate
    BEFORE TRUNCATE ON intake_answers
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-RQ-1, REQ-ST-7');
CREATE TRIGGER relayed_instructions_append_only
    BEFORE UPDATE OR DELETE ON relayed_instructions
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-RQ-9, REQ-ST-7');
CREATE TRIGGER relayed_instructions_no_truncate
    BEFORE TRUNCATE ON relayed_instructions
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-RQ-9, REQ-ST-7');

-- The services read the declarations and append the records; they change neither (0009_roles).
GRANT SELECT ON
    networks, assets, admitted_tokens, reserves, routes, pauses,
    requesters, requester_grants, requester_credentials
    TO bezent_service;
GRANT SELECT, INSERT ON intake_answers, relayed_instructions TO bezent_service;

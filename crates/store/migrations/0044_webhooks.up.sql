-- SPDX-License-Identifier: AGPL-3.0-or-later
-- F-002, M-1 part (issue #2): webhooks to requesters (SPEC §7.C REQ-ST-5, REQ-IC-5; REQ-ID-4;
-- PLAN DEC-15). Additive: no earlier table changes.
--
-- The sequenced log (`event_log`, 0007) stays the source of truth. A notification only points at a
-- position in it; a requester that never receives one loses nothing, because it can read the log
-- from its last position (REQ-ST-4, REQ-ST-5: "no requester may need one to arrive").
--
--   * webhook_secrets: each requester's signing secret, sealed (ChaCha20-Poly1305) under the
--     ingress's webhook key, which lives in a file the ingress reads, never in the store. An HMAC
--     key must be readable to sign, so it cannot be hashed like a credential; it moves no value,
--     so it is not the signer's (REQ-AR-3 is about keys that move value). Append-only: the
--     latest row is the one in use, an earlier one stays as a record of the rotation.
--   * webhook_transfer_urls: the callback URL of a (requester, transfer), set only by a requester
--     whose request made or reached the transfer (REQ-ID-4). Its own requester may replace it
--     (Swaps issues a new callback token on each retry): the table is append-only, the latest row
--     is the URL in use, and every row records the change: who (the requester), when, and the
--     keyed digest (HMAC-SHA256 under a subkey of the webhook key; PR #55 review S5) of the new
--     URL and of the one it replaced, so a reader of the store cannot test a guess of a token in
--     a URL offline. The URL is sealed: a requester's
--     callback URL may carry its own access token.
--   * webhook_condition_urls: a requester's URL for conditions (REQ-IC-5); append-only, the
--     latest is in use.
--   * webhook_cursor: how far the dispatcher has fanned the log out into deliveries. It only
--     moves forward.
--   * webhook_condition_holds: which transfers a shared gate condition holds (CND-Paused,
--     CND-CutOff, CND-ReserveShort), per route, so a condition is notified once per (condition,
--     route) when the first transfer is held and once when the last is released, not once per
--     transfer, and to every requester granted on the route (REQ-IC-5).
--   * webhook_deliveries: the outbox. One row per (requester, de-duplication key), so fanning an
--     entry out twice delivers it once; a row is retried with back-off until acknowledged
--     (at-least-once) and is dead-lettered after a bound. A delivery ends delivered or dead and
--     never changes after. A row carries no body and no URL: only what it points at (the log
--     position, the transfer, the condition and whether it started or cleared). The dispatcher
--     builds the body from the log entry at send time, checks the row against that entry, and
--     posts to the URL current then, so a row it did not write is never signed as it stands
--     (PR #55 review M2).
--
-- The dispatcher has a role of its own, `bezent_webhooks` (PR #55 review M2). Only it reads the
-- sealed signing secrets and URLs, moves the cursor, keeps the condition holds, and writes
-- deliveries; a trigger refuses a delivery written by any other role, so a service login (the
-- ingress, the core, an endpoint, all members of `bezent_service`) cannot have bezent sign and
-- post a notification it never derived from the log. The ingress keeps INSERT on secrets and URLs
-- (registration) and reads its requesters' deliveries; it never reads a sealed column. The role is
-- a member of nothing: it is granted SELECT on the few tables it reads, and nothing else.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_webhooks') THEN
        BEGIN
            CREATE ROLE bezent_webhooks NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;

    -- The ingress API's own role: the only one that registers URLs and makes secrets (PR #55
    -- re-check S-c). It inherits bezent_service, so the ingress login holds this role alone.
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'bezent_ingress') THEN
        BEGIN
            CREATE ROLE bezent_ingress NOLOGIN;
        EXCEPTION WHEN duplicate_object OR unique_violation THEN
            NULL;  -- created concurrently by another database's migration on the same server
        END;
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_ingress' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_ingress must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE g.rolname = 'bezent_ingress'
                 AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_endpoint',
                                   'bezent_kreivo_endpoint', 'bezent_evm_endpoint',
                                   'bezent_signer', 'bezent_operator', 'bezent_webhooks')) THEN
        RAISE EXCEPTION 'no role but an ingress login may be a member of bezent_ingress'
            USING ERRCODE = 'BZ001';
    END IF;
    GRANT bezent_service TO bezent_ingress;

    IF current_user = 'bezent_webhooks'
       OR EXISTS (SELECT 1 FROM pg_auth_members m
                  JOIN pg_roles g ON g.oid = m.roleid
                  JOIN pg_roles u ON u.oid = m.member
                  WHERE g.rolname = 'bezent_webhooks' AND u.rolname = current_user) THEN
        RAISE EXCEPTION 'migrations run as the migration owner, never as bezent_webhooks (%)', current_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF EXISTS (SELECT 1 FROM pg_roles
               WHERE rolname = 'bezent_webhooks' AND (rolsuper OR rolbypassrls OR rolcreaterole)) THEN
        RAISE EXCEPTION 'bezent_webhooks must not be a superuser, bypass row security or create roles'
            USING ERRCODE = 'BZ001';
    END IF;
    -- The services' roles never hold it, and it holds no role (PR #55 review M2).
    IF EXISTS (SELECT 1 FROM pg_auth_members m
               JOIN pg_roles g ON g.oid = m.roleid
               JOIN pg_roles u ON u.oid = m.member
               WHERE (g.rolname = 'bezent_webhooks'
                      AND u.rolname IN ('bezent_service', 'bezent_core', 'bezent_endpoint',
                                        'bezent_kreivo_endpoint', 'bezent_evm_endpoint',
                                        'bezent_signer', 'bezent_operator', 'bezent_ingress'))
                  OR u.rolname = 'bezent_webhooks') THEN
        RAISE EXCEPTION 'bezent_webhooks and the services'' roles must not be members of each other (REQ-ST-5)'
            USING ERRCODE = 'BZ001';
    END IF;
END
$$;

CREATE TABLE webhook_secrets (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    requester  TEXT NOT NULL REFERENCES requesters (requester),
    -- ChaCha20-Poly1305: a 96-bit nonce, and a 32-byte secret with its 16-byte tag. The
    -- requester's name is the associated data, so a sealed secret cannot be moved to another row.
    nonce      BYTEA NOT NULL CHECK (length(nonce) = 12),
    sealed     BYTEA NOT NULL CHECK (length(sealed) = 48),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhook_secrets_latest ON webhook_secrets (requester, id DESC);

CREATE TABLE webhook_transfer_urls (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    -- The actor of the change is always this requester (REQ-ID-4): no one else can set it.
    requester     TEXT NOT NULL REFERENCES requesters (requester),
    transfer_id   BIGINT NOT NULL REFERENCES transfers (id),
    url_nonce     BYTEA NOT NULL CHECK (length(url_nonce) = 12),
    url_sealed    BYTEA NOT NULL CHECK (length(url_sealed) > 16),
    -- The record of the change: keyed digests of the new URL and of the one it replaced (NULL on the
    -- first registration). The URLs themselves stay sealed.
    url_digest    BYTEA NOT NULL CHECK (length(url_digest) = 32),
    previous_digest BYTEA CHECK (previous_digest IS NULL OR length(previous_digest) = 32),
    -- The log's head when it was registered: entries after the first registration are notified,
    -- earlier ones are already in the log for the requester to read.
    after_seq     BIGINT NOT NULL CHECK (after_seq >= 0),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhook_transfer_urls_latest ON webhook_transfer_urls (requester, transfer_id, id DESC);
CREATE INDEX webhook_transfer_urls_transfer ON webhook_transfer_urls (transfer_id);

CREATE TABLE webhook_condition_urls (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    requester     TEXT NOT NULL REFERENCES requesters (requester),
    url_nonce     BYTEA NOT NULL CHECK (length(url_nonce) = 12),
    url_sealed    BYTEA NOT NULL CHECK (length(url_sealed) > 16),
    registered_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX webhook_condition_urls_latest ON webhook_condition_urls (requester, id DESC);

CREATE TABLE webhook_cursor (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    after     BIGINT NOT NULL CHECK (after >= 0)
);

-- The dispatcher starts at the log's head when this migration runs: entries from before webhooks
-- existed had nobody registered for them, and the log already holds them.
INSERT INTO webhook_cursor (singleton, after) SELECT TRUE, seq FROM event_log_head;

CREATE TABLE webhook_condition_holds (
    transfer_id BIGINT NOT NULL PRIMARY KEY REFERENCES transfers (id),
    condition   transfer_condition NOT NULL
        CHECK (condition IN ('CND-Paused', 'CND-CutOff', 'CND-ReserveShort')),
    -- The route the transfer travels: '<asset or token>/<network>/<kind>' (webhooks.rs).
    route       TEXT NOT NULL CHECK (route <> ''),
    -- A transfer holds one condition at a time.
    since_seq   BIGINT NOT NULL CHECK (since_seq >= 1)
);

CREATE INDEX webhook_condition_holds_route ON webhook_condition_holds (condition, route);

CREATE TABLE webhook_deliveries (
    id              BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    requester       TEXT NOT NULL REFERENCES requesters (requester),
    -- The log position the notification points at, and its stable de-duplication key (REQ-ST-5).
    -- No foreign key to event_log: the log's own triggers refuse its truncation (REQ-ST-4), and a
    -- reference would answer first.
    seq             BIGINT NOT NULL CHECK (seq >= 1),
    dedupe_key      TEXT NOT NULL CHECK (dedupe_key <> ''),
    -- The transfer a 'transfer' delivery is about: it is sent to the callback URL current when it
    -- is attempted (the requester may have replaced it), not the one it was fanned out to.
    transfer_id     BIGINT REFERENCES transfers (id),
    -- 'transfer' (to the transfer's callback URL) or 'condition' (to the condition URL), each sent
    -- to the URL current when it is attempted.
    target          TEXT NOT NULL CHECK (target IN ('transfer', 'condition')),
    -- A condition delivery: the shared gate condition, and whether it started or cleared.
    condition       TEXT,
    change          TEXT,
    state           TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered', 'dead')),
    attempts        INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    leased_until    TIMESTAMPTZ,
    last_status     INTEGER,
    -- Only a coarse class, shown to the requester (PR #55 review M1): never an address, an error
    -- text or a response body, so the listing cannot probe what answers where.
    last_error      TEXT CHECK (last_error IS NULL OR last_error IN (
                        'no-secret', 'url-unopenable', 'url-not-allowed', 'address-not-allowed',
                        'timeout', 'connect-failed', 'http-status', 'request-failed',
                        'notice-refused')),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at     TIMESTAMPTZ,
    UNIQUE (requester, dedupe_key),
    CONSTRAINT webhook_deliveries_finished CHECK ((state = 'pending') = (finished_at IS NULL)),
    -- The key is the one the dispatcher derives from what the row points at, nothing else.
    CONSTRAINT webhook_deliveries_shape CHECK (
        (target = 'transfer' AND transfer_id IS NOT NULL AND condition IS NULL AND change IS NULL
         AND dedupe_key = 'log-' || seq::text)
        OR (target = 'condition' AND transfer_id IS NULL
            AND condition IN ('CND-Paused', 'CND-CutOff', 'CND-ReserveShort')
            AND change IN ('held', 'released')
            AND dedupe_key = 'log-' || seq::text || '-' || change || '-' || condition))
);

CREATE INDEX webhook_deliveries_due ON webhook_deliveries (next_attempt_at, id) WHERE state = 'pending';
CREATE INDEX webhook_deliveries_requester ON webhook_deliveries (requester, state, id);

-- Append-only records.
CREATE TRIGGER webhook_secrets_append_only
    BEFORE UPDATE OR DELETE ON webhook_secrets
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5');
CREATE TRIGGER webhook_secrets_no_truncate
    BEFORE TRUNCATE ON webhook_secrets
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5');
CREATE TRIGGER webhook_transfer_urls_append_only
    BEFORE UPDATE OR DELETE ON webhook_transfer_urls
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5, REQ-ID-4');
CREATE TRIGGER webhook_transfer_urls_no_truncate
    BEFORE TRUNCATE ON webhook_transfer_urls
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5, REQ-ID-4');
CREATE TRIGGER webhook_condition_urls_append_only
    BEFORE UPDATE OR DELETE ON webhook_condition_urls
    FOR EACH ROW EXECUTE FUNCTION refuse_append_only_change('REQ-IC-5');
CREATE TRIGGER webhook_condition_urls_no_truncate
    BEFORE TRUNCATE ON webhook_condition_urls
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-IC-5');

-- REQ-ID-4 at the store: a callback URL is set only for a transfer the requester's own request
-- made or reached. Knowing a bridge reference is not enough.
CREATE FUNCTION webhook_transfer_urls_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM intake_answers a JOIN intake i ON i.id = a.intake_id
                   WHERE a.transfer_id = NEW.transfer_id AND i.requester = NEW.requester) THEN
        RAISE EXCEPTION 'callback URL refused: no request of % made or reached transfer % (REQ-ID-4)',
            NEW.requester, NEW.transfer_id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER webhook_transfer_urls_guard
    BEFORE INSERT ON webhook_transfer_urls
    FOR EACH ROW EXECUTE FUNCTION webhook_transfer_urls_guard();

-- The cursor only moves forward, so no entry is fanned out twice by a rewind.
CREATE FUNCTION webhook_cursor_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF TG_OP <> 'UPDATE' OR NEW.after < OLD.after OR NEW.singleton IS DISTINCT FROM OLD.singleton THEN
        RAISE EXCEPTION '% on webhook_cursor refused: the cursor only moves forward (REQ-ST-5)', TG_OP
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER webhook_cursor_guard
    BEFORE INSERT OR UPDATE OR DELETE ON webhook_cursor
    FOR EACH ROW EXECUTE FUNCTION webhook_cursor_guard();
CREATE TRIGGER webhook_cursor_no_truncate
    BEFORE TRUNCATE ON webhook_cursor
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5');

-- A delivery is pending until it ends delivered or dead, and never changes after; what it
-- carries never changes at all; its attempts only grow.
CREATE FUNCTION webhook_deliveries_guard() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = public, pg_temp
    AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'DELETE on webhook_deliveries refused: a delivery is a record (REQ-ST-5)'
            USING ERRCODE = 'BZ001';
    END IF;
    IF OLD.state <> 'pending' THEN
        RAISE EXCEPTION 'delivery %: it ended %, and a finished delivery never changes (REQ-ST-5)',
            OLD.id, OLD.state
            USING ERRCODE = 'BZ001';
    END IF;
    IF ROW(NEW.requester, NEW.seq, NEW.dedupe_key, NEW.target, NEW.created_at)
       IS DISTINCT FROM
       ROW(OLD.requester, OLD.seq, OLD.dedupe_key, OLD.target, OLD.created_at)
       OR NEW.transfer_id IS DISTINCT FROM OLD.transfer_id
       OR NEW.condition IS DISTINCT FROM OLD.condition
       OR NEW.change IS DISTINCT FROM OLD.change
       OR NEW.attempts < OLD.attempts THEN
        RAISE EXCEPTION 'delivery %: what it carries never changes and its attempts only grow (REQ-ST-5)',
            OLD.id
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER webhook_deliveries_guard
    BEFORE UPDATE OR DELETE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION webhook_deliveries_guard();
CREATE TRIGGER webhook_deliveries_no_truncate
    BEFORE TRUNCATE ON webhook_deliveries
    FOR EACH STATEMENT EXECUTE FUNCTION refuse_append_only_change('REQ-ST-5');

-- M2 (PR #55 review): only the dispatcher's role writes a delivery or a condition hold, whatever
-- is granted. The table's owner and a superuser keep writing everything, for drills and restores
-- (NFR-7). Asked of the session's login (session_user), as 0036 does: a SECURITY DEFINER function
-- owned by the migration owner runs as the owner (current_user), so asking current_user would
-- exempt every caller of such a function.
CREATE FUNCTION webhook_dispatcher_only() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT (pg_has_role(session_user, 'bezent_webhooks', 'MEMBER')
            OR pg_has_role(session_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')
            OR (SELECT rolsuper FROM pg_roles WHERE rolname = session_user)) THEN
        RAISE EXCEPTION '% on % refused: only the webhook dispatcher writes it (REQ-ST-5; %)',
            TG_OP, TG_TABLE_NAME, session_user
            USING ERRCODE = 'BZ001';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER webhook_deliveries_dispatcher_only
    BEFORE INSERT OR UPDATE ON webhook_deliveries
    FOR EACH ROW EXECUTE FUNCTION webhook_dispatcher_only();
CREATE TRIGGER webhook_condition_holds_dispatcher_only
    BEFORE INSERT OR UPDATE OR DELETE ON webhook_condition_holds
    FOR EACH ROW EXECUTE FUNCTION webhook_dispatcher_only();

-- S-c (PR #55 re-check): only the ingress API registers a URL or makes a secret, on behalf of the
-- requester. The core's and the endpoints' logins hold bezent_service too, and a garbage row from
-- one of them would become the "latest" and silently stop a requester's webhooks. Granted to
-- bezent_ingress alone, and refused here to any other login, whatever is granted. The owner and a
-- superuser are exempt (drills, restores; NFR-7). Asked of session_user, as above.
CREATE FUNCTION webhook_ingress_only() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path = pg_catalog, public, pg_temp
    AS $$
BEGIN
    IF NOT (pg_has_role(session_user, 'bezent_ingress', 'MEMBER')
            OR pg_has_role(session_user, (SELECT relowner FROM pg_class WHERE oid = TG_RELID), 'MEMBER')
            OR (SELECT rolsuper FROM pg_roles WHERE rolname = session_user)) THEN
        RAISE EXCEPTION '% on % refused: only the ingress API registers it for a requester (REQ-ID-4; %)',
            TG_OP, TG_TABLE_NAME, session_user
            USING ERRCODE = 'BZ001';
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER webhook_secrets_ingress_only
    BEFORE INSERT ON webhook_secrets
    FOR EACH ROW EXECUTE FUNCTION webhook_ingress_only();
CREATE TRIGGER webhook_transfer_urls_ingress_only
    BEFORE INSERT ON webhook_transfer_urls
    FOR EACH ROW EXECUTE FUNCTION webhook_ingress_only();
CREATE TRIGGER webhook_condition_urls_ingress_only
    BEFORE INSERT ON webhook_condition_urls
    FOR EACH ROW EXECUTE FUNCTION webhook_ingress_only();

-- Grants. No function here is PUBLIC's; trigger functions fire without an EXECUTE grant.
REVOKE EXECUTE ON FUNCTION webhook_transfer_urls_guard() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION webhook_cursor_guard() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION webhook_deliveries_guard() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION webhook_dispatcher_only() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION webhook_ingress_only() FROM PUBLIC;

-- The ingress (bezent_ingress, which inherits bezent_service): registers URLs and makes secrets,
-- alone. bezent_service reads back what it needs to answer (never a sealed column), and lists its
-- requesters' deliveries.
GRANT INSERT ON webhook_secrets, webhook_transfer_urls, webhook_condition_urls TO bezent_ingress;
GRANT SELECT (id, requester, created_at) ON webhook_secrets TO bezent_service;
GRANT SELECT (id, requester, transfer_id, url_digest, previous_digest, after_seq, registered_at)
    ON webhook_transfer_urls TO bezent_service;
GRANT SELECT (id, requester, registered_at) ON webhook_condition_urls TO bezent_service;
GRANT SELECT ON webhook_deliveries TO bezent_service;

-- The dispatcher (bezent_webhooks): reads the log and what it needs to address and build a
-- notification, opens secrets and URLs, and alone writes the cursor, the holds and the outbox.
GRANT USAGE ON SCHEMA public TO bezent_webhooks;
GRANT SELECT ON event_log, event_log_head, transfers, intake, intake_answers, requester_grants
    TO bezent_webhooks;
GRANT SELECT ON webhook_secrets, webhook_transfer_urls, webhook_condition_urls TO bezent_webhooks;
GRANT SELECT, UPDATE (after) ON webhook_cursor TO bezent_webhooks;
GRANT SELECT, INSERT, DELETE ON webhook_condition_holds TO bezent_webhooks;
GRANT SELECT, INSERT ON webhook_deliveries TO bezent_webhooks;
GRANT UPDATE (state, attempts, next_attempt_at, leased_until, last_status, last_error, finished_at)
    ON webhook_deliveries TO bezent_webhooks;

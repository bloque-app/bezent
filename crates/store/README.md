<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# bezent-store: logins and grants

bezent's PostgreSQL schema lives in `migrations/` (applied by the migration owner, who owns every table,
trigger and function). This page is about who else may do what in it: the roles, the logins that hold
them, and the grant matrix. Issues #35, #33, #46 and #27; migrations `0045_owner_grants` and `0046_writer_guards` (the review of
PR #56).

## One login per service, one role per login

Every service logs in to the store as a login of its own. That login is a member of exactly one role,
its service's, and of nothing else, held `WITH INHERIT TRUE, SET FALSE` and without `ADMIN OPTION`. It
acts as itself for its whole life, through the privileges it inherits from the role; it cannot run
`SET ROLE` (#27), and it cannot grant its role to anyone. No role owns anything, so none can alter, drop
or disable a trigger, and none may create in the schema or the database.

| Service | Its login is a member of | Checked at start-up by |
|---|---|---|
| core | `bezent_core` | `bezent-core` |
| ingress API | `bezent_ingress` | `bezent-ingress` (`bezent_ingress::connect`) |
| webhook dispatcher | `bezent_webhooks` | `bezent-ingress`, for `[webhooks] database_url` |
| Kreivo endpoint | `bezent_kreivo_endpoint` | `bezent-kreivo-endpoint` |
| EVM endpoint | `bezent_evm_endpoint` | `bezent-evm-endpoint` |
| signer | `bezent_signer` | `bezent-signer` |
| each operator | `bezent_operator` | (the operator surface, F-014) |

At start-up, before it serves, each service runs the same check (`bezent_store::login::check_login`)
and refuses to start, saying why, unless:

- the server is PostgreSQL 16 or later (before 16 every member of a role may `SET ROLE` into it);
- the connection acts as its login (`current_user = session_user`);
- the login is not a role of the store's own, is no superuser, and cannot create roles or databases,
  replicate or bypass row security; nor can any role it reaches;
- the login holds no role `WITH ADMIN OPTION`, directly or through a role it reaches (with it, the
  login could grant its role to any other, the ingress's included, and every running service would
  gain the role's privileges at once);
- the login is a member of its service's role and of no other role, without SET: it can `SET ROLE`
  into no role at all;
- neither the login nor anything it reaches owns an object of the store;
- the store's migrations are exactly the ones the binary was built with, versions and checksums
  (`bezent_applied_migrations()`): a store behind, ahead, or migrated from other files is refused;
- the login's privileges, inherited ones included, are exactly its role's rows of the matrix below. A
  grant widened by hand (to the login or to the role) is refused.

Two older roles stay, holding nothing:

- `bezent_service`, which every service but the signer used to log in as a member of, is **retired**:
  it holds no privilege in a migrated store, and no login should be a member of it (the check refuses
  one that is). `bezent_ingress` keeps a membership of it, without SET (roles are shared by every
  database on a server, and an earlier migration of another database would grant it again).
- `bezent_endpoint` is the endpoints' **mark**: `bezent_kreivo_endpoint` and `bezent_evm_endpoint`
  hold it without SET, and the store's guards look for it ("an endpoint wrote this"). It holds no
  privilege; each endpoint's role holds its own.

## Every pool is one login's

A service makes its pool with `bezent_store::login::pool_options()` (the signer with its own
`pool_options`, which wraps it). A connection that comes back to the pool acting as another role than
its login is closed, never handed out again, and counted (`role_leaks`). That is the cause of the
flaky test of #27: a test ran `SET ROLE bezent_signer` on a connection of its owner pool and gave it
back still acting as the signer, and a later query of the same test drew it. Tests switch roles only
with `SET LOCAL ROLE` inside a transaction, and the test `pool_roles` fails on any session-level role
switch in the sources.

## The grant matrix

`src/grants.rs` (`MATRIX`) is the one statement of it; the store test `grant_matrix` asserts that a
migrated store grants exactly this, so a migration that widens any grant fails CI. The test reads the
catalogue, not a list: every table, view, sequence, function and schema of every schema but the
server's own, and the database, so an object added later is covered without a change to the test. A
grant to any grantee the matrix does not name (a login, a `pg_*` role, a new role), any PUBLIC grant
beyond those below, and any default privilege (`ALTER DEFAULT PRIVILEGES`) fails it. Every role also has
`USAGE` on the schema; PUBLIC holds nothing on any table and may call only three helpers that run with
the caller's own privileges (`is_terminal_state`, `route_usage_locked`,
`transfer_edge_needs_coordinate`). A privilege on some columns only is written `table(columns)`.

Who **writes** each record (INSERT, unless said otherwise; nothing is granted UPDATE but where shown,
and no role may DELETE or TRUNCATE anything but the dispatcher's own condition holds):

| Record | Written by |
|---|---|
| `intake`, `intake_answers`, `relayed_instructions` | ingress |
| `transfers` | ingress (requests: a bound deposit's destination on the issuing network and its refund destination on its arrival's, an unassociated arrival's refund on the arrival's network, 0049), core (withdrawals and refunds of them) |
| `transfers` columns, UPDATE | ingress: `expires_at`; core: `on_chain_reference` |
| `transitions` | ingress: only a request's decision, `Received` to `Accepted` or `Refused`, of a deposit or of the refund of an unassociated arrival, with no coordinate; core: every other edge, the ones that rest on chain data (the store refuses any other writer, 0046); each under the id the store's sequence drew for the session, never one a login chooses (0049) |
| `event_log` | each kind by the role whose events it records (0046): `transfer`, `transition` by the store's own triggers; `refusal` by the ingress; `key-compromise` by the role that detects it (the core, an endpoint on a network it covers, the signer), stamped by the store with its detector and detection key, one per pair (0049); `pause-requested` by the endpoints, the core or the signer; `delivery-anomaly` by the endpoints or the core; every other kind (conditions, reviews, the reserve check's pages) by the core |
| `intents` | core: a delivery order only on a final, executed observation of its transfer's source, for exactly the amount it carries (0049), to the deposit's destination and in the transfer's own asset and token (0050) |
| `unsigned_attempt_cancellations` | core: an attempt ordered and never signed, cancelled under an approved review resolution into `Refunding` (0051) |
| `book_entries`, `book_postings`, `book_quarantine` | core |
| `reserve_evaluations`, `reserve_statements`, `reserve_statement_abandoned` | core |
| `pauses(asset, cause, evaluation_id, quarantine_id, reading_id)` | core |
| `observations`, `observation_outcomes`, `observation_ranges`, `final_heads` | Kreivo endpoint, EVM endpoint: each its own networks only, the Kreivo endpoint the issuing network, the EVM endpoint the EVM origin networks (the guard of 0049 on every chain table, by the session's login and the role it acts as; 0034 and 0035 before it) |
| `prepared_transactions`, `signed_transactions`, `non_execution_proofs` | Kreivo endpoint, EVM endpoint: each its own networks only, a preparation counting as its order's network and a signed transaction as its intent's (0049) |
| `reserve_readings` | Kreivo endpoint, EVM endpoint: each its own networks only (0049) |
| `signatures` | signer (with the nonce and the asset it decoded from the bytes it signed, 0049) |
| `webhook_secrets`, `webhook_transfer_urls`, `webhook_condition_urls` | ingress |
| `webhook_deliveries`, `webhook_condition_holds` (and DELETE), `webhook_cursor(after)` UPDATE, `webhook_deliveries(state, attempts, next_attempt_at, leased_until, last_status, last_error, finished_at)` UPDATE | webhook dispatcher |
| `manual_actions(kind, transfer_id, to_state, network, block_number, block_hash, block_time, justification)`, `manual_approvals(action_id, justification)`, `ledger_accounts(network, address, kind, reason)` | operators |
| `networks`, `assets`, `admitted_tokens`, `reserves`, `routes`, `requesters`, `requester_grants`, `requester_credentials`, `surplus_funding_addresses`, `endpoint_scopes` | nobody: the migration owner's declarations |

Who **reads** what (SELECT):

| Role | Reads |
|---|---|
| core | every record but `requester_credentials`, `requesters`, `requester_grants` and the webhooks' tables |
| ingress | the declarations (`networks`, `assets`, `admitted_tokens`, `reserves`, `routes`, `pauses`, `requesters`, `requester_grants`, `requester_credentials`, `surplus_funding_addresses`, `ledger_accounts_listed`), the requests (`intake`, `intake_answers`, `relayed_instructions`), `transfers`, `transitions`, `attempt_milestones`, `observations`, `observation_outcomes`, `event_log`, `event_log_head`, and the views `transfer_status`, `event_log_position`, `book_balances`; of the webhooks' tables, `webhook_deliveries` and the unsealed columns of the secrets and URLs. **No** intent, signature, signed bytes, proof, reading, book entry or posting, or reserve record (the `book_balances` view gives it the balances only) |
| Kreivo endpoint, EVM endpoint | `networks`, `assets`, `admitted_tokens`, `reserves`, `endpoint_scopes`, `intents`, `signatures`, their own records (`observations`, `observation_outcomes`, `observation_ranges`, `observation_gaps`, `observation_watermarks`, `final_heads`, `prepared_transactions`, `signed_transactions`, `non_execution_proofs`, `reserve_readings`) and `event_log`. **No** transfer, request or book |
| webhook dispatcher | `event_log`, `event_log_head`, `transfers`, `intake`, `intake_answers`, `requester_grants`, and the webhooks' tables, sealed columns included |
| signer | `intents` (the signing intent and the core's order), `prepared_transactions`, `signatures`, and since 0049 `admitted_tokens` and `signed_transactions(id, intent_id)` (to compare the bytes it decodes with the core's order: the token an asset is paid in, a cancel's target) and `event_log(seq)` (the position of an alert it appends); nothing else |
| operators | `manual_actions`, `manual_approvals`, `reviews`, `unassociated_arrivals`, `ledger_accounts`, `ledger_accounts_listed`, `book_quarantine`, `surplus_funding_addresses`, `reserve_evaluations`, `reserve_statements`, `reserve_statement_abandoned`, `reserve_readings` |
| `bezent_service`, `bezent_endpoint` | nothing |

Who may **call** what (EXECUTE; the functions that run as the owner are marked *definer*):

| Role | Calls |
|---|---|
| core | `attempt_proof` *definer*, `attempts_unproven` *definer*, `pause_reading_is_supply_mismatch` *definer*, `source_bindable`, `funding_arrival`, `one_business_day_after`, `bezent_writer_holds` |
| ingress | `source_bindable`, `funding_arrival`, `bezent_writer_holds` |
| Kreivo endpoint, EVM endpoint | `bezent_writer_holds`, `bezent_endpoint_writer`, `bezent_evm_endpoint_writer`, `bezent_row_network`, `observation_kind_on_network`, `observation_kind_for_event`, `observation_writer_covers` (the guards' helpers, which run as the inserting login) |
| signer | `attempt_gate_open` *definer*, `statement_place_refusal` *definer* |
| operators | `source_bindable`, `funding_arrival`, `one_business_day_after` |

Every service and the operators also call `bezent_applied_migrations()` *definer*, which lists the
migrations applied to the store, for the start-up check. The trigger functions that run as the owner
(`event_log_assign_seq`, `transfers_log_created`, `transitions_apply`, `transfers_source_verified_final`)
fire without an EXECUTE grant and are nobody's to call.

## What the store refuses whoever writes it

Grants say who may write a table; the guards say what. Two of them hold guardrails 2 and 3 (`INV-6`,
`REQ-RQ-7`) whatever login writes (0046):

- A transfer reaches `Source-Final` (by a transition, a review's exit, or an insert at Source-Final for
  a bound deposit) only when the observation at its source coordinate is final: its outcome is `final`
  (so not `withdrawn`), decided at a recorded final head at or above the observed block with the same
  hash, and no recorded final head at the observed height has another hash than the one observed.
- Only the core records a transition that rests on chain data. The ingress records a request's decision
  out of `Received`, and nothing else.

The audit of PR #56 added these (0049), for every login but the owner:

- Each endpoint's login writes only its own networks' chain records (observations, outcomes, ranges,
  final heads, readings, prepared and signed transactions, proofs), checked by the session's login.
- A delivery order rests on a final, executed observation of its transfer's source and orders exactly
  what it carries: a mint, the arrival's amount scaled to the asset (rounded down), of the asset's
  issuing asset, on the issuing network; a payout of a withdrawal, the redemption's amount scaled to the
  destination's token, to its destination; a refund mint, the redemption's amount to its redeemer; a
  payout of an unassociated arrival's refund, the arrival's amount on its network.
  Since 0050 (issue #36) the order's other fields are tied too: a mint's `to` is the 32-byte account the
  deposit's destination decodes to (`0x` and 64 hex digits, or SS58); a payout's `asset` is its transfer's
  and its `token`, when it names one, the token admitted for that asset on the destination network (an
  unassociated arrival's refund: the arrival's own token, or, its token not being admitted, that token and
  no asset).
  Since 0050 too, a network admits one token per asset (a unique index on `admitted_tokens (network,
  asset)`): the payout's scaling and its token check read the same row.
- A transition's id, like a transfer's, is the one the store's sequence drew for the session.
- A `key-compromise` alert carries the detector and detection key the store stamps on it; a detector
  looks only for its own, so a row another role wrote never suppresses it.
- A bound deposit's destination is on the issuing network and its refund destination on its arrival's
  network; an unassociated arrival's refund pays out on the arrival's network; neither is accepted
  otherwise.

An attempt that was ordered and never signed (issue #25, 0051) stood in the way of a withdrawal's refund for
good: it has an intent and, there being no signed transaction, no proof of non-execution to record. The core
cancels it, once, under an approved review resolution into `Refunding` of that transfer; the store accepts
the cancellation only when the attempt is the transfer's next one, ordered, its predecessor proven not
executed at finality, and nothing was signed for it (no signature, no signed bytes). From then on it records
no intent, signature or signed bytes for that attempt, and `attempts_unproven()` does not list it.

## Operators: one login each (REQ-GV-5)

Every operator logs in as a login of their own, a member of `bezent_operator`; **a login is never
shared**. The store stamps the session's login as a manual action's actor and as an approval's approver
(`manual_actions.actor`, `manual_approvals.approver`, migration 0015), and refuses an approval by the
action's own login. A login two people share could therefore act and approve as one person, or approve
nobody's action at all: value that needs a second approver (REQ-GV-5) would never move. An operator who
leaves has their login dropped (`DROP ROLE`); their recorded actions keep their name.

## Connecting over TLS

Every service connects through `bezent_store::tls::connect`, which builds the pool with rustls (no OpenSSL) and
reads the policy from the database URL it is given (the URL in the service's configuration or secret file):

```
postgres://<login>@<host>:5432/<database>?sslmode=verify-full&sslrootcert=/etc/bezent/rds-ca.pem
```

- `sslmode=disable | require | verify-ca | verify-full`, as in libpq. `require` fails unless the server offers TLS,
  and encrypts, but does not verify the certificate (unlike libpq, even with `sslrootcert`); only `verify-ca` and
  `verify-full` do. `verify-full` checks the certificate chain against `sslrootcert` (a PEM bundle, added to the bundled web roots;
  mount it, it is not in the image) and the host name. Production uses `verify-full`.
- **No silent downgrade.** `allow` and `prefer` fall back to a plain connection, so they are refused, and so is a URL
  with no `sslmode`, for any server that is not on this machine (a loopback address or a Unix socket). A server
  that does not offer TLS, or whose certificate does not verify, stops the service from starting.
- The error never repeats the URL, which holds the password.

`crates/store/tests/tls.rs` checks this against a server that forces TLS with a throwaway CA and one that offers none
(the `database TLS` job of `ci.yml`; locally, `tools/ci/tls-postgres.sh <dir>` starts the first and prints the
variables the test reads).

## Deploying

The order matters: running `0045` empties `bezent_service` at once, so services of the previous release
stop working as soon as it runs. As the migration owner, on the release's store:

1. **Stop** every service (core, ingress and its webhook dispatcher, both endpoints, signer).
2. **Migrate** the store to the release (`0045`, `0046`, …). The services check the store's whole
   migration set at start-up and refuse a store that is behind.
3. **Bring every login to its role alone**: run the block below once per service login, with the
   login's name and its role. It is safe on a login made for an earlier release, and on a new one.
4. **Make each operator's login** (one per person, below) and bring it to `bezent_operator` the same way.
5. **Check** every login (the query at the end) and **start** the services. Each runs its own check
   before it serves and refuses to start, saying why, if anything is left.

Create a login that does not exist yet with its own password (kept in the service's secret file, never
in its config); the name is the deployment's choice:

```sql
CREATE ROLE bezent_core_prod LOGIN PASSWORD '…';
```

Then bring it to its role alone. The block revokes every membership the login holds **whoever granted
it** (a plain `REVOKE bezent_service FROM x` only removes the grants the current role made, leaves any
other with a WARNING, and the service then refuses to start), and `CASCADE` takes away whatever the
login granted on with an `ADMIN OPTION`. It makes the login `INHERIT` (a signer login made before
`0045` was `NOINHERIT`, because it ran `SET ROLE`; it would now hold no privilege and be refused),
revokes any privilege granted to the login itself, and grants the role `WITH INHERIT TRUE, SET FALSE`,
without `ADMIN OPTION`:

```sql
-- deploy: bring a login to its role alone (run as the migration owner; as a superuser where an
-- earlier membership was granted by one)
DO $$
DECLARE
    login TEXT := 'bezent_core_prod';   -- the service's login
    svc   TEXT := 'bezent_core';        -- its role: bezent_core, bezent_ingress, bezent_webhooks,
                                        -- bezent_kreivo_endpoint, bezent_evm_endpoint, bezent_signer
                                        -- or, for an operator, bezent_operator
    m RECORD;
BEGIN
    EXECUTE format('ALTER ROLE %I LOGIN INHERIT', login);
    FOR m IN SELECT g.rolname AS grp, gr.rolname AS grantor FROM pg_auth_members a
             JOIN pg_roles g ON g.oid = a.roleid
             JOIN pg_roles u ON u.oid = a.member
             JOIN pg_roles gr ON gr.oid = a.grantor
             WHERE u.rolname = login LOOP
        EXECUTE format('REVOKE %I FROM %I GRANTED BY %I CASCADE', m.grp, login, m.grantor);
    END LOOP;
    EXECUTE format('REVOKE ALL ON ALL TABLES IN SCHEMA public FROM %I', login);
    EXECUTE format('REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM %I', login);
    EXECUTE format('REVOKE ALL ON ALL FUNCTIONS IN SCHEMA public FROM %I', login);
    EXECUTE format('REVOKE ALL ON SCHEMA public FROM %I', login);
    EXECUTE format('REVOKE ALL ON DATABASE %I FROM %I', current_database(), login);
    EXECUTE format('GRANT %I TO %I WITH INHERIT TRUE, SET FALSE', svc, login);
END
$$;
```

A login must not be a superuser, nor able to create roles or databases, replicate or bypass row
security (`ALTER ROLE … NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS`, as a superuser),
and must not own the database or any object in it (the check refuses a login that is the
database's `datdba`): if it does, `ALTER DATABASE … OWNER TO` the migration owner first, and
`REASSIGN OWNED BY` the login `TO` the migration owner.

The sidecar (`services/sidecar`) and the tests make their logins the same way
(`bezent_store::login::ensure_login`).

### Operators: one login each

Each operator gets a login of their own, never shared, made with the block above and `svc` set to
`bezent_operator`:

```sql
CREATE ROLE ops_alice LOGIN PASSWORD '…';
-- then the block above, with login := 'ops_alice' and svc := 'bezent_operator'
```

No service checks an operator's login at start-up (the operator surface, F-014, will). Until then,
check them with the query below.

### Check every login

Every login that is a member of a store role must hold exactly one membership, of one store role, with
INHERIT, without SET or ADMIN, have none of the dangerous attributes, and own nothing in the database.
This lists every login that breaks any of that, and why; it must return no rows:

```sql
-- deploy: check every login of a store role
WITH store_roles(name) AS (VALUES ('bezent_service'), ('bezent_core'), ('bezent_ingress'),
         ('bezent_endpoint'), ('bezent_kreivo_endpoint'), ('bezent_evm_endpoint'),
         ('bezent_webhooks'), ('bezent_signer'), ('bezent_operator')),
logins AS (
    SELECT DISTINCT u.oid, u.rolname FROM pg_auth_members a
    JOIN pg_roles g ON g.oid = a.roleid JOIN pg_roles u ON u.oid = a.member
    WHERE g.rolname IN (SELECT name FROM store_roles) AND u.rolcanlogin
)
SELECT l.rolname, problem FROM logins l, LATERAL (
    SELECT 'is a member of ' || string_agg(g.rolname::text, ', ') AS problem
    FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid
    WHERE a.member = l.oid HAVING count(*) <> 1
    UNION ALL
    SELECT 'holds ' || g.rolname::text || ' with SET, with ADMIN or without INHERIT'
    FROM pg_auth_members a JOIN pg_roles g ON g.oid = a.roleid
    WHERE a.member = l.oid AND (a.set_option OR a.admin_option OR NOT a.inherit_option)
    UNION ALL
    SELECT 'is a superuser, or may create roles or databases, replicate or bypass row security'
    FROM pg_roles r WHERE r.oid = l.oid
      AND (r.rolsuper OR r.rolcreaterole OR r.rolcreatedb OR r.rolreplication OR r.rolbypassrls)
    UNION ALL
    SELECT 'owns objects of this database'
    WHERE EXISTS (SELECT 1 FROM pg_class WHERE relowner = l.oid)
       OR EXISTS (SELECT 1 FROM pg_proc WHERE proowner = l.oid)
       OR EXISTS (SELECT 1 FROM pg_namespace WHERE nspowner = l.oid)
       OR EXISTS (SELECT 1 FROM pg_database WHERE datname = current_database() AND datdba = l.oid)
) p
ORDER BY 1, 2;
```

The store test `s5_the_readme_brings_an_old_login_to_its_role_alone` runs both blocks of this section
as written, on a login made the way an earlier release made them.

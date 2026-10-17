<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-002 — notes

## M-0 part (T-002-01 to T-002-06)

`services/ingress` serves the request and query surfaces over HTTP and JSON on the store, as the `bezent_service`
role. Migration `0010_requester_surface` adds the declarations it reads (networks, assets, admitted tokens, reserves,
routes, pauses, requesters, grants, hashed credentials) and the records it appends (`intake_answers`,
`relayed_instructions`). No merged migration changed; the state graph of `0003` is untouched (amendment 0009 pending).
The OpenAPI document is `services/ingress/openapi.json`; a test and the CI fail when it is stale.

**Delivery guarantee.** Delivery is at-least-once within a declared retry bound (`max_attempts`, 12 by default, about
70 minutes), then the delivery is dead-lettered and stays visible at `GET /v0/webhooks/deliveries?state=dead` (and in
the error log). It is not retried forever and it is not redelivered. A webhook is a hint: the sequenced log is the
source of truth (`REQ-ST-4`, `REQ-ST-5`), and no requester may need a webhook to arrive. A requester that missed one
(dead letter, outage, replaced URL) reads the log from its last position.

### Choices made here, open to review

1. **A deposit leaves the ingress in `Accepted`, never `Source-Final`.** The ingress does not decide finality
   (`REQ-AR-2`); the core moves `Accepted → Source-Final` when the arrival is final. Amendment 0009 (proposed) says a
   deposit bound to a final arrival is *created* at `Source-Final`; if it is applied, the core or the ingress needs a
   finality read it does not have today.
2. **Two shapes of refusal.** Once the arrival's asset and amount are known, a refused request is a transfer in
   `Refused` (§5.4), bound to nothing, with bridge reference `bz-r-…` derived from (requester, request reference).
   Before that (`ERR-SourceNotFound`; `ERR-UnknownRoute` for a token not admitted; a withdrawal's extras that reach
   nothing or reach a withdrawal already extended; any refusal of a refund that names no asset, choice 7) there is no
   transfer to make (a transfer names a bridged asset, or is a bound refund of a token, `0011`), so the refusal is an
   `intake_answers` row and a `refusal` entry in the sequenced log.
3. **`ERR-SourceNotFound`** covers: nothing observed at the coordinate, not executed, withdrawn (`REQ-IN-7`), or not
   at a reserve address. The amount and token are always the observation's (`REQ-RQ-7`).
4. **Capacity** is the number of a route's transfers not yet terminal. At capacity, `ERR-Overloaded`, nothing
   recorded, so the same request can be retried (`REQ-LM-4`). The intake also refuses with `ERR-Overloaded` when its
   permits (`max_in_flight`) are taken or the store does not give a connection within `acquire_timeout_ms`.
5. **Window limit** is the trailing `window_seconds`: the sum of the route's transfers that did not end with nothing
   moved (not `Refused`, `Expired` or `Withdrawn`).
5a. **The route lock** (review S2). Window and capacity are exact only if every decision that reads them is
   serialised per route through the commit; that part is inherent. What is decided under the lock is now only what
   needs it: `route_usage_locked` (`0011`) takes the lock and reads the window used and the transfers in flight with
   a snapshot taken after it, each from an index bounded by what it counts (`transfers_route_window`,
   `transfers_route_in_flight`), not by the route's history. Then the insert (`ON CONFLICT DO NOTHING`, no savepoint),
   then the answer and the transition in one statement, then the commit: three round trips and the fsync, down from
   eight. The arrival, the asset, the reserve, the destinations, the route's declared limits, the pause, the
   coordinate's binding and the per-transfer limits are all read before the lock; a binding that commits in between
   is caught by the store's unique index and refused with `ERR-SourceConsumed`.
6. **Pauses** are rows with wildcard columns (asset, network, direction; all empty is global), checked on deposits.
   Who writes them, with actor and justification (`REQ-GV-5`), is the operator surface's (`F-014`).
7. **A refund of an unassociated arrival** applies no route limits and no pause (it releases value a route may have
   refused) and waits in `Received` for the operator's second approver (`REQ-IN-6`). It needs no bridged asset
   (amendment 0010, "Not a gap"): an arrival at a reserve of a token that is not admitted is refunded as a transfer
   that names the token instead (`transfers.token`, `0011`; the amount is then in the token's minor units on the
   source network), and its status carries `token` in place of `asset`. Such a refund exists only bound to its
   arrival; a refusal of one is an answer alone (choice 2).
8. **Destinations:** a deposit's on the issuing network; its refund destination on the arrival's network
   (`REQ-OT-4`); an unassociated arrival's refund on its own network; a withdrawal's refund destination on the issuing
   network (`REQ-RF-4`). Formats: `evm`, `ss58` (checksum verified), `opaque`.
9. **A withdrawal's extras** are added once per withdrawal, by the first request that reaches it with the
   `withdrawal` grant; a second is `ERR-SourceConsumed`. That request links the withdrawal to its requester, which is
   what lets the requester read it. The withdrawal's row is locked first and what the extras are checked against is
   read by a new statement once the lock is held (review M1: under READ COMMITTED a statement that waited for a row
   lock keeps its earlier snapshot, so two requesters could both extend one withdrawal and the second could read the
   first's refund destination). The store backs it: a transfer is answered to at most one request
   (`intake_answers_transfer_once`, `0011`). Amendment 0010 A1 (proposed) would make the second
   `ERR-ReferenceConflict` and route attested extras through the `attest` grant; not applied until ruled.
10. **The commitment (`REQ-RQ-9`)** is read from the redemption's observation, `detail.commitment` (hex), and compared
    with BLAKE2b-256 of the relayed bytes (`DEC-14`). The Kreivo endpoint (`F-007`) must record it there.
11. **Reads are scoped to the requester** (`REQ-ID-4`): a transfer is visible to a requester whose request made or
    reached it. The log keeps global, gapless positions and returns the requester's entries in a range; `next_after`
    carries the cursor. Entries a withdrawal gathered before its requester attached extras are behind that cursor;
    status shows them, the log does not.
12. **Credentials** are `<id>.<secret>`; the store keeps SHA-256 of the secret. Provisioning is the operator's
    (`F-014`); tests insert them as the owner.
13. `ERR-ExcessPrecision` is never raised by the ingress: no request carries an amount.
14. F-001 note N8 is handled at this boundary: block numbers above `i64::MAX` and empty hashes are refused as malformed.
15. **After the commit nothing answers "nothing recorded"** (review S1, `REQ-RQ-1`). A store error on `COMMIT` means
    it did not commit (`internal`); a lost connection during it leaves the outcome unknown (`outcome-unknown`, 503:
    repeat the same request, which answers with the original). Once committed, a failed read of the status answers
    `taken-in` (202: read it by request reference).
16. **An expiry that has already passed** is refused as malformed and not recorded (review S4, `REQ-LM-2`); checked for
    a new request only, so a repeat still answers with the original (`REQ-ID-1`).
17. **EVM destinations** written in mixed case must carry a valid EIP-55 checksum (review S5); all-lower and all-upper
    carry none and are taken as they are.

### Spec gaps found (each needs a ruling before it is relied on)

- **G1 — who may add a withdrawal's extras.** `REQ-RQ-4` lets "the requester" add them by the redemption's reference,
  but a redemption is anyone's and its reference is public, and `REQ-ID-4` says knowing a reference is not authorising.
  Nothing says which requester owns a redemption. Built: first requester with the grant wins (choice 9). Amendment
  0008 (redeemer-scoped references) does not settle it either.
- **G2 — cut-offs.** `REQ-LM-1` declares them and `REQ-LM-2` refuses what will not execute, but §10 names no error for
  a request past a cut-off and the spec does not define one. They are published in the route table, not enforced.
- **G3** (refund of a token not admitted) was not a gap (amendment 0010, "Not a gap"); built, choice 7.
- **G4 — errors with no name.** A malformed request (including one carrying a claimed outcome, `REQ-RQ-7`) and relayed
  instructions that do not match the commitment (`REQ-RQ-9`) have no `ERR-*` in §10. Used: `malformed` (not an
  `ERR-*`) and `ERR-SourceMismatch`. An unauthenticated caller gets `ERR-NotAuthorised` with HTTP 401.

### Not built (out of scope here)

Webhooks (`REQ-ST-5`, `REQ-IC-5`; M-1), moving transfers past `Received`/`Refused`/`Accepted` (`F-003`), verifying
coordinates against chains (endpoints), the operator surface (`F-014`).

## Review of PR #22: what was fixed and what is left

Fixed in this PR: M1 (choice 9), M2 (choice 7, G3), S1 (choice 15), S2 (choice 5a), S4 (choice 16), S5 (choice 17), the
window total (choice 5), and the store test whose `TRUNCATE intake CASCADE` check now names intake's own trigger.

**Intake latency after S2** (`intake_latency`, debug build, PostgreSQL 14 on the arm64 laptop; noisy, so the median of
five runs, and two runs of the code before the fix on the same store for comparison). Sequential p99 4.6 ms (before:
5.8 to 6.0 ms). 8 concurrent requesters on one route: p50 8.5 ms (before: 13.5 to 14.1 ms), p99 22 ms (runs ranged
19.6 to 73 ms; before: 36.6 to 40.4 ms). All within `NFR-1`'s 250 ms; the gate belongs on x86/CI.

Left for later, each to its own issue or ruling:

- **S3, contingent on amendment 0010.** A1: attested extras need the `attest` grant (no `Attest` variant in `auth.rs`
  yet), and a later different body becomes `ERR-ReferenceConflict` instead of `ERR-SourceConsumed`. A3: rename
  `malformed` to `ERR-Malformed` and `ERR-SourceMismatch` (relayed instructions) to `ERR-CommitmentMismatch`.
- **The global log head** (not changed here). The head row is locked from a transaction's first append (the transfer
  insert trigger) through its commit's fsync, so every writer of the log is serialised. That comes from `0007`
  (gapless positions assigned inside the writer's transaction), not from the requirement. The reviewer's alternative:
  append with an identity id and no position, and let a single sequencer assign gapless positions after commit;
  readers read only sequenced rows. Gapless and stable reads hold. Not needed for M-0 with one requester; decide it
  before `NFR-1` is gated.
- **REQ-ID-4 against A1.** Even with M1 fixed, any requester with the `withdrawal` grant can name the refund destination
  of any public redemption (the first wins), and any requester with the `refund` grant can claim any unbound arrival;
  the operator's second approver is the only guard on the latter. For the A1 ruling.
- **Log reader caveats.** A withdrawal's entries from before its extras were linked never reach its requester's log
  read (choice 11; `REQ-ST-4` says "everything since"). Entries with no transfer other than `refusal` are visible to
  nobody, which will matter for `REQ-IC-5` conditions. Pages count positions, not matching entries, so a requester
  with sparse entries needs several calls to catch up.
- **Pool sharing.** GET routes and the authentication lookup take no intake permit and share the pool with intake; a
  burst of reads can push intake into `ERR-Overloaded` through pool timeouts.
- **Credentials** have no `revoked_at` or expiry (`F-014`, before a second requester exists).
- **Digest of EVM addresses.** An EVM address that differs only in case gives a different body digest, so such a
  repeat is refused as a conflict rather than replayed.
- **`REQ-RQ-6` "unconsumed funding"** refunds are not built; only unassociated arrivals are.

## Follow-up, issue #30 (SPEC v0.5.1: amendments 0010 and 0011)

Migration `0034_ingress_0010` (additive). What changed, and what supersedes the choices above:

- **Choice 9 and S3 are applied.** Extras are an expiry and, for an attested backend, the relayed instructions;
  there is no refund destination any more (`0011`: a withdrawal's refund goes to its redeemer), and a request
  that carries one is `ERR-Malformed`. Extras are written once: a later request, by any requester, that reaches a
  withdrawal that already has extras is refused with `ERR-ReferenceConflict` (it was `ERR-SourceConsumed`). Extras
  of an attested backend need the `attest` grant on top of `withdrawal` (`ERR-NotAuthorised`, nothing recorded).
  Extras to a finished withdrawal stay `ERR-SourceConsumed`.
- **Named errors (G4).** `malformed` is `ERR-Malformed` (HTTP 400, nothing recorded); relayed instructions that miss
  the commitment are `ERR-CommitmentMismatch` (a recorded refusal). Both joined the `refusal_error` domain.
- **Cut-offs (G2).** The ingress never refuses for one; the route table still declares them. The hold is the
  core's `CND-CutOff` (`services/core/tests/conditions.rs`), not duplicated here.
- **What a request may name: an explicit kind (review of PR #44, M1, M2, S1, S2).** The first version derived "arrival
  at a reserve" from an observation's columns and let a row that named no event through (fail open). Now every
  observation carries `kind`, written by the endpoint that saw it, from a closed set:
  - origin network: `arrival`, `other_arrival` (a token not admitted), `convertible_arrival`, `conversion_output`,
    `conversion_input`, `payout_leg`, `outflow`, `self_transfer`;
  - issuing network: `mint`, `redemption`, `refund_mint`, `control` (pause, unpause, bounds);
  - either: `transaction` (bezent's own signed transaction), `other` (anything else; the default when none is
    written; nothing binds it).

  A network records only the kinds its role produces, and each endpoint's login records only its own network's
  observations and outcomes: an endpoint role is registered in `endpoint_scopes` with the networks it covers, and a
  login holding `bezent_endpoint` with no registered role records none. The Kreivo endpoint's login now holds
  `bezent_kreivo_endpoint` (the issuing network) and writes the kind (`ContractEvent::kind`; `transaction` for its
  proofs' observations). **Deployment:** grant the Kreivo endpoint's login `bezent_kreivo_endpoint` (it inherits
  `bezent_endpoint`) before this migration's code runs, or it records nothing. Rows recorded before 0034 are
  backfilled from `detail.event` for the issuing network only; every origin-network row is `other`, because before
  0034 any login holding `bezent_endpoint` could write one and none of its `detail` is verifiable. A row with no
  event, or an event the issuing network does not produce, is `other`. An observation whose `detail` carries an
  `event` is refused unless its `kind` is the one that event names (the book still reads `detail.event`). **Deployment:**
  the Kreivo network must be declared in `networks` as the issuing network (and the endpoint's scope in
  `endpoint_scopes` covers it) at deployment, or the endpoint records nothing; stop the endpoint across the
  migration, since the backfill does not run again.

  The store's `source_bindable()` decides by kind: a **deposit** binds `arrival` or `conversion_output` (REQ-CV-2: the
  deposit is minted against the conversion's output), not from a reserve or a venue address; never a
  `convertible_arrival` (REQ-CV-1). The reserve and venue sender exclusions apply to an `arrival` only: a conversion
  output's sender is the venue by design, so a pool listed in `venue_addresses` does not block its deposit. The
  **refund of an unassociated arrival** binds any arrival at a reserve
  (REQ-IN-6): `arrival` whatever its amount or sender (a venue delivery, a leg from another reserve, an amount outside
  the route's bounds), `other_arrival`, `conversion_output`. **Never a `convertible_arrival`:** it is held until it is
  converted and its value is released only through its conversion (REQ-CV-1, REQ-CV-6); a release path for an
  unconverted one would take an amendment. Neither binds an observation with
  an on-chain reference. The rule runs on the insert of the transfer, on a binding made by an update, and again when
  the source becomes final; a source on a declared origin network never becomes final with no observation.
- **An arrival not yet final is retryable (review S4).** A deposit or refund naming an observation of a bindable kind
  that is not final yet is answered `not-final` (HTTP 503, `Retry-After`), with nothing recorded and the request
  reference not consumed: the same request sent again once the arrival is final is taken in. One withdrawn before
  finality stays `ERR-SourceNotFound`, recorded. Stated in the OpenAPI description of `coordinate`. Swaps should send
  its request after the arrival is final, or retry on `not-final`.
- **The attest grant is not per backend (review S3), a V0 narrowing.** REQ-RQ-4 says "that backend's attesting
  grant"; `requester_grants` has no backend column, so one `attest` grant opens the extras of every attested backend.
  Acceptable for V0, with one attested backend and one attesting party. Revisit when a second attested backend is
  declared: `requester_grants` then gains a backend.
- **Left open, each to its own issue:** a conversion deposit records only the output's coordinate; REQ-CV-2's record of
  the convertible arrival with both amounts is not built. A refund of a `convertible_arrival` that was since converted
  is not linked to its `conversion_output` in the store, so both could be released; the operator's second approver on
  a refund (REQ-RQ-6) is the check until they are linked. The reserve book still classifies by `detail.event`
  (`services/core/src/reserve/book.rs`); moving it to `kind` is F-004's.
- **What the EVM endpoint (PR #39) must do to land on this.** Write `kind` on every observation:
  `Arrival` → `arrival`, `OtherArrival` → `other_arrival`, `ConvertibleArrival` → `convertible_arrival`, `Conversion`
  → `conversion_output`, `ConversionInput` → `conversion_input`, `Payout` → `payout_leg`, `Outflow` → `outflow`,
  `SelfTransfer` → `self_transfer`, `ZeroTransfer`, `UnpairedReference`, `UndecodedReference`, `UndecodedTransfer` →
  `other`, and its signed transactions' observations (`position_kind = 'transaction'`) → `transaction`. Register its
  role in a migration numbered after 0034: `INSERT INTO endpoint_scopes VALUES ('bezent_evm_endpoint', 'origin',
  'evm')`; without it, its login records no observation.

## M-1 part: webhooks (issue #2, T-002-07)

Migration `0044_webhooks` (additive; numbered after main's 0043_ledger_accounts); code in `services/ingress/src/webhooks/`; tests in
`services/ingress/tests/webhooks.rs`. The sequenced log stays the source of truth: a notification only points at a
position in it, and a requester that receives none loses nothing (`REQ-ST-4`, `REQ-ST-5`).

**Registration.** `PUT /v0/webhooks/transfers/by-request/{reference}` or `…/by-bridge/{reference}`, body `{"url"}`:
one callback URL per (requester, transfer), set only for a transfer the requester's own request made or reached (404
otherwise, and a store trigger refuses it too: `REQ-ID-4`). Its own requester may replace it (Swaps issues a new
callback token on each retry): a `PUT` with another URL replaces it (200), the same URL again changes nothing (200).
Every change is a new row of an append-only table: the actor (always the requester), the time, and a keyed digest
(HMAC-SHA256 under a subkey of the webhook key, PR #55 review S5) of the
new and of the replaced URL; the URLs stay sealed. A delivery goes to the URL current when it is attempted, so a retry
after a replacement reaches the new one with the same bytes. Another requester can neither set nor see it. `PUT /v0/webhooks/conditions`, body `{"url"}`: the condition URL;
the latest is used. URLs are `https`, absolute, with no user information or fragment, and an IP literal must be a
global address (see "Where a webhook may go"). `POST /v0/webhooks/secret` makes and returns a new signing secret, once.
`GET /v0/webhooks/deliveries?state=pending|delivered|dead` lists the requester's own deliveries. Without a
`[webhooks]` section the surface answers `webhooks-disabled` (503).

**Payload.** Transfer: `{"type":"transfer","seq","dedupe_key":"log-<seq>","bridge_reference","request_reference"}`
(the requester's own reference). Condition: `{"type":"condition","seq","dedupe_key":"log-<seq>-held|released-<CND>",
"condition","route":{"asset","network","kind"}}`. Nothing else: no state, no amount, no outcome; the requester
re-reads. Headers `bezent-timestamp` (Unix seconds), `bezent-signature` (`v1=` hex HMAC-SHA256 of
`<timestamp>.<raw body>` under the requester's secret), `bezent-dedupe-key`. The body is built at send time from the
log entry the delivery points at, and is the same bytes on every attempt; the timestamp and signature are per attempt.
A receiver (PR #55 review S1) rejects a `bezent-timestamp` outside a bound of its clock (for example ±5 minutes) first,
so a captured delivery cannot be replayed later, then verifies the signature in constant time, then de-duplicates by
`dedupe_key`, keeping keys at least as long as the whole retry window (about 70 minutes by default; it follows the configured
`max_attempts` and back-off, not the timestamp bound, because a retry reuses the key with a fresh timestamp). Rotation (S2) takes effect at once, retries included: the receiver
accepts the old and the new secret until it holds the new one; a delivery it rejects meanwhile is retried and counts
toward the dead-letter bound. Both are in the OpenAPI description and the `WebhookSecret` schema.

**Where a webhook may go (PR #55 review M1).** A requester names the URL, so the dispatcher must not be its way into
bezent's network. At registration: scheme, user information, fragment, and an IP literal or a denied host refused
there already; names are not resolved then. At send time, the guard: the host is resolved once and every address it
resolves to must be admitted, or nothing is sent; the attempt's client is then pinned to those vetted addresses and
resolves nothing else, so DNS rebinding cannot swap the address between the check and the connection. Admitted: global
unicast. Loopback, RFC 1918, unique-local and CGNAT addresses only for a host on `[webhooks] allow_hosts` (the
harness's), which may also take plain `http`; the ingress refuses to start with an allowlist on a store that declares
a production network, so a configuration line alone never opens it in production. Never, allowlisted or not:
unspecified, link-local (`169.254.169.254`, `169.254.170.2`, `fd00:ec2::254`), multicast, broadcast, documentation,
benchmarking and reserved ranges, the IPv4-mapped, NAT64 and 6to4 forms of a refused address, and bezent's own service
addresses (`[webhooks] deny`: addresses, networks, `ip:port`, host names). Redirects are off, proxy variables are
ignored, the connection has 3 s and the request `timeout_ms`, and the response body is never read. A requester sees
only a coarse class in `last_error` (`no-secret`, `url-unopenable`, `url-not-allowed`, `address-not-allowed` (a name that does not resolve is the same class, so the listing is no oracle for internal names),
`timeout`, `connect-failed`, `http-status`, `request-failed`, `notice-refused`; a store check holds it to
that set), never an address or an error text; the operator's log keeps a detail without the URL.

**Delivery.** A dispatcher (a task beside the ingress, on a two-connection pool of its own, logged in as its own
login: `[webhooks] database_url`, a member of `bezent_webhooks` only; PR #55 review M2) fans the log out from a
forward-only cursor into `webhook_deliveries`, unique per (requester, de-duplication key), in one transaction with the
cursor's move; a second dispatcher skips a locked cursor and claims deliveries with `SKIP LOCKED` and a lease. Each is
posted until a 2xx, with exponential back-off (2 s doubling, capped at 1 h, a quarter of jitter), and
dead-lettered after `max_attempts` (12). Nothing is sent unsigned: a requester with no secret has its deliveries
retried and, at the bound, dead-lettered, with the reason. URLs are never logged (a `reqwest` error is stripped of
its URL). Logs: info when delivered, warn per retry, error when dead.

**The dispatcher's role (PR #55 review M2).** `bezent_webhooks` is a member of no role and no service role is a member
of it. Only it reads the sealed secrets and URLs, moves the cursor, keeps the condition holds and writes deliveries; a
trigger refuses a delivery or a hold from any other role (the table's owner and a superuser aside, NFR-7), whatever is
granted. Only the ingress API's own role, `bezent_ingress` (it inherits `bezent_service`), holds INSERT on secrets and URLs, and a trigger on `session_user` refuses any other login: the core's and the endpoints' logins cannot add a row that would become a requester's "latest" and silently stop its webhooks (PR #55 re-check S-c). The ingress reads back only unsealed columns. A delivery row carries no
body and no URL, only what it points at (log position, transfer, condition and change), and its key is checked to be
the one derived from those. At send time the dispatcher reads the log entry, checks the row against it (the entry's
transfer, the condition taken on or left, the core's own condition entry) and the recipient's standing (its request
reached the transfer, or it is granted on the route), and builds the body; a row that does not match ends dead at once
as `notice-refused`, unsent. So no service login can have bezent sign and post a body of its choosing to a requester's
URL. Provisioning: create the ingress API's login `IN ROLE bezent_ingress` only, and the dispatcher's login `IN ROLE bezent_webhooks` only; the ingress refuses to start the
dispatcher on a login without it.

### Choices made here, open to review

1. **Signing secrets are sealed, not hashed, and not the signer's.** An HMAC key must be readable to sign. It moves
   no value, so `REQ-AR-3`/`REQ-KY-*` do not put it in the signer. It is sealed with ChaCha20-Poly1305 under the
   ingress's webhook key (`[webhooks] key_file`, 64 hex characters, never an environment variable), with the
   requester's name as associated data. Not the credential's sibling: a credential is stored as SHA-256, and making
   that digest the HMAC key would let anyone who reads the store forge notifications. Callback URLs are sealed the
   same way, because Swaps' resolve URL carries its access token in the query. Losing the key loses secrets and URLs:
   requesters take a new secret, and transfers registered before can be registered again (a requester may replace its own URL); the log
   covers them.
2. **Only entries after the registration are notified.** A registration records the log's head (`after_seq`);
   entries at or before it are not fanned out to it. Swaps registers in `afterPersist()`, after the answer, and its own
   scheduled re-read covers the gap. Deterministic, where "whatever the dispatcher had not reached yet" was not.
3. **Which conditions, to whom (`REQ-IC-5`; orchestrator ruling 2026-10-15).** A condition on a route is notified to
   the condition URL of every requester granted on the route (a grant of the route's kind, deposit or withdrawal),
   whether or not it holds a transfer there. The shared gate conditions (`CND-Paused`, `CND-CutOff`,
   `CND-ReserveShort`) are notified once when the condition starts (the first transfer of the route is held) and once
   when it clears (the last is released); the in-place holds and releases themselves are not notified per transfer.
   `CND-ReserveShort` starts and clears with the core's `condition-raised` / `condition-cleared` entries, which name
   an asset and a network and are notified as a withdrawal route. Any other
   condition (`CND-AmountMismatch`, …) is that transfer's news and goes to its URL. A route is
   `(asset or token, network, transfer kind)`, the network being the source network of a deposit and the destination
   network otherwise.
4. **Other entries with no transfer are not notified**: `refusal` answers without a transfer, operator and reserve
   signals (the core's `condition-raised`/`condition-cleared` of `CND-ReserveShort` are the exception, choice 3).
   They are not a requester's transfer, and the log reader does not show them either (choice 11 above).
5. **No redelivery of a dead letter.** The recovery is the log from the requester's last position; the dead letter is
   observable at `GET /v0/webhooks/deliveries?state=dead` and in the error log.
6. **The sidecar runs the dispatcher** as its own login (`bezent_webhooks`), with `https` to global addresses and plain
   `http` and internal addresses only for the hosts `BEZENT_SIDECAR_WEBHOOK_HOSTS` names (none by default), its own
   listeners denied, and a key read from the file `BEZENT_SIDECAR_WEBHOOK_KEY_FILE` names (mode 600; never the key
   in the environment, PR #55 review S3), or a random one in memory, with which registrations
   do not survive a restart.

### Spec gaps found (each needs a ruling before it is relied on)

- **W1 — conditions that hold none of a requester's transfers.** `REQ-IC-5` names "a route paused, a network stalled,
  reserve short" but not who is notified. Ruled 2026-10-15: every requester granted on the route, whether or not it
  holds a transfer there (choice 3). Still unbuilt: a pause of a route with no transfer on it at all, and a stalled
  network (the reserve check's `reserve-check-stalled`), write no log entry that names the route, so they reach nobody
  until the log reader shows them.
- **W2 — the dead-letter bound and what follows it.** Ruled 2026-10-15: at-least-once within a declared retry bound,
  then dead-lettered and visible at `GET /v0/webhooks/deliveries`; the log is the source of truth (see "Delivery
  guarantee"). Default 12 attempts over about 70 minutes, configurable.
- **W3 — Swaps' callback URLs** (`the Swaps integration pull request`, F-012). (a) Resolved 2026-10-15: a retry that adopts a live
  transfer makes a new callback token, and the requester may replace the URL of its own transfer (registration above).
  (b) Swaps' `/exec/resolve/<token>` takes its service token as `access_token` in the query, so the URL bezent holds is
  a Swaps credential (sealed here, choice 1); the module could check `bezent-signature` instead. Neither blocks: the
  module re-reads status on any callback.

### Security review of PR #55: what is fixed, what is still open

Fixed in the PR: M1 (send-time address guard, pinning, coarse error classes), M2 (the dispatcher's own role, bodies
built from the log at send time), S1 and S2 (receiver guidance: timestamp bound before the signature and de-duplication;
rotation cut-over), S3 (the sidecar reads its key from a file), S5 (keyed URL digests), S6 (the key file must be mode
600), and the OpenAPI line that said a condition reached only requesters with a held transfer.

Still open, each larger than a fix in this PR:

- **S4 — growth without bound.** Every `PUT` of a URL and every `POST /secret` adds a row; `webhook_deliveries` refuses
  DELETE and has no retention; none of these routes takes an intake permit or a rate limit. To do before a second
  requester exists: a cap on URL changes and rotations per requester (and per transfer), and a retention for finished
  deliveries (partitioned by month and archived under an operator role, the rows staying append-only until then).
- **S7 — a stall is silent, and one slow receiver delays the rest.** A failing fan-out page rolls back and is retried
  every tick, logged at `warn` only; nothing exports the cursor's lag behind the log's head, so operators cannot alert on
  it. A batch waits for its slowest attempt (connections now give up after 3 s, the request after `timeout_ms`), so a
  receiver that holds every request open delays every other requester by up to that. To do: export the lag (the head
  minus `webhook_cursor.after`, and the age of the oldest pending delivery) where the other services' metrics go, and
  run attempts per requester with their own concurrency so one receiver cannot hold the batch.

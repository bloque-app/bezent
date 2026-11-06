<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-005 — notes

Decisions taken building the `M-0` stub (`T-005-01` to `T-005-06`), the security review's fixes (PR #21), and what
is left.

## Decisions

1. **The signature table backs the guards in the store.** `signatures` (migration 0012) holds one row per intent
   and one per on-chain reference; a trigger copies the reference and the amount from the recorded intent, so the
   signer cannot sign for a reference or amount the core did not record. The service role reads it and never writes
   it. The signer's guards check first; the store makes a race between two signers harmless.
2. **The intent payload the signer reads** (`intents.payload`, F-001 left it free): `message` (`0x` hex, the exact
   signing payload), `amount` (decimal string of minor units, `INV-11`), `from` (`0x` hex, for a destroy). The core
   (`F-003`) must write these. Key selection is by the intent's `network` and `purpose` through the configuration's
   bindings; the purpose that triggers the Kreivo guard is `destroy`.
3. **The link is a Unix socket with peer credentials and a token**, both ways checked by user id, as aledgent's
   `F-035` built it. No mutual TLS: the core and the signer share a host or a task, **but never a user id**
   (review S4, `REQ-CU-1`): `Callers::new` refuses a caller with the signer's own uid, so the binary does not start
   beside one. Only tests, which run both ends as one user, build the callers without that check.
4. **The window cap sums the key's signatures in the window**, across its purposes, from the store; signing is
   serialised by a process mutex and an advisory lock. The guard trusts the intent's declared amount: decoding the
   call to check that the bytes move that amount comes with the endpoints that build the transactions (`F-007`,
   `F-009`). See Open.
5. **The compromise path writes to the event log.** The signer may write nothing but signatures, so
   `compromise::check` runs in the core, as the service role: a signature that does not verify appends
   `key-compromise` (the alert record, with the key and the coordinate) and `pause-requested` (scope `global`) in one
   transaction. The pause itself, rotation and paging belong to the pause path and operations (`F-014`). Issue #77
   built the pause: see "The compromise pause". The `pause-requested` row stays as the record of the request.
6. **A replay returns the recorded signature**; it never signs again. sr25519 signatures are randomised, so the
   restart test proves this by byte equality.
7. **Production cannot be configured in `M-0`.** The file backend refuses a key file not tagged `test`; a binding of a
   key to a network of another environment fails the start; an intent for such a network is refused at sign time.
8. **A row the signer wrote proves nothing** (review M1, `INV-9`, `AC-I1.2`). The signer is what the compromise
   path must catch, so a signature is bezent's only if it verifies, with the key's public key, over the exact
   `message` of an intent the core recorded. `compromise::check` uses the `signatures` row only to find the candidate
   intent (public key and `r ‖ s`, low-`s` normalised so a malleated secp256k1 signature is no false alarm, review
   N5) and then verifies. The core also verifies every signature the link returns (`Client::sign`, exact length,
   low-`s`, recovery byte) before it hands it on; one over other bytes raises `key-compromise` and the global pause
   request, and is never returned.
9. **Only the coordinator records an intent** (review S5, `INV-9`). Every service holds `bezent_service`, so that
   role lost INSERT on `intents`; the new `bezent_core` role, held only by the core's login, has it.
10. **The signer's start proves its login** (review S3, `REQ-AR-3`). `SET ROLE` is undone by any `RESET ROLE`, so
    `check_login` refuses a login that can become any role but `bezent_signer`, owns anything, has a role attribute
    (superuser, create role or database, replication, bypass row security), or holds a privilege beyond the signer
    role's allow-list. A `RESET ROLE` then gains nothing.
11. **No core dumps** (review S4, `REQ-CU-1`). The process sets `RLIMIT_CORE` to 0 (soft and hard) and, on Linux,
    `PR_SET_DUMPABLE 0`, before it reads a key. On Linux a failure stops the start; on macOS there is no
    `PR_SET_DUMPABLE` and a failure is only logged.
12. **A mint of a reference bezent did not deliver is a signature it did not record** (issue #26, PR #72; `AC-I1.2`,
    `REQ-IC-4`, `INV-8`, `INV-9`). The check is the core's (`services/core/src/late_mint.rs`), over the observer's
    final `Minted` and `RefundMinted`; it raises through the compromise path above, once per observation. It raises
    when a reference bezent ordered was minted (a) after a signed transaction of it was proven not executed at a final
    block below the mint, or (b) with no signed transaction of it at all: the endpoint stores the exact bytes before it
    submits (`INV-9`), so a bezent mint always has its `signed_transactions` row first. A mint of a reference with no
    order is not matched: it is `REQ-IC-2`'s reserve check. A later attempt explains a mint (`REQ-RF-5`) only if it was
    executed in the mint's block and the block has no more mint events of the reference than executed attempts, or it
    is undecided by the prover (no proof, no inclusion and no anomaly for it) and its era covers the mint. The wait
    ends **only on the prover's own record**, never on the final head: the observer and the supply reader write the
    head, the prover is a separate worker, and a head past the era's end says only that the prover could have decided
    (ending the wait on it raised a global pause on a legitimate retry whenever the prover was behind, down, or the
    retry was included in the last blocks of its era, or the observer was catching up over old history). The prover
    writes no progress marker of its own and none was added: `Prover::step` records a proof, an inclusion or an
    anomaly, and nothing when it cannot decide. An attempt with no era on record (the signing path always records it,
    migration 0037) waits like any other, with an error logged each cycle. *Not tied to the extrinsic*: the observation's position is the event's index in the block, not
    its extrinsic's, so the count is what the record supports; tying them needs the endpoint to record the event's
    phase (`ApplyExtrinsic(i)`, read in `chain/light.rs` as `ev.phase()` and dropped) in the observation's `detail`.
    The alarm is a global pause: every widening has a test that a normal delivery raises nothing, and an undecided
    attempt is held, never raised on, while the prover has not decided it. Migration 0075 keeps a queue
    (`late_mint_checks`), filled by the store in the outcome's own transaction, so a cycle reads only the mints not
    yet settled and no id or time order can make it skip one; a look that fails is counted and pages after three
    in a row (`late-mint-check-failed`), without stopping the others, and so does a cycle whose candidate query
    fails (`late-mint-watch-failed`, once per streak; it used to be a warning).
    *A prover that does not decide* (migration 0076, `held_cycles`): a held row stays open and raises nothing; each
    cycle that finds an attempt undecided with its era over at the final head counts, and at 60 cycles (five minutes at
    the core's default) the core pages, durably (`late-mint-prover-silent`, once, then once a day while the mint is still held with the prover still silent, read from the log),
    naming the reference, the attempt, the signed transaction, its era and the head. No pause, no key-compromise
    alert; the row stays open, so if the prover later decides against the mint the alarm still raises, once. An
    operator learns that the prover is stuck or that a key may be compromised; during a long observer catch-up the
    page can also mean only that the prover (stepped after the observer in the endpoint's loop) has not had its turn. **`max_walk`**: when a nonce is
    consumed the prover walks back from the finalized head to the era's birth and gives up (`unknown`, nothing
    recorded) when that is more than `max_walk` blocks, at every later pass; an attempt included that long ago is
    therefore never recorded, and it is this page's case, not a raise (the mint may be that attempt's, and the core
    cannot tell). The page carries the era and the head, so the gap shows; the operator resolves it by reading the
    chain. `max_walk` is unchanged. A nonce that was never consumed does not walk and is proven once the era ends.

## The compromise pause (issue #77)

Before this, `raise` appended `pause-requested` and nothing read it. `REQ-IC-2`, `REQ-IC-4`, `AC-I1.2` and `REQ-BK-1` say
bezent MUST pause. Recovery is in `deploy/compromise-pause.md`; what the specification left open is amendment 0022, ruled
by @pandres95 on 2026-11-06 (`d-pause-0022`, "As recommended": A1b, A2a, P1 kept, P2 built; A3 and A4 not on the item).

### 1. What stops, and what cannot be stopped

| Layer | Under a global row in `pauses` |
|---|---|
| Ingress | Refuses new requests (`ERR-RoutePaused`, `intake.rs`, the `NULL`-matches-all test) on the next request |
| Core | `gate` reads `pauses` on every step: before each phase of a first attempt (the transfer stays `Source-Final` until a signature is ready), a new attempt, a resolution. It records no further order or signing intent. So the signer has nothing new to sign (`INV-9`) |
| Core, an attempt already signed | Passes, under any pause, a person's or the reserve check's too: it is in flight (`REQ-GV-1`, `INV-15`). Before #77 the gate held it `Source-Final` with `CND-Paused` while its transaction was on chain; the auto-pause would have made that common. A later preparation of the same order that fails its dry run (the signed transaction was never persisted and its era passed) records no new order while a pause covers the route (`deliver`; test `inv_15_a_signed_attempt_whose_new_preparation_fails_orders_nothing_under_a_pause`) |
| Refund mint, refund payout, cancel, statement | Keep being ordered and signed, by design (`t.route()` is `None`; F-006 NOTES 1; `REQ-NX-4`; `REQ-PR-1`). "Stops new signing" (issue #77) means new delivery signing |
| Signer | **A signing intent recorded before the pause, whose attempt has no signature, is new work and is not signed** (amendment 0022, A1b; `REQ-GV-1` v0.5.7: "New work is everything before an attempt's first recorded signature"). The signer asks the store's `pause_holds_intent` (migration 0125, `SECURITY DEFINER`; the signer reads neither `pauses` nor `transfers`) last, after every guard that raises, inside its signing transaction, and refuses with `REQ-GV-1`. Before the ruling it read no pause, and such an intent was signed at the next poll, or whenever a stopped endpoint returned: on Kreivo within one era, on an EVM network with no bound. A replay, a refund mint, a cancel and a statement are not held. Tests `conditions.rs::ac_i2_1_inv_15_a1b_an_intent_recorded_before_the_pause_is_not_signed_until_it_is_lifted` (core) and `delivery.rs::ac_i2_1_a1b_an_intent_recorded_a_moment_before_the_pause_is_not_signed_until_it_is_lifted` (Kreivo endpoint) |
| Endpoints | Read no pause. A refusal of the signer is "not signed": the intent stays and is asked for again on the next pass, and signed once the pause is lifted |
| Already-signed transactions | Stored bytes are submitted and resent: in flight, and unstoppable anyway once anyone holds them. On Kreivo an era ends them; on an EVM network only the nonce does |
| On-chain component, vault | **Not touched.** Their pause is a separate act of a separate role (`REQ-HN-9`, §7.F, `AC-I2.*`), a person's: runbook step 1; amendment 0022, A2a (ruled: it stays a person's act) |

So: bezent stops itself. **It cannot stop a stolen key**, which signs by itself; that is the on-chain pause and, as the
backstop, the bounds of `REQ-KY-3`. Likewise a compromised signer process ignores anything asked of it. What a pause adds
against a real thief is that bezent stops feeding new transfers through a key it no longer trusts, and a person is paged
with the bridge already quiet.

**Which routes.** The spec says "the affected routes" (`REQ-IC-2`) and "pause" (`REQ-IC-4`). A detection knows a public
key and a network, or only an intent (`signer-link`, `signer-guard`). It does not know how far the breach goes, and all
keys share a signer. A Kreivo key is every route: every deposit mints with it and the issuing network names no route, so
no row but the global one expresses it. Global covers the affected routes in every case and costs only new work
(`REQ-GV-1`), so it is what's built, as decision 5 recorded. Narrowing is amendment 0022, A3. The core's `INV-4` supply
mismatch is the exception that already exists and knows better: it names its asset and pauses that (0040).

### 2. Who applies it, with what grant

**The store, in the alert's own transaction: a `SECURITY DEFINER` trigger on `event_log` (migration 0095). No role gains
any privilege.** `pauses` has the same rows in the matrix as before: the signer and the endpoints hold nothing on it, the
core keeps `pauses(asset, cause, evaluation_id, quarantine_id, reading_id)`, and nobody holds `alert_seq`. The trigger
function is not `PUBLIC`'s.

The obvious design widens `pauses_guard_insert` and gives four roles `INSERT (cause, alert_seq)`, with a definer helper
checking the alert. It was rejected because it gives more for less: the signer, which writes nothing but signatures and
its alert, would hold a privilege on `pauses`; a helper and a same-transaction check would have to re-derive what a trigger
knows by construction; and a raiser could forget the insert, or commit an alert without it. Here a pause cannot be forged
by naming an alert, an old alert cannot be replayed, a pause cannot be narrowed by its writer, and no alert exists without its
pause. The guard gains only the check that `alert_seq` is a `key-compromise` row, which the owner is held to as well.

**This gives every detector login a power it did not have: to pause the whole bridge.** Before, a detector's alert
asked a person (`pause-requested`, read by nothing). Now the alert is the pause. A detector login (the core, either
endpoint, the signer) can write an alert with a `dedupe` of its own choosing (a free-text key, 0049: it has no meaning to
the store beyond uniqueness within the detector), and every new `dedupe` is a new alert, so a new global pause, until two
operators lift it. A compromised or buggy detector can therefore hold the bridge down, repeatedly, and an alert stays in the
log for ever. It is an outage, not a loss (it stops new work only, `INV-15`), and the same login could already page a person
by the same act; but it is a new authority. The owner accepted it for now (amendment 0022, P1, ruled 2026-11-06). It still cannot name a scope (`detector` is the store's stamp, so only the core has the `asset` exception), lift a
pause or edit one. The ingress, the webhook dispatcher and the operators' logins cannot write an alert (0020, 0046, 0049).
`REQ-AR-3` and the key rules are untouched: no key near the store, nothing new in the signer process.

### 3. The bound

- **Detection to pause: none. The alert's commit is the pause's.** It needs no core and no `check.tick`, and it holds with
  every other service down (test `ac_i1_2_the_core_down_at_detection_the_pause_holds_when_it_returns`). If the store is
  down nothing is recorded, but then nothing can be signed either: the signer releases a signature only once recorded
  (`INV-9`), and the detector raises again at its next look.
- **Pause to no new signing intent: the next `pauses` read of the next step**, at `READ COMMITTED`; the core loop is
  sequential, so at most one step already past its gate at the commit records one more intent. Inside `NFR-8`.
- **Pause to no new signature: the signer's next read of `pauses`** (amendment 0022, A1b), in its signing transaction, at
  `READ COMMITTED`, under its advisory lock: a signature that read the store before the pause committed is the last one,
  on every network. Inside `NFR-8`. Until the ruling a signing intent recorded before the pause was signed, one era on
  Kreivo, unbounded on an EVM network; that is closed.
- **Detection itself lags the event**, separately from all of this: for what the endpoints see on chain, finality, plus
  observer lag (`NFR-6`), plus the prover's cadence; a lane's idle use is seen only at the next preparation. The core's
  late-mint check runs in its cycle (`Core::cycle`), so a slow `check.tick`, which has no timeout, delays that detection and
  the core's own. While it is stuck the core records no intent, so a slow tick cannot admit new signing. It does delay
  #76's page, which comes after the tick; that page's job is done by the pause already being in force.

### 4. Every raiser, and false alarms

All go through `compromise::raise` except the last. A global pause from a wrong alarm is an outage, recovered only by the
two-login lift (`AC-I2.2`): no new path. "Flag" means I would not want to pause on it as it stands. `REQ-IC-2` and
`REQ-IC-4` require the pause for each ("a condition it cannot explain"), so it is built and flagged, not exempted.

| Route | Fires on | How it could fire falsely | Pause? |
|---|---|---|---|
| Core, `engine.rs` `verified` → `returned` | A recorded signature not over the intent's bytes | Signer encoding bug; a restored store | Yes |
| `link::Client::sign` → `returned`: Kreivo and EVM submit, core statements (test bed: `sidecar` stand-in) | The signer's answer is not over the recorded bytes | Same. A statement-key fault pauses deliveries too: the signer is suspect either way | Yes |
| Signer, `signer.rs` `Answer::Mismatch` | The bytes do not answer the core's order | Endpoint or signer decoder version skew, a format change: every intent | Yes, **flag** at upgrades |
| EVM `proof.rs` `check_signature` (2), Kreivo `proof.rs` `check_signature` (2) | A consumed nonce's signature answers no intent | A store restored to before the signature (`NFR-7`) | Yes |
| EVM `proof.rs` `raise_once`, consumers | Not exactly one lane transaction at its nonce in the consuming block | **An incomplete block read**: the nonce reads agree across providers, the block is one read | **Flag** |
| EVM `proof.rs` `raise_once`, undecoded | The consumer is not an EIP-1559 transaction of the lane | A provider's bad decode; otherwise true | Yes |
| EVM `submit.rs` `lane_runs_code` | Both providers report code at the lane (EIP-7702) | Both providers wrong together. One provider's word is unknown: it pages `lane-use-unknown` and raises nothing (amendment 0022, P2) | Yes |
| EVM `submit.rs` `lane_use` | Both providers report the lane's nonce past every nonce signed plus one; the lower of the two is raised on | Both providers wrong together; a restored store. One provider's word is unknown, as above (P2; `Pair::latest_nonces`, each provider on its own, not the higher of two) | Yes |
| EVM `submit.rs` signer-key | The answer is not the lane key's | A half-done rotation | Yes |
| Kreivo `proof.rs` `raise_once` | A nonce consumed by an extrinsic of the lane's account that is not one bezent recorded and cannot decode, or by none | An extrinsic bezent recorded in an older format is recognised by its hash against every recorded signed transaction of the lane before anything is called undecodable. **A runtime upgrade does not reach this path** (below). Otherwise true | Yes |
| Kreivo `submit.rs` `signer_mismatch` | The answer is not the lane key's | A half-done rotation | Yes |
| Kreivo `submit.rs` `check_idle_use` | The nonce at the finalized head is past every nonce signed plus one | A restored store | Yes |
| Core, `late_mint.rs` | A final mint of a reference after its proof, or with nothing signed | A restored store; the prover-behind case was removed in PR #72 | Yes |
| Core, `reserve/check.rs` `supply_breach` (`INV-4`, not through `raise`) | The contract's count and the issuance differ | Unchanged: it pauses the asset under 0040, the trigger leaves it alone | Yes, the asset |

Hardening the flagged EVM ones changes when they fire, not the spec. The lane detectors ask both providers before
alarming since the owner ruled amendment 0022's P2 (2026-11-06; test
`submit.rs::ac_i1_2_p2_one_provider_reporting_the_lane_used_outside_bezent_pauses_nothing_and_both_do`); the consumer raiser
(read the block from both) was not on the item and is unchanged. The Kreivo one is done (review of this
change): an extrinsic of the lane's account that `decode_extrinsic` rejects is matched, by `blake2_256`, against every
signed transaction recorded for that lane, not only the one watched, before it is called undecodable. A match is bezent's
own: nothing is proven (`REQ-NX-3`), and nothing is raised. An unrecorded one still pauses.

**Does a runtime upgrade reach it? No.** `decode_extrinsic` is fixed-format code (`SIGNED_V4`, the address, a
`PassAuthenticate` of `None`, no fee asset) and reads nothing from the chain. A runtime upgrade does not change the bytes of
a transaction already on chain. One bezent signed before an upgrade carries the old `spec_version` and `transaction_version`
in what it signed, so the chain refuses it afterwards and it never lands. After an upgrade bezent builds and signs nothing
for a `spec_version` it has no format for (`SPEC_VERSIONS`; the payload check in `submit.rs`), until a build that knows
that format, and the same build decodes it. So an extrinsic of the lane's account that this decoder rejects, in a block
after an upgrade, is one bezent did not sign. What can make a recorded one undecodable is a code deploy that changes the
decoder, and the hash match covers that. The earlier reading of this table, and of the runbook, was wrong.

### Restoring the store (`NFR-7`)

The trigger is not applied to a session of the schema owner or a superuser (`session_user`; the exemption of 0020, 0047,
0049, 0050): a restore replays alerts whose pauses were lifted or are in the same dump, and applying it would resurrect
the first and collide with the second (`pauses_one_per_alert`). The restore drill (`restore_drill.rs`: `pg_dump` plain,
`psql` as the owner) creates the trigger after the data, so it never fired there, and it now also checks that a standing
pause is restored and a lifted one is not. A restore that loads into a store already migrated does fire it, and is
covered by `nfr_7_an_alert_the_owner_replays_pauses_nothing_and_its_own_pause_row_still_loads`. **An alert the owner writes
outside a restore pauses nothing either**: the owner is not a service, holds every privilege already, and pauses by
inserting the pause naming the alert (the guard admits it). No service connects as the owner.

### The other late-mint rules

`late_mint.rs` was read again for a path by which a normal delivery raises. None found. "Nothing signed" needs a mint of a
reference with an order and no `signed_transactions` row, and the endpoint stores the bytes before it submits (`REQ-EO-1`):
only a store restored to before that row, or a mint by something else, gives one. "After a proof" with an attempt that
legitimately follows (`REQ-RF-5`) is explained by that attempt's inclusion in the mint's block, and while the prover has not
yet recorded the inclusion it is held (a page after a bound, never a raise). The count rule (`events <= executed`) errs
toward explaining. The remaining true positives are a restored store and a backfill of history that predates
`signed_transactions`, which 0075 ran once, long before this change.

### 5. Idempotence and replay

- **One alert, one pause.** `pauses.alert_seq` is unique; the trigger writes once per alert row.
- **A repeat adds nothing and fails nothing.** A repeated detection is refused by 0049's unique index (`23505`), which
  `raise` reads as "already raised", and so writes no alert and no pause, while paused or after a lift. Each detector also
  asks `raised` first. The pause cannot itself raise `23505` here: a new alert has a new position.
- **After a lift, only a new detection pauses again.** That follows from the above, and is proven at both layers.
- **A second, different detection while paused gets its own row.** One row per bridge would let an approval given for
  the first alert lift a pause over a second nobody had looked at, and #76's lift deletes by pause id. The cost: each
  row takes its own two-person lift, and one event two detectors both see makes two rows.
- **A pause already in place stands in for nothing.** Whoever lifts a person's or the reserve check's pause may never have
  seen this alert.
- **An alert from before 0095** has no pause, and its repeat never gets one: the runbook checks for them before deploying.

### Tests, at the layers §9 names

`crates/store/tests/compromise_pause.rs` (the store: real logins for each role, guard, unpause refused, repeat, second
alert, pause in place, one transaction, concurrency). `services/kreivo-endpoint/tests/delivery.rs` (service, each process on
its own role: `ac_i1_2_a_detection_stops_signing_end_to_end`, core down, signed attempt in flight).
`services/ingress/tests/limits.rs` (new requests refused). Every row of the table in 4 has a test that asserts the pause and
fails without it, in the signer, core and endpoint suites, except the core's statement caller of `Client::sign`, which is the
same function as the link test's (`ac_i1_2_core_refuses_a_returned_signature_over_other_bytes`). Each was proved to fail by
dropping the trigger; the in-flight test by reverting the gate. `e2e/criteria/exceptions.toml` holds no `AC-I1.2` entry, so
there was none to remove: the map was satisfied by tests that named the criterion and verified only the alert.

## Open — these block `M-1` (real keys)

- **The guards check what the intent declares, not the bytes** (review S1; issue #23). `message` is signed opaque;
  `amount`, `from` and `purpose` are the core's claims beside it. A wrong or compromised core can record
  `{purpose: "mint", amount: "0", message: <a destroy from a user account>}` and every second-line guard passes. Until
  the endpoints (`F-007`, `F-009`) decode the payload and the signer checks the decoded call against the declared
  fields, the guards must not be described as enforcing `DEC-3`. Also: the window cap sums a key's signatures across
  every network and asset it is bound to, adding minor units of different decimals; `REQ-KY-3` is per asset and
  window, so the sum must be keyed by (key, network, asset), the asset taken from the intent.
- **`AC-I4.2` rests on a configuration label** (review S2; issue #25). "Production" is whatever the configuration
  says about a network id: `{"id": "polygon", "environment": "test"}` lets a test key sign a chain-id-137
  transaction. The test-only backend needs a built-in deny-list of production chain ids and genesis hashes
  (Polygon 137, Base 8453, Ethereum 1, Kreivo's genesis) that no configuration can lift; once the payload is decoded,
  the chain id or genesis inside the bytes must match the declared network.
- **One signature per intent and per reference, forever, against retries** (review S6; issue #24). `REQ-EO-1`
  allows a second signature once the first is proven not executed, and `signed_transactions` already keys on
  (`intent_id`, `attempt`); `transfers.on_chain_reference` is write-once. With migration 0012 as it is, a delivery
  whose first attempt is proven not executed can never be signed again. Amendment 0009 (proposed) keys it by
  (reference, attempt); it is not ruled, so nothing is implemented. Decide with the 0009 ruling, before 0012 reaches a
  database that persists: then `attempt` on intents or signatures, both unique indexes on (…, attempt), the replay
  lookup by (`intent_id`, `attempt`), the cap counting a reference once across its attempts, and the `F-003` rule that
  attempt n+1 is signed only once attempt n's non-execution is recorded.
- Review notes not taken here: N1 (`SET search_path` on `signatures_from_intent`), N2 (the trigger should force
  `signed_at := now()`; the signer can backdate a row out of every cap window), N3 (socket directory owner and mode;
  `fchmod` or a umask before `bind`), N4 (refuse the SHA-256 of the empty token), N6 (the compromise path runs in the
  core and nothing calls it yet; `F-007`, `F-009`, `F-014`), N7 (who BLAKE2-hashes an sr25519 payload over 256 bytes;
  before `F-007`), N9 (the test fixture never closes the signer's pool). Also from S4: `mlock` for key pages, and
  refusing a key file that is group- or other-readable or owned by another uid.
- `M-1`: the AWS KMS backend for delivery keys and Vault for the rest (`DEC-11`), behind `KeyBackend`.

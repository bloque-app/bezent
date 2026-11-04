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
    the core's default) the core pages once, durably (`late-mint-prover-silent`, deduplicated by the observation),
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
bezent MUST pause. Recovery is in `deploy/compromise-pause.md`; what the specification leaves open is in amendment 0022
(proposed, not built).

### 1. What stops, and what cannot be stopped

| Layer | Under a global row in `pauses` |
|---|---|
| Ingress | Refuses new requests (`ERR-RoutePaused`, `intake.rs`, the `NULL`-matches-all test) on the next request |
| Core | `gate` reads `pauses` on every step: before each phase of a first attempt (the transfer stays `Source-Final` until a signature is ready), a new attempt, a resolution. It records no further order or signing intent. So the signer has nothing new to sign (`INV-9`) |
| Core, an attempt already signed | Passes, under any pause, a person's or the reserve check's too: it is in flight (`REQ-GV-1`, `INV-15`). Before #77 the gate held it `Source-Final` with `CND-Paused` while its transaction was on chain; the auto-pause would have made that common |
| Refund mint, refund payout, cancel, statement | Keep being ordered and signed, by design (`t.route()` is `None`; F-006 NOTES 1; `REQ-NX-4`; `REQ-PR-1`). "Stops new signing" (issue #77) means new delivery signing |
| Endpoint and signer | Read no pause. **A signing intent recorded before the pause is still signed and submitted**, at the next poll, or whenever a stopped endpoint returns. On Kreivo for at most one era (`MAX_ERA`, 64 blocks) from its preparation: past it nothing is signed (`req_hn_3_a_prepared_transaction_whose_era_passed_is_never_signed`). **On an EVM network with no bound**: nothing expires (`NO_EXPIRY`). Whether that is new work is not settled: amendment 0022, A1. `REQ-GV-1` and `INV-15` forbid stopping more than new work, so it is not built |
| Already-signed transactions | Stored bytes are submitted and resent: in flight, and unstoppable anyway once anyone holds them. On Kreivo an era ends them; on an EVM network only the nonce does |
| On-chain component, vault | **Not touched.** Their pause is a separate act of a separate role (`REQ-HN-9`, §7.F, `AC-I2.*`), a person's today: runbook step 1; amendment 0022, A2 |

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

A service **cannot use this as a free switch beyond what it already had**. The ingress, the webhook dispatcher and the
operators' logins cannot write an alert (0020, 0046, 0049). A login of a detector can write a false one, with its role
stamped on it, in the log forever, and that pauses the bridge: an outage, not a loss, and the same login could already
send a person running by the same act. It cannot name a scope (`detector` is the store's stamp, so only the core has the
`asset` exception), lift a pause or edit one. `REQ-AR-3` and the key rules are untouched: no key near the store, nothing
new in the signer process.

### 3. The bound

- **Detection to pause: none. The alert's commit is the pause's.** It needs no core and no `check.tick`, and it holds with
  every other service down (test `ac_i1_2_the_core_down_at_detection_the_pause_holds_when_it_returns`). If the store is
  down nothing is recorded, but then nothing can be signed either: the signer releases a signature only once recorded
  (`INV-9`), and the detector raises again at its next look.
- **Pause to no new signing intent: the next `pauses` read of the next step**, at `READ COMMITTED`; the core loop is
  sequential, so at most one step already past its gate at the commit records one more intent. Inside `NFR-8`.
- **Not bounded by this:** the signing intents of 1 above: one era on Kreivo, unbounded on an EVM network. So issue #77's
  "stops new signing within a stated bound" holds in full for what the pause stops, for intents recorded after it, and for
  Kreivo's recorded intents within an era; it holds on an EVM network only for intents recorded after the pause, until A1 is
  ruled.
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
| EVM `submit.rs` `lane_runs_code` | The lane holds code (EIP-7702) | **One provider's word**: any code on either counts | **Flag** |
| EVM `submit.rs` `lane_use` | The lane's nonce is past every nonce signed plus one | **One provider's word**: `latest_nonce` takes the higher of two, right for picking a nonce, wrong for an alarm. Also a restored store | **Flag: the one I most want hardened** |
| EVM `submit.rs` signer-key | The answer is not the lane key's | A half-done rotation | Yes |
| Kreivo `proof.rs` `raise_once` | A nonce consumed by an extrinsic it cannot decode, or by none | **A runtime upgrade that changes the extrinsic format makes bezent's own extrinsics undecodable** | **Flag** |
| Kreivo `submit.rs` `signer_mismatch` | The answer is not the lane key's | A half-done rotation | Yes |
| Kreivo `submit.rs` `check_idle_use` | The nonce at the finalized head is past every nonce signed plus one | A restored store | Yes |
| Core, `late_mint.rs` | A final mint of a reference after its proof, or with nothing signed | A restored store; the prover-behind case was removed in PR #72 | Yes |
| Core, `reserve/check.rs` `supply_breach` (`INV-4`, not through `raise`) | The contract's count and the issuance differ | Unchanged: it pauses the asset under 0040, the trigger leaves it alone | Yes, the asset |

Hardening the flagged ones (ask both providers before alarming, read the block from both, recognise an extrinsic bezent
recorded by hash before calling it undecodable) changes when they fire, not the spec; it is not in this change.

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

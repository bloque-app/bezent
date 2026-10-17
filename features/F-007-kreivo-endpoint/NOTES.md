<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-007 — notes

Decisions taken building `T-007-01` to `T-007-04` (implementer, 2026-10-13). Each is a reading of the spec and
plan, not a ruling; the ones marked **for review** are where the plan could be read another way.

## Decisions

1. **Two intents per delivery, both the core's (for review).** The plan says "for a recorded intent, build the
   extrinsic … record the exact bytes, get the signature". F-005 settled that only the core records intents
   (`INV-9`, review S5) and that the signer signs the intent's `message` as it is (F-005 NOTES, decision 2). The
   endpoint builds the message (only it reads the chain), so it cannot be in the first intent. The flow is:
   the core records an **order** (`mint` or `refund-mint`, the reference, `{"order": {…}, "amount"}`, no
   `message`, so the signer refuses it); the endpoint **prepares** it (call, lane nonce, era born at the
   finalized head, dry run) into `prepared_transactions`; the core records the **signing intent**
   (`submit::signing_intent_payload`: `message`, `amount`, `prepared`); the endpoint asks for its signature,
   persists the signed extrinsic, submits. The core's two steps are `F-003`'s; the tests play them as the
   `bezent_core` role. The endpoint never gains `bezent_core`.
2. **The endpoint checks the bytes against the order before asking for a signature.** The signing intent must
   name a prepared transaction whose dry run passed and whose era is live, carry exactly its message, and that
   message must be its payload's; the call must decode to the contract message the order asks for, to the
   configured contract, with value zero; amount, purpose and reference must be the order's. This is the
   endpoint half of issue #23 (decode the payload) for Kreivo; the signer's own decoding is still open there.
3. **The endpoint hashes payloads over 256 bytes** (F-005 NOTES, N7): `message` is the BLAKE2b-256 of the
   signing payload when the payload is longer, as `sp_runtime` signs. A mint's payload is near that length.
4. **The transaction is a signed v4 extrinsic from the delivery key's own account**, with Kreivo 0.17.1's
   extensions (`PassAuthenticate: None` first; spec 134, transaction version 13, read from the runtime at
   `87f8728`). A runtime of another `spec_version` is refused at preparation, not guessed at. The format is
   written from Kreivo's source; it is first exercised against a real node in `T-013-05`.
5. **One attempt per order, plus fresh eras.** A failed dry run is recorded and the order is not prepared again:
   retrying is the core's (a new order). An order whose prepared transaction expired *unsigned* is prepared
   again (attempt n+1); nothing could have executed. A signed one blocks every new attempt for its reference
   until a non-execution proof is recorded (`REQ-EO-1`). Today the signer still refuses a second signature
   for the same reference (issue #24, amendment 0009 unruled), so a retry after non-execution cannot be signed
   yet; nothing here works around that.
6. **Observations are recorded only from final blocks**, so each one gets its `final` outcome in the same
   transaction; `REQ-OB-4`'s re-checking before finality does not arise for this endpoint.
7. **Gaps live in the store** (migration `0018_kreivo_endpoint`): `observation_ranges` (append-only) with views
   `observation_watermarks` and `observation_gaps`. A trigger refuses a read range that starts past the
   watermark, so moving past a gap is impossible at the store layer, whatever the code does. An event
   sequence that jumps, an event that does not decode, a block that cannot be read, or a backlog longer than
   `max_walk` is a gap; the same open gap is not recorded twice.
8. **Signature topics are computed, not read from metadata**: ink! 5 hashes `Name(type,…)` with the field
   types as the contract source writes them (`EVENT_SIGNATURES` in `src/kreivo.rs`). A change to an event's
   fields in `contracts/bezent` must change that table; an unknown topic is a gap, never a guess.
9. **Non-execution by nonce.** Inclusion consumes the sender's nonce whatever the dispatch result, so: nonce at
   the final block not past `N` and the era ended there, not executed; consumed at block `X` by a transaction
   that is not this one, not executed; included and refused, not executed once final (`REQ-NX-1`). The
   consuming block is found by walking parent hashes back from the finalized head and bisecting the nonce.
   Inclusions are recorded as observations of kind `transaction` (`REQ-OB-6`).
10. **AC-I1.2 from chain data.** When another transaction of the delivery key consumed a lane nonce, it is
    decoded and handed to `bezent_signer::compromise::check` (F-005 review N6): one that answers no intent
    bezent recorded appends `key-compromise` and the global `pause-requested`.
11. **The lane** (`REQ-EO-4`): one per delivery key, every mint and refund mint of the network in it, in nonce
    order. Stated plainly: one unknown transaction holds every later one for up to its era (64 blocks, about six
    and a half minutes), since nonces are consecutive. `REQ-EO-4`'s SHOULD is met only within that bound; more
    lanes would need more delivery keys, which the contract's single delivery role does not allow.
12. **Light client only**: subxt 0.44 over smoldot-light 2.1 from aledgent's fork (`[patch.crates-io]`,
    aledgent#121), behind the default feature `light-client`. No RPC fallback was added. Every test runs against
    `chain::fake::FakeChain`; nothing reaches a network. The pinned chain spec is Kreivo's light spec, copied
    from aledgent's canary; the relay chain's spec is a deployment file (`relay_chain_spec`), not committed.

## Review fixes (PR #31, security review of `58dab73`)

13. **M1: the nonce's consumer is identified before anything is proven** (supersedes the "different
    transaction" part of 9 and 10). At the consuming block `X`, the extrinsic from the lane account with nonce
    `N` is decoded. The same signed payload (sender, era, nonce, tip, call), whatever its signature, is this
    transaction: its result is ours (`Executed` or `Refused`), and a copy whose signature the signer did not
    record also starts the compromise path. Another payload proves `nonce-consumed-by-other` only when its
    call does not carry this reference to the contract and the intent its signature answers (if any) is not
    for this reference; otherwise the outcome stays unknown and a `delivery-anomaly` is recorded (`INV-8`).
    No decodable lane extrinsic in `X` is unknown plus the compromise path. The compromise check runs before
    the proof is written; observation and proof are one transaction; a store error fails the step and the
    next one looks again. A store index allows one signing intent per prepared transaction (migration `0019_endpoint_role`).
14. **M2: the whole signing payload is decoded before a signature** (`Submitter::check_payload`): call (the
    order's message, value zero, the recorded dry run's limits, a storage deposit limit), era (mortal,
    `MAX_ERA`, born at the dry-run block, a block of the finalized chain at most `MAX_ERA` back), nonce (the
    row's, not consumed, every nonce below it held by a live prepared transaction of the lane, no other live
    signed transaction at it), no tip, no `PassAuthenticate`, no fee asset, a known spec and its transaction
    version (the finalized head's), the network's genesis; then the payload rebuilt from all of that must be
    the same bytes. The prover takes nonce and era from the persisted extrinsic and the birth block from the
    prepared payload (whose signature by the lane key it checks) and the chain; rows that disagree are an
    anomaly and prove nothing. "The lane's expected nonce" is read as above, so a later nonce may be signed
    while an earlier prepared one is unsigned (`req_eo_4` keeps that); a gap is refused.
15. **S1: `bezent_endpoint`** (migration `0019_endpoint_role`), a member of `bezent_service`, alone may insert proofs, prepared
    transactions and read ranges. The endpoint's login is a member of `bezent_endpoint`; the ingress and the
    core are not.
16. **S2:** the proof trigger also requires signer, nonce and expiry to be the prepared transaction's.
17. **S3, S4: signals are rows, written once** ([`signal`](../../services/kreivo-endpoint/src/signal.rs)):
    `key-compromise` plus `pause-requested` through `bezent_signer::compromise` (its `raise` is now public),
    or `delivery-anomaly` for bytes that do not answer their rows; deduplicated by coordinate or key, never
    only a log line. The signer answering with another key, or a signature that is not the lane key's, starts
    the compromise path. At preparation, a chain nonce past every nonce bezent signed (the key used while the
    lane was idle) starts it too.
18. **S5:** `relay_genesis` is declared with `relay_chain_spec`; a chain spec that names a relay chain needs
    both, the relay spec's genesis must be the declared one and its `id` the chain spec's `relay_chain`, or the
    start fails closed (`--check` too). The fixtures carry Kusama's identity only.
19. **Note, max_walk gap:** its reason no longer carries a block count, so the same open gap is recorded once.

## Second review fixes (PR #31, verification review of `e0ef566`)

20. **M1-bis: a consumer bezent did not sign is never proof** (supersedes the "another payload" part of 13).
    The nonce's consumer proves `nonce-consumed-by-other` only when it is a recognised bezent transaction: its
    signature answers an intent bezent recorded, its call is a direct `Contracts::call` of `mint` or
    `refund_mint` to the contract for that intent's reference, and that reference is not this one. A consumer
    whose signature answers nothing is unknown (the compromise path is raised as before): a wrapper such as
    `Utility::batch`, `batch_all`, `force_batch`, `Proxy::proxy`, a multisig or `dispatch_as` can carry this
    reference with the lane as origin, and no wrapper is read into. A recorded signature over anything else is
    unknown plus a `delivery-anomaly`. A re-signed copy bezent did not sign still counts when it executed, but
    its refusal proves nothing. "Our reference" is decoded from the signed call and must be the intent's.
21. **The contract agrees before any proof** (closes that item of issue #33). Every `NotExecuted` is checked
    against the contract at the proof's final block through `ContractsApi_call`: `mint_reference_used(reference)`
    for a mint, `redemption(redeemer, reference)`'s `refunded` flag for a refund mint (a redemption the contract
    does not hold was never refunded; it never removes one). A used reference writes no proof and records a
    `delivery-anomaly`; an answer that fails or does not decode is unknown. It catches a mint at another nonce and
    calls that never consume the lane's nonce. `FakeChain` now keeps the contract's state per block and answers
    both getters.
22. **S-a, S-b: who writes signals and the issuing network's observations** (migration `0020_signal_writers`). An `event_log` row of kind `key-compromise`, `pause-requested` or
    `delivery-anomaly` is appended only by a login holding `bezent_endpoint` or `bezent_core`, so the ingress
    cannot write one in advance and suppress the real signal. Observations and outcomes of any network that is
    not a declared origin network are inserted only by a login holding `bezent_endpoint`, so the ingress cannot
    forge the `transaction` observation that silences the prover. The migration owner keeps both. No grant was
    added: the core's login holds `bezent_service` and `bezent_core`, which the signer's tests now model with a
    test role (`core_login_for_tests`, as F-003's tests do); the link test's `SET ROLE` became `SET LOCAL ROLE`
    in a transaction (#27).
23. **S-c, S-d: the relay checkpoint and the `para_id` are pinned** (`AC-I4.1`). `relay_checkpoint` is the
    BLAKE2b-256 of the canonical JSON (keys sorted, no whitespace) of the relay spec's `checkpoint` and
    `lightSyncState` fields, whichever it has: smoldot warp-syncs from it and trusts its GRANDPA set. It is
    required when the relay spec has one and refused when it has none (the genesis is then the trust root); a
    mismatch prints the spec's hash, so an operator pins it from `--check`. `para_id` is required with
    `relay_chain_spec`, and the chain spec's `para_id` must equal it. Either mismatch exits 1 before anything is
    read, `--check` included. The fixtures declare `para_id = 2281`; the identity fixture has no checkpoint.

## Integration with F-003 (after PR #32 merged)

24. **Numbering.** PR #32 merged first with `0014_lifecycle`, `0015_lifecycle_review` and
    `0017_lifecycle_verification`; this feature's migrations follow them in their own order:
    `0018_kreivo_endpoint`, `0019_endpoint_role`, `0020_signal_writers`. None runs before 0017 on a
    fresh database. `0015` already makes `bezent_endpoint` the way `0019` does (`IF NOT EXISTS`), so
    both apply in either state.
25. **The core reads the real tables** (migration `0021_delivery_integration`). `attempt_proof` and
    `attempt_gate_open` read `non_execution_proofs` directly instead of through `to_regclass` and
    dynamic SQL, with 0017's behaviour, owner and grants; the core's engine no longer checks that
    `prepared_transactions` exists. F-003's test-only copy of the two tables is removed; the store,
    signer and core tests now record the order and a prepared transaction before a signing intent,
    so their proofs pass `0019`'s guard (S2).
26. **End to end** (`tests/delivery.rs`): the core service records the order, the endpoint prepares
    it on the in-memory chain, the core records the signing intent over the recorded bytes, the
    endpoint has it signed over the signer's link, checks it, persists and submits it, and the core
    verifies the signature and moves the transfer to `Delivering` on that record. Each step on its
    own role: the core's login, `bezent_endpoint`, `bezent_signer`.
27. **Grants** (issues #33, #35, part). Only `bezent_endpoint` inserts `signed_transactions`,
    `observations` and `observation_outcomes`, for every network; the ingress and the core no
    longer can (0020's issuing-network guard stays, as a second check). No F-007 function is
    PUBLIC's (0017's F1): the trigger functions go to no role, `bezent_writer_holds` to
    `bezent_service`, as their triggers run it as the inserting login. Still open: the ingress's
    own tables (`intake`, `intake_answers`, `relayed_instructions`) and `transfers`/`transitions`
    are writable by every `bezent_service` member, since there is no ingress role yet; one
    endpoint role serves every network, so the Kreivo endpoint's login could write an origin
    network's observations (one role per endpoint when F-009 lands); `event_log` stays open to every
    service; the core's own role is not separated from `bezent_service` for reading.

## Left from the review (each over the ~30-line budget, or outside this feature)

- **Contract code hash at start-up** (S5): the `Chain` trait has no storage read for `ContractInfoOf`.
- **S1 follow-up** (issue #33): done for `signed_transactions` and observations (27); per-endpoint roles remain.
- **The core** (`F-003`) must surface `delivery-anomaly` rows (to `Needs-Review`); the endpoint only records.
- **A gap longer than `max_walk`** stays closed and stalls the follower until `max_walk` is raised.
- **A nonce consumed before the era's birth** stays unknown. M2 makes it unreachable for anything signed now
  (at signing the nonce is at least the chain's at a finalized block at or after the birth).
- **The bisect reads state** up to the era's age; pruned peers make it unknown. Scanning block bodies in the
  era window for (lane, `N`) would not need old state.
- **`T-013-05`** should assert that `tx_hash` matches the body item `chain_getBlock` returns: `FakeChain`
  hashes the way it encodes.

## Limits stated plainly

- **History**: a light client reads old state from peers that keep it; most nodes prune state after 256
  blocks. A backlog older than that stays a visible gap until a peer serves it (`REQ-RN-6`), and a proof
  whose blocks are pruned stays unknown.
- **Runtime upgrades**: events are decoded with the metadata subxt holds; an old block of an earlier runtime
  may not decode, and becomes a gap.
- **The light-client code path is compiled in CI but exercised only by `T-013-05`** (zombienet).

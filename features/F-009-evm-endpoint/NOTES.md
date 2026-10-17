<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-009 — notes

Decisions taken building `T-009-01` to `T-009-06`, Polygon only (implementer, 2026-10-14). Each is a reading of
the spec and the plan, not a ruling; the ones marked **for review** could be read another way. Base and BNB Chain
are configuration plus their fork tests (`M-2`).

## Where it stands

- **Built on PR #31** (F-007, merged while this was written): the endpoint writes F-007's tables
  (`observation_ranges`, `prepared_transactions`, `non_execution_proofs`) and needs `bezent_endpoint` (0019) and
  0021's grants.
- The partial work left by the stopped helper was reused after review: `rpc.rs` (provider trait, HTTP client, parsers),
  `rpc/mock.rs`, `pair.rs` (cross-checked reads, `DEC-12`), `network.rs` (identity and declarations), `config.rs`,
  `evm.rs` (delivery call, strict decoding, EIP-1559 payloads) and the fixtures. The pinned hash of block 95 400 000
  in `fixtures/endpoint.toml` was re-checked on the fork. `observe.rs`, `submit.rs`, `proof.rs`, `signal.rs`,
  `store.rs`, `main.rs`, the migration and every test are new.

## Decisions

1. **Its own role (for review)** — migration `0035_evm_endpoint_role` (numbered 0022 until PR #44 merged 0034). `bezent_evm_endpoint` is a member of
   `bezent_endpoint` `WITH INHERIT TRUE, SET FALSE` (so it has 0015, 0019 and 0021's grants, and cannot `SET ROLE`
   into it; review M1) and a trigger lets a login holding it, asked of `session_user` as well as `current_user`, write
   the records of EVM networks only (`networks.address_format = 'evm'`): observations, outcomes, ranges, final heads,
   prepared and signed transactions, proofs. It cannot write the issuing network's observations or proofs (`INV-8`).
   The other direction (the Kreivo endpoint writing an EVM network's records) needs a Kreivo role of its own; that is
   F-007's (its NOTES 27). The migration owner and a superuser write everything (`NFR-7`).
2. **Observations only from final blocks** (as F-007 decision 6). The final point is the lower of the two providers'
   `finalized` heads, with the hash both give there (`REQ-OB-1`: the network's own finality, Heimdall milestones on
   Polygon PoS). Every log's block hash must be the one both give for its height. An observation is recorded with
   its `final` outcome, and the final point in `final_heads` (the core requires it to treat a source as final); a
   final head with another hash at the same height stops everything. `AC-A2.1` and `AC-A2.3` hold because nothing
   pre-final is recorded; `REQ-OB-4`'s re-checks do not arise.
3. **Gaps** (`REQ-OB-3`, `AC-D2.3`): a range that fails (a provider error, the providers disagreeing on its logs or
   hashes, a removed log) is a gap in `observation_ranges`, recorded once, and read again next step.
4. **What is an observation** (the table in `src/observe.rs`): `Arrival`, `Conversion`/`ConversionInput`,
   `ConvertibleArrival`, `OtherArrival`, `Payout`, `Outflow`, `ZeroTransfer`, `SelfTransfer`, `UnpairedReference`,
   `UndecodedTransfer`. A `Transfer` that does not decode as ERC-20, or an amount above `u128`, is **recorded, never a
   gap**: anyone can send the vault such a token, and a gap would stop observation for good.
5. **Payout pairing** (F-008 NOTES, #9 F4): a `Reference` is a payout only when the log right before it, in the same
   transaction, is the vault's `Transfer` of an admitted origin token, above zero, to an address other than the vault.
   Recipient and amount come from that transfer. A paired `Reference` is not recorded on its own.
6. **Conversions** are matched by transaction, never by amount or time: in one transaction the vault pays a declared
   pool its convertible and that pool pays the vault the origin token. The output is `Conversion` with both amounts.
7. **A self transfer is never bindable**: its `recipient` column is empty (the address is in the detail), because the
   ingress binds a deposit to any observation with an admitted token received at the reserve address, whatever its
   event. See "Found" below.
8. **Prepared rows on an EVM network**: `valid_from_block` is the dry-run block, `valid_until_block` is `i64::MAX` (no
   expiry, `REQ-NX-1`), `checkpoint_hash` the dry-run block's hash, `call` the target followed by the calldata,
   `message = signing_payload = 0x02 ‖ rlp(fields)`; the signer signs its Keccak-256 (`secp256k1-evm`). There is never
   a `nonce-unconsumed` proof here.
9. **Preparation**: gas is the higher of the two estimates times the margin, bounded by `max_gas`; fees are twice the
   higher base fee plus the higher priority fee, capped by `max_fee_per_gas`; the lane must hold the fee. A failure is
   a failed dry run with the network's words (`fee: …`, `InsufficientBalance … fee`, `Roles: ConditionViolation(…)`).
   One preparation per order; a retry is the core's (a new order).
10. **Decode-and-compare** (`Submitter::check`): chain id, nonce, value, access list, target, calldata, gas limit and
    fees (the recorded dry run's, within bounds), and the delivery itself (role key, MultiSendCallOnly, token,
    beneficiary, amount, reference) against the order; the intent's network, purpose, reference, transfer, attempt and
    amount against the order's. A mismatch signs nothing and records a `delivery-anomaly`.
11. **The lane** (`REQ-EO-4`): one per delivery key. The next nonce is the lowest at or above the chain's (the higher
    of the two latest) that no prepared transaction with a passing dry run holds. Stated plainly: one unknown payout,
    or one prepared and never signed, holds every later payout until it executes or a cancel consumes its nonce.
12. **Cancels** (`REQ-NX-4`, **for review**): the core orders one as an intent of purpose `cancel`, payload
    `{"order": {"cancel": <signed transaction id>}, "amount": "0"}`, no reference and no `transfer_id` (so the core's
    per-attempt queries never count it). The endpoint prepares a transfer of nothing from the delivery key to itself
    at the payout's nonce, its fees above the payout's by an eighth. The signer's binding for the delivery key must
    list `cancel`. The signer signs a `cancel` only when its bytes are an EIP-1559 transfer of nothing from the key to
    itself, with no calldata and no access list (review S6). No cancel is prepared or signed while the lane holds code
    (an EIP-7702 delegation would run): that starts the compromise path (review S5).
13. **Proofs** (`src/proof.rs`): the consuming block is found by bisecting the lane's nonce from the dry-run block to
    the final point, and the lane's transaction at that nonce is identified: the same signed transaction (executed, or
    a final refusal: `refused`); a re-signed copy (executed counts; refused proves nothing); a wrapped copy (any other
    payload carrying the reference: unknown, and the compromise path when bezent did not sign it, review S2; a
    `delivery-anomaly` when it did); a recognised bezent transaction (a recorded signature over exactly its payload,
    another reference, a delivery or a cancel: `nonce-consumed-by-other`); anything else is unknown plus the compromise
    path. Before any proof the vault must be read to the final point and no observation may show the vault paying the
    reference (the EVM analogue of F-007 NOTES 21): a `Payout`, or a reference in a transaction where the vault's own
    transfer moved value. A bare emit, which anyone can make, never blocks a proof (review S1).
14. **Start-up** (`AC-I4.1`, review S4): besides chain id, pinned hash and the store's declarations, both providers at
    the final point must show the configured Roles modifier's `avatar()` and `target()` as the vault and the vault's
    `isModuleEnabled(roles)` true, and the delivery key must hold no code. Anything else fails closed.

## Found

- **EIP-7702** (for `F-014` and the signer's guards): Foundry's account 4, the test lane, carries a 7702 delegation on
  Polygon at the pinned block, so a transfer of nothing to itself runs code (the first fork run's cancel was refused
  for gas). A 7702 authorisation signed with the delivery key also consumes its nonce **with no transaction from it**;
  the prover then finds no lane transaction at the nonce and stays unknown, with the compromise path. The fork tests
  clear the delegation (`anvil_setCode`), as bezent's own key has none.
- **The ingress binds by fields, not by event** (`services/ingress/src/intake.rs`, `arrival`): any executed observation
  with an admitted token and the reserve as recipient can back a deposit. Decision 7 keeps self transfers out; the
  ingress (or the core) should also bind only `Arrival` and `Conversion`. Follow-up for F-002/F-003.

## Left

- **Core**: since issue #41 the core orders the cancel at the deadline and drives it while the transfer is in review
  (F-003 NOTES 18-23). A payout refused by the vault's allowance holds `CND-PayoutWindow`, named only from the
  structured `dry_run.violation = {status, key}` this endpoint records when both providers returned the same
  `ConditionViolation(uint8, bytes32)` revert data (PR #52 review S-2; `evm::condition_violation`,
  `DryRun::violation`). The reason keeps each provider's words, for people only.
- **The fork tests read one anvil through two URLs**: the providers' independence is shown by the mock tests only.
- **Base and BNB Chain** (`M-2`): configuration, their fork tests, and BNB Chain's finality tag.

## Review of PR #39: left for later

Fixed in the PR: M1, S1, S2, S4, S5, S6 (each has a test named after its id). Not fixed here:

- **S3, the providers' independence beyond the file path**: two `url_file`s may still hold the same URL, or the same
  vendor under two keys. Refuse a pair with the same host after reading the URLs, and better, declare a vendor per
  provider and refuse two of one vendor (`DEC-12`; `F-014` holds the production keys).
- **S7, the role**: one `bezent_evm_endpoint` serves every EVM network, so a compromised Polygon instance can write
  Base or BNB Chain records. It needs a role per EVM network (or a login-to-network table the guard reads) before
  `M-2`. The role also inherits `bezent_service` through `bezent_endpoint` (INSERT on `transfers` and the intake
  tables); give it its own grants instead. At start-up the endpoint should confirm its login holds
  `bezent_evm_endpoint` and is no superuser, and fail closed otherwise.
- **S4, the rest**: the emitter and MultiSendCallOnly are not checked against expected code hashes at start-up.
- **S6, the rest** (F-005 follow-up): the signer does not decode a payout to check that its amount is the intent's
  `amount`.
- **N1** compare the per-provider `finalized` hashes with the cross-checked header at that height; **N2** pass the
  follower's head to the prover; **N3** split a range that keeps failing, and de-duplicate gap rows by range, not by
  reason text; **N4** a zero `Transfer` into the vault is an `Arrival` of 0 (classify as `ZeroTransfer`, recipient
  empty); **N5** an origin-token `Outflow` with no reference breaks `INV-14` and is only recorded (raise an anomaly,
  or F-004 reconciles it); **N6** admission (`REQ-AS-7`) should exclude fee-on-transfer and rebasing tokens; **N7**
  the bisection's `hi - lo` should use checked arithmetic; **N8** a cancel capped at `max_fee_per_gas` cannot replace
  a payout already at the cap; **N9** document how a rotated-out lane's live transactions are drained.

### Second review of PR #39

- **MF1, #38**: migration `0036_delivery_pairing` pairs a delivery's two steps in the store: a prepared transaction
  answers an order on that order's network (the EVM endpoint prepares EVM orders only, any other endpoint never one);
  a signing intent names a prepared transaction of its own order (network, transfer, attempt, purpose, reference,
  scope). The owner's session is exempt from the intent check, because a restore loads `intents` before
  `prepared_transactions` (NFR-7). `0035` maps a prepared row to its order's network and pins its guards'
  `search_path`. The core and both endpoints read only preparations on the order's own network, and the signer binds
  an EVM key only to a network that declares a `chain_id`, a Kreivo key only to one that does not.
- **SF1**: before each preparation and signature the endpoint raises the compromise path when the lane's nonce is past
  every nonce bezent signed plus one, or the lane holds code. A lane bezent never signed for raises nothing on its
  nonce (as the Kreivo endpoint's S4).
- **SF2**: the signer signs a cancel only on its network's `chain_id`, with gas 21000 and fees within the network's
  `cancel_max_fee_per_gas`.
- **0034 (PR #44)**: the endpoint writes `observations.kind` (`observe::kind`; `transaction` for its signed
  transactions, whose detail no longer names an `event`), and `0035` registers its scope (`bezent_evm_endpoint`,
  origin, `evm`). **Deployment:** the EVM endpoint's login must hold `bezent_evm_endpoint`, each EVM network be
  declared with `role = 'origin'` and `address_format = 'evm'`, and the signer's EVM networks declare `chain_id` (and
  `cancel_max_fee_per_gas` where `cancel` is bound), or the endpoint records nothing and the signer refuses to start.

### Third review of PR #39

- **MF-A, #38**: migration `0037_signing_intent_bytes`: any intent carrying `message` or `prepared` must name a
  prepared transaction whose dry run passed, of its own order (as 0036), carry exactly its `message` and its order's
  `amount`. The signer reads the named prepared transaction itself (it now holds `SELECT` on `prepared_transactions`)
  and refuses (`Refusal::NotPrepared`) unless bytes, amount, order and network all match.
- **SF-A**: no session is exempt from that check any more, the owner's and a superuser's included. Plain `pg_dump`
  creates triggers after the data, so the restore drill (which restores signing intents) needs no exemption.
- **SF-B**: an EVM key, by key id and by public key, binds to one network only: the signer refuses such a
  configuration, and `0038_evm_key_one_network` refuses a signature (or an EVM prepared transaction's lane) on a
  second network. The signer reads every EVM intent's chain id (`Refusal::ChainId`); the endpoint already compares
  the payload's chain id with its network's.
- Not done here: the reviewer's suggested start-up login check for the core and both endpoints (no superuser, no
  table owner), and N4 and the zero-value `paid` note on observation kinds.

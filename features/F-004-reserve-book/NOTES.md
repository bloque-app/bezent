<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-004 — notes

Built `T-004-01` to `T-004-05` (implementer, 2026-10-14): migration `0030_reserve_check`, the module
`services/core/src/reserve/` (`book`, `readings`, `check`, `statement`), the `ed25519` scheme in the signer's key
trait, and the tests named after the ids they verify. Each choice below is a reading of the spec and plan, not a
ruling. Review fixes of PR #40 (2026-10-14): migration `0031_reserve_check_stalls`, decisions 6, 8, 9, 11, 12 and 14
updated or added, and the review items left open listed at the end. Verification fixes (2026-10-14, second round):
migration `0032_reserve_check_verification`, decisions 15 to 19.

## Decisions

1. **Migration 0030.** F-009 (branch `feat/F-009-evm-endpoint`) is built at the same time and may add migrations
   from 0022; this one is numbered 0030 so neither renumbers. Whichever PR merges second renumbers if they meet.
   Additive only: columns on `book_entries`, `book_postings`, `assets` and `pauses`, two new append-only tables,
   two trigger functions (neither PUBLIC's), and grants.
2. **The book is posted from final observations, not from transfer states.** §5.3 counts every final arrival as
   owed mint, bound to a deposit or not, so an unassociated arrival must be in the book before any transfer binds
   it. Each movement rests on its observation's own block (`book_entries.network`, `block_number`, `block_hash`),
   so the check reads the book as of the same final point it reads the network at. Posting runs at the start of
   every core cycle and every evaluation; a movement is posted once (`(kind, reference)` unique, corrections
   aside).
3. **Movements, accounts, items.** The table in `reserve/book.rs`. Reserve and in-transit are debit-normal; supply,
   owed mint, owed payout and surplus credit-normal, so every balanced entry keeps the equation. Owed-mint items
   are the arrival's coordinate; owed-payout items the withdrawal's bridge reference, derived from (redeemer,
   reference) so a redemption is itemised before any withdrawal exists (closes F-001 NOTES N5).
4. **REQ-AS-9.** A mint takes the arrival out of owed mint in full; the remainder below one minor unit of the asset
   is credited to surplus with the deposit's bridge reference as its item. A mint of another amount is booked as
   minted and leaves the rest owed, so the check diverges on that item.
5. **Only the core posts.** INSERT on the book moves from `bezent_service` to `bezent_core`: the ingress, which
   faces the internet, may no longer write the book. Two store tests were updated for it (`signer_role.rs`'s list
   of the core role's grants, `store_rules.rs`'s service-role walk).
6. **The final point per network** is the latest recorded final head at or below both the network's contiguous
   read watermark (`observation_watermarks`, F-007) and the block before any observation still waiting for its
   outcome. Each point records how far, in block time, it lags the network's latest recorded final head; beyond
   `networks.reserve_point_lag_seconds` (default 60; the operator sets one minute plus the network's finality time)
   the evaluation **diverges** with a `final_point` difference, whatever the terms say, so an observation that never
   gets its outcome or a stalled watermark cannot freeze the point (review M2; `NFR-5`, `REQ-IC-2`). A network with
   no read range, a reading or the store that fails, makes the evaluation `incomplete`: recorded, paged once per
   episode, never taken as holding; an episode longer than the smallest bound of the asset's networks pauses the
   asset (`REQ-IC-2`, `reserve-check-stalled`). Each asset is evaluated on its own; a failure is paged
   (`reserve-check-failed`) and the others go on.
7. **What is re-derived from where (REQ-BK-8).** Supply and public reserve balances from `Readings` at that point;
   an attested position from its party's last report; owed mints and owed payouts item by item from the final
   observations, matched by the references bezent writes (mint reference, payout reference, unassociated-refund
   reference, refund mint by (redeemer, reference)), never by amount or time. Value in transit is the empty
   list: bezent sends no value through a venue in V0 (§4.3, `DEF-8`, rebalances withdrawn). Surplus is the balance
   the other terms imply, compared with the book's and required to be ≥ 0 (`REQ-BK-3`).
8. **A divergence pauses through `pauses`.** The core inserts one row per asset (`cause = 'REQ-BK-1'`, naming the
   diverging evaluation; since 0031 also `cause = 'REQ-IC-2'`, naming an evaluation that stayed incomplete beyond
   the bound or a quarantined observation); the store lets a non-owner insert nothing else there, and the core
   cannot delete. The
   ingress already refuses on it (`ERR-RoutePaused`) and the core's gate holds `CND-Paused`. A human is paged once
   while the pause stands (`reserve-divergence` in the event log, which no requester reads, and the `Pager`).
   Lifting the pause is the operator's (delete the row).
9. **REQ-BK-10 alert level.** The spec asks for an alert *before* the floor is reached but declares no level. The
   asset gains `surplus_floor` and `surplus_alert` (above it); the alert fires below `surplus_alert`, a second one
   below the floor. Ruled at plan level (DEC-16, 2026-10-14: each asset declares its alert level beside its floor).
   A floor or alert level left NULL alerts (`surplus-levels-undeclared`, review S6); the check then reads a NULL
   floor as zero and a NULL alert level as the floor.
10. **REQ-BE-6.** A reversal is an attesting party's report (reference `party:report`): surplus debited, the
    position's reserve credited. While a reversal is unrestored and the surplus right after it was below the floor,
    the core's gate holds new deposits of that asset in `CND-Paused` (refund mints, which complete work in flight,
    are not held). A restoration is one report restoring the reversal in full, at the same position; mints resume
    by themselves. A reversal unrestored after one business day (`one_business_day_after`) alerts.
11. **Surplus contributions come from chain data and the operator's declarations, never from the book alone**
    (review S1; `REQ-BK-5`, `REQ-BK-8`). The owner declares the addresses it funds each reserve from
    (`surplus_funding_addresses`, migration 0031, append-only, no service login writes it). A final arrival at the
    reserve from one of them is posted as a `surplus-contribution` (reserve against surplus, its coordinate as
    reference); every other arrival is owed mint, bound to a deposit or not, and no call can turn it into surplus
    (`book::surplus_contribution` is gone). The check derives contributions the same way, from the arrival's sender,
    and the statement publishes the funding addresses (`operator_funding_addresses`) and each contribution's
    coordinate, amount and sender, so a verifier recomputes owed mint from public data (`AC-E1.1`). Listing
    contributions is ruled (DEC-16, 2026-10-14).
12. **The statement.** It states the latest complete evaluation no older than 15 minutes, and lists surplus as
    both what the other terms imply (`amount`) and what the book records (`book_amount`), with every movement of
    surplus as of the evaluation's points (`terms.surplus.movements`: contributions, withdrawals, network costs and
    venue fees, `REQ-AS-9` remainders, reversals, restorations, corrections; DEC-16 and review S2). A publish cut
    short after the document reached the store is adopted on the next attempt (same asset, sequence, predecessor
    and key), never replaced, and `publish_due` goes on per asset (review S7). Adoption signs nothing it did not
    derive (verification SF4): a document whose signature file already verifies with the key is adopted as it is;
    an unsigned one only if its bytes are exactly the statement re-derived from the evaluation and issue time it
    names; anything else is `Stuck`, logged in the event log and paged (`statement-stuck`). Each evaluation records
    the last book entry it read (`book_entry_cut`), and both its book terms and the statement's movements stop there,
    so the movements add up to `book_amount` (verification SF5; an entry with a lower id that commits after the read
    could still slip in, which only a snapshot-consistent read would close: closed by decision 27).
    JSON, pretty-printed; the exact bytes are signed (ed25519) and published with a separate
    signature file, both under `<asset>/<sequence>.json[.sig]` in an append-only `StatementStore` that refuses an
    existing name (`DirectoryStore` in tests; DEC-16's bucket is F-014's). Each names the SHA-256 of the asset's
    previous one; the store refuses a statement that does not name its predecessor, and `verify_chain` detects a
    revision from a later statement alone (`AC-E1.2`). Its fields are in `reserve/statement.rs`.
13. **The statement key.** `Scheme::Ed25519` in the signer's key trait (`ed25519-dalek`, strict verification). The
    signer's configuration refuses to bind an ed25519 key to any network, so it can never sign a transaction;
    `KeyBackendSigner` refuses any other scheme. Tests use a file key.
14. **What the book cannot post never stops anything** (review M1; `INV-1`, `REQ-IC-2`). No entry is built with
    fewer than two non-zero postings: a movement of zero (anyone can send zero of a token to the vault) stays the
    observation it is, posts nothing and is owed nothing. A final observation whose entry the store refuses for good
    (a constraint, a trigger, a value out of range; not a connection or serialisation failure, which is retried) is
    quarantined once (`book_quarantine`, append-only): its asset is paused (`REQ-IC-2`), a human is paged
    (`book-quarantine`), the rest is posted, and the check evaluates the asset at once. Neither `Core::cycle` nor the
    check fails on a posting error any more. The operator resolves a quarantine (a correction entry, then lifting the
    pause). A lock timeout (`55P03`) is passing, tried again, never quarantined.
15. **An operator top-up is never a deposit's source** (verification MF1; `INV-1`, `INV-2`, `REQ-IN-3`). One rule,
    the store's `funding_arrival(asset, network, sender, block)`, says whether an arrival is from a declared funding
    address at or after the declaration's block. The store refuses a deposit binding such an arrival, and Source-Final
    on one (trigger `transfers_source_not_funding`, even for the owner); the ingress answers `ERR-SourceNotFound` (the
    closest existing error: "the coordinate named does not hold a movement bezent can bind"), and the core's engine
    holds any binding made before the rule and logs it. The book and the check apply the same rule
    (`Position::funded_by`), so the top-up is surplus everywhere and nobody gets a mint for it.
16. **The latest final head is measured against the clock** (verification MF2; `NFR-5`, `NFR-6`, `REQ-IC-2`). Each
    point's `Lag` records `head_age_seconds`, the check's clock minus the latest recorded final head's block time; above
    the network's bound the evaluation diverges with a `final_head` difference (pause and page), so heads that stop
    being recorded never leave the check holding. This closes the "observer staleness" item of the first round for the
    reserve check; NFR-6's own observer alert stays with operations (F-014).
17. **A failed evaluation pauses like an incomplete one** (verification SF1; `REQ-IC-2`). It is recorded as
    incomplete, pages `reserve-check-failed` only when the asset's evaluations turn incomplete, and the stall rule
    pauses the asset past the bound. If the store cannot even record it, the check keeps the episode in memory: one
    page, then one `reserve-check-stalled` page past a minute saying the pause could not be recorded.
18. **The lag bound's ceiling** (verification SF2). `networks.reserve_point_lag_seconds` is at most 3600 (a check
    constraint), and setting it above the completion budget of any route the network serves (all routes, for the
    issuing network) is refused. A route's budget lowered later is not re-checked against it.
19. **Funding declarations are never retroactive** (verification SF3). `surplus_funding_addresses.from_block` is set
    at declaration to the block after the latest observed or final head recorded on the network, and an explicit
    earlier block is refused; an arrival already observed keeps its classification. Declarations from before 0032
    have no block and apply to nothing. The column comment says a funding address must be the operator's alone, never
    custodial or shared. The statement publishes each address's `from_block`.
20. **A refund never binds an operator top-up** (third round, SF-A). The `transfers_source_not_funding` trigger also
    covers `kind = 'refund' AND refund_of IS NULL` (a refund of an unassociated arrival, `REQ-RQ-6`), and the ingress
    answers `ERR-SourceNotFound` in `decide_refund`, as for a deposit. Migration 0033.
21. **The lag bound defaults to the finality time plus a minute** (third round, SF-B). `networks.finality_seconds` is
    configured per network; `reserve_point_lag_seconds` has no fixed default now: unset, it is `finality_seconds + 60`,
    at most 3600, and re-derived when only the finality time changes. An explicit bound stays the operator's. The
    values for Polygon, Base and BNB Chain, with their sources, are in `services/core/fixtures/networks.toml`. A network
    left at 0 finalizes within a minute by declaration; the operator MUST set it for any slower one.
22. **The budget rule runs on insert** (third round, SF-C). A network's bound is checked against its routes' budgets on
    insert and when the bound or finality time changes; a route's budget is checked against its network's bound and
    the issuing network's on insert and when the budget or network changes. This closes the open item of decision 18
    (a budget lowered after the bound was set).

## Integration points

- **Readings (T-004-02).** `reserve::readings::Readings` is the interface. Since issue #42 the running core uses
  `readings::Recorded`, which reads what the endpoints record at final heads (`reserve_readings`, migration 0039;
  decisions 23 to 26), and `bezent-core` runs `ReserveCheck::tick(&SystemClock)` and, with `[statement]` configured,
  `Statements::publish_due(now)` after every cycle. Tests still use the fake for everything but those decisions.
- **F-009.** Payout legs must carry `sender` = the vault (the book posts a payout only from the reserve address),
  arrivals `recipient` = the vault, and a conversion's output `detail.event = "Conversion"` (booked as kind
  `conversion`). Convertible holdings come through `Readings::convertible`.
- **F-011 (attested backends).** Swaps' reports of arrivals and payouts are not posted yet; the check of an
  attested asset will diverge until they are. Reversal and restoration are posted (`book::reversal`,
  `book::restoration`).
- **F-014.** The owner declares the operator's funding addresses (`surplus_funding_addresses`) and each network's
  `reserve_point_lag_seconds` (one minute plus its finality time); the operator surface records withdrawals and costs
  through `book::surplus_withdrawal` and `book::cost`, resolves quarantined observations, and lifts a pause.

## Plan-level decisions since (DEC-16, 2026-10-14)

- **The alert level of REQ-BK-10:** each asset declares it beside its floor (decision 9; `surplus_alert`).
- **Who signs the statement:** the signer process, through a recorded intent of purpose `statement`, so the statement
  key lives only behind the signer (`REQ-AR-3`). Built in issue #42 (decision 28); `KeyBackendSigner` (a key in the
  process that holds state) stays for tests only and is never wired in a deployment.
- **Contributions and withdrawals in the statement:** listed (decisions 11, 12).
- **Value in transit:** always empty in V0, since no route leaves value in transit (decision 7).

## Issue #42: the running core (2026-10-15)

Migration `0039_reserve_readings`; `services/kreivo-endpoint/src/supply.rs`, `services/evm-endpoint/src/reserve.rs`,
`reserve::readings::Recorded`, `reserve::statement::SignerProcess`, the signer's `statement` purpose, and `bezent-core`'s
loop. Each choice below is a reading of the spec and plan, not a ruling.

23. **Readings rest on recorded final heads.** An endpoint records, in one transaction with a final head, what it read
    at that head (`reserve_readings`): the store refuses a reading at a block and hash that is not a recorded final
    head of its network (`REQ-OB-5`), a kind its network's role does not produce, and any writer but that network's
    endpoint (0034's scopes, 0035's EVM guard). The check's final point is a recorded final head, so every term is read
    at the one point per network the book is cut at. A point with no reading recorded, or with an unknown one, is a
    `ReadError`: the evaluation is incomplete and the stall rule (decision 17) pauses past the bound.
24. **Kreivo records final heads now.** Before #42 the Kreivo endpoint recorded none, so the check had no issuing-network
    point. After each follow step it records the finalized head (its block time from `Timestamp::Now`) with each bridged
    asset's supply: the contract's `supply(asset)` by a dry run and `pallet-assets`' `Asset::supply` of `Here(index)`,
    both through the light client. The issuance is the reading (§5.3: the total on the issuing network); a count that
    differs is recorded with `mismatch`, committed with the head. An asset the chain does not hold, or a read that
    fails, is unknown. A mismatch is a breach of `INV-4` (PLAN `DEC-16`, 2026-10-15; PR #50 review M1): the core reads
    the durable flag at every evaluation, at any recorded head, not only at its point, so a stop after the endpoint's
    commit loses nothing. Each mismatch event raises `key-compromise` once, scoped to the asset (an event starts at a
    flagged reading whose known predecessor was not flagged, or was flagged with another difference, and the dedupe
    names that reading: the same difference again after the counts agreed is a new event; PR #50 re-check S-2), with the actions pause, rotate, report, and pauses the asset's routes under cause `INV-4`
    (migration 0040: the pause names the flagged reading). While a mismatch stands at the point read, the evaluation
    diverges (term `supply_mismatch`) whatever the book says, so a book correction matching the issuance does not
    make it hold, and an `INV-4` pause the operator lifts is recorded again. The core never lifts it.
25. **EVM balances from both providers.** At each new final head, before recording it, the endpoint asks each provider
    for the vault's `balanceOf` of every admitted token (`reserve`) and every configured convertible (`convertible`,
    `REQ-CV-4`) at that block number. Both answers are kept in `detail`; agreeing answers are the reading, anything else
    (disagreement, a failure, an answer that is not 32 bytes, a balance past `u128`) is unknown (`DEC-12`). A head
    already read is not read again; a head the payout prover recorded first (it records the final head it proves at
    and reads no balance) is read when the follower reaches it, so a payout in flight never leaves the latest final
    point without readings (PR #50 review M4).
26. **Attested reserve reports stay a stub** (F-011, issue #11): `Recorded::attested` answers "no attested assets
    configured", so an attested asset's evaluations are incomplete and pause past the bound, never hold.
27. **The book is cut by commit order** (PR #40 second round, (1)). An evaluation records `pg_current_snapshot()` with
    the last entry id, from one statement (`reserve_evaluations.book_snapshot`); the book's terms and the statement's
    movements take an entry only when its recording transaction (`book_entries.recorded_tx`, set by the store) is
    visible in that snapshot. An entry with a lower id that commits later is in neither. Evaluations recorded before 0039
    have no snapshot and keep the id cut.
28. **The statement through the signer** (DEC-16, 2026-10-14). The core records an intent of purpose `statement` on the
    issuing network: `message` the statement's exact bytes, `amount` "0", and `statement: {asset, sequence, sha256}`; the
    store refuses any other shape, a statement field on any other intent, and an ed25519 signature over anything but a
    statement intent (and the statement scheme is now allowed in `signatures`). The signer names the statement key
    apart (`statement_key`, ed25519, bound to no network; no binding may take the purpose) and signs a statement intent
    only when its bytes hash to the named SHA-256 and are a bezent statement of the named asset and sequence naming that
    key, on a declared issuing network of the keys' environment. It never signs a second, different statement for the
    same asset and sequence, whatever the key (`REQ-PR-2`; PR #50 re-check S-1), and signs only at a place that skips
    nothing but abandoned places, naming each exactly (decision 29), which it reads through the store's
    `statement_place_refusal` (migration 0041; the signer's role reads neither table). The core reaches it over the link (`SignerProcess`), checks the returned
    signature against the recorded bytes (`Client::sign`) and against the statement key it is configured with, and
    `Statements` verifies every signature again before anything is written. The core holds only the public key
    (`[statement] public_key`).
29. **A signed statement is that place's statement.** If a publish stops after the signer signed and before the store
    took the document, the next publish finds the signed intent for that asset and sequence and, once vetted, writes its
    bytes and signature and records them; it never derives another statement for the place. Vetted (PR #50 review M3):
    nothing reaches the append-only store before the recorded signature verifies with the current statement key and
    the bytes are exactly what bezent re-derives from the book at the evaluation and issue time they state. One that
    is not (the declarations or the code changed in between, as a deploy does), or that an earlier statement key
    signed, is never stored and never re-signed: the place is **abandoned** (PR #50 re-check S-1, S-3, the reviewer's
    proposal). Migration 0041's append-only `reserve_statement_abandoned` records the asset, place, the SHA-256 and key
    of what was signed, the intent, the reason and the login that recorded it; a `statement-abandoned` event pages once;
    the next statement takes the next place, names the last published one as `previous_sha256` and lists
    `abandoned: [{sequence, sha256, signing_key, reason}]`. The store refuses a statement at an abandoned place and one
    that skips a place not abandoned; `verify_chain` accepts a gap only when the later statement names it. No key
    rotation is needed and nothing pages every cycle. A document in the store with no signature file is signed, by the
    recorded signature over exactly its bytes when there is one, only when it re-derives; anything else in the store
    under the next name leaves the chain stuck, paged as `statement-stuck` once per statement cycle (a day) for the same
    reason, not every cycle.
30. **The statement is required in configuration** (PR #50 review M2; `REQ-PR-1`: "MUST publish, at least daily").
    Without `[statement]` the core refuses to start. Only a test deployment may leave it out, and only by saying so:
    `statement = "disabled-test-only"` with `environment = "test"` (the default environment is production), logged at
    error level at start. Outside test the published fixture's statement key is refused too. The file alone never
    makes a deployment a test one (PR #50 re-check M-1): at start and every cycle the core reads the store's
    networks, and `environment = "test"` (so a disabled statement) and the fixture's statement key are refused while
    any declared network is not a test network (`bezent_store::environment`, the rule the sidecar uses: `kreivo-local`,
    `polygon-amoy`, `base-sepolia`, `bnb-testnet` and the test ids `kreivo-test`, `polygon-test`, `base-test`; the
    harness's anvil forks use the production ids and are not test networks). A statement due and not
    published within an hour of falling due (a day after the last one, or the asset's first evaluation before the
    first) pages `statement-overdue`, once per due time, whatever kept it (store, signer, no complete evaluation);
    one cycle's publishing is bounded at a minute, so a signer that never answers does not freeze the cycle and the
    check. The store is the directory `[statement] directory` (DEC-16's object-locked bucket is F-014's, mounted or
    synchronised there). The sidecar (TEST only) still runs neither.
31. **The lag bound and the role** (PR #40 second round, (2)). 0033 already re-checks a route's budget against the bound
    when the budget changes (decision 22); 0039 adds a network's role to the columns that re-check its bound, since a
    network made the issuing one serves every route.

## Review items left open (PR #40 review, 2026-10-14)

Not fixed in this PR (each needs more than a small change, or a ruling); none of them lets a divergence pass unseen.

- **S2, rest:** a cost in the origin token (`book::cost`) and a correction need no observation; requiring the
  observation of the outflow for an origin-token cost (`REQ-BK-4`, `REQ-GV-5`) changes the operator surface (F-014).
  Every such movement is now listed in the statement with its reference, so none is hidden.
- **S5:** the book unit is the largest admitted precision, worked out at read time; admitting a finer token later
  changes the meaning of every posting (the asset then diverges for good, failing safe). Fix: pin
  `assets.book_precision` once, refuse a finer admitted token, and backfill; a migration and a trigger.
- **N1:** cuts are per network, so a mint at or before the issuing cut can sit against an arrival above the origin cut
  while an earlier origin observation is pending; it diverges (fails safe, pauses). Consistent cuts across networks
  would avoid the false pause.
- **N2:** arrivals match the reserve address exactly, payouts without regard to case; funding addresses without
  regard to case on an EVM network only, exactly elsewhere (SS58 is case-significant; decision 15). Harmless with
  F-009 (it spells the vault as declared); one rule would be better.
- **N3:** `verify_chain` needs the list to start at sequence 1; `verify` does not check the signature file's `sha256`
  and `statement` fields.
- **N4:** `mints_paused` looks at surplus right after each reversal (the literal reading of `REQ-BE-6`); a later cost
  that takes surplus below the floor while a reversal is unrestored does not pause mints (it alerts, below the floor).
- **N5:** the comment "a reversal that leaves surplus at or above the floor pauses nothing" in
  `req_be_6_a_reversal_below_the_floor_…` has no assertion behind it.
- **Observer staleness:** closed for the reserve check by decision 16; `NFR-6`'s own alert on the observer is
  operations' (F-014).
- **Statement size:** `terms.surplus.movements` and `contributions` are listed from genesis; a window since the
  previous statement would keep each statement small (the chain keeps the history).

## Spec gaps

- **Value in transit** has no observation in V0; if a venue delivery ever moves reserve, the spec needs it observed
  before the book can itemise it.

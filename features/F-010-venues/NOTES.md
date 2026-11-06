<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-010 — notes

What was already built when this feature started (`origin/develop` at `08634a5`), what it adds, and what waits. Not
normative: `SPEC.md` and `PLAN.md` decide.

## Already there

| Requirement | Where it is built | Where it is tested |
|---|---|---|
| `REQ-CV-7` the role: spends only admitted convertibles, never the origin token; output only to the vault; bounded per window; nothing else | `contracts/evm/src/VaultRoles.sol`, `script/DeployVault.s.sol`, `config/polygon.json` (F-008) | `contracts/evm/test/fork/Conversion.t.sol` (a to c, approvals, single-hop swap only, two convertibles), `Negative.t.sol`, `Adversarial.t.sol` |
| Pinned pool (`REQ-VN-4`): the role pins the pool's fee tier; the declared pool is the deepest | `VaultRoles.sol`, `config/polygon.json` | `Conversion.t.sol` (`undeclaredPoolIsRefused`, `declaredPoolIsTheDeepest`) |
| Base and BNB Chain carry no conversion role | `config/base.json`, `config/bsc.json` (`venue: none`, no convertibles); the script refuses a member without a venue | `Deploy.t.sol` (`conversion member` only where a venue is configured; Base and BNB Chain deploys); `services/evm-endpoint/tests/fork.rs` deploys Base and BNB Chain with no conversion member |
| Observation of a convertible arrival, a conversion's input and its output (by transaction, never by amount or time), with both amounts in the output's detail | `services/evm-endpoint/src/observe.rs` (F-009) | `services/evm-endpoint/tests/observe.rs` |
| The kinds `convertible_arrival`, `conversion_output`, `conversion_input` and what each may bind: a deposit binds `arrival` or `conversion_output`, never a convertible arrival; its amount is its bound observation's. These rules constrain the kind, the place, the finality and the single use of an observation, **not whose value it is or which requester may claim it** (below) | migrations 0034, 0048 (F-002) | `crates/store/tests/arrival_kind.rs`, `writer_guards.rs`; `services/ingress/tests/requests.rs` |
| The mint is the observation's amount, scaled and rounded down | `services/core/src/engine.rs` (F-003, F-009 T-009-08) | `services/core/tests/lifecycle.rs`, `scaling.rs` |
| A conversion's output is booked as reserve, its input is not; convertible holdings are read per token and network, outside the equation | `services/core/src/reserve/` (F-004) | `services/core/tests/reserve_book.rs`, `reserve_check.rs`, `services/evm-endpoint/tests/reserve.rs` |
| The statement itemises unconverted holdings | `services/core/src/reserve/statement.rs` | `services/core/tests/statement.rs` |
| A transfer in `Source-Final` holding `CND-AwaitingConversion` mints nothing and goes to `Needs-Review` past its deadline (`REQ-CV-6`) | `services/core/src/engine.rs` (`source_final`) | `services/core/tests/conditions.rs`, with the condition put there by a hand-inserted transition (until amendment 0018 was built: see "Amendment 0018, A1b" below) |

## Added

| Test | Layer (SPEC §9) | Criterion |
|---|---|---|
| `services/core/tests/conversions.rs` `req_cv_1_the_store_refuses_…` (rewritten as `ac_a5_1_req_cv_1_inv_6_…` by amendment 0018's build: a deposit now names the arrival, and the store refuses a mint against it) | Store | `REQ-CV-1`: no transfer names a convertible arrival; it stays an unassociated arrival |
| `services/core/tests/conversions.rs` `req_cv_2_req_cv_5_the_store_refuses_…` | Store | `REQ-CV-2`, `REQ-CV-5`: a transfer's amount is the output's, not the input's |
| `services/core/tests/conversions.rs` `ac_a5_1_ac_a5_2_…_the_ingress_binds_the_output_and_the_core_mints_it` (now `ac_a5_2_req_cv_2_req_cv_5_inv_6_an_output_named_directly_is_bound_and_minted`; its `AC-A5.1` half moved to the A1b tests below) | Service, across the seam: the real ingress router on its own login, the core on its own, observations in the shape the EVM endpoint records them | `AC-A5.1` (a request naming the convertible arrival or the swap's input is refused, nothing minted), `AC-A5.2` (not final: nothing; final: bound for the output's amount and minted for it, never the input's; the coordinate is consumed once), `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-5`, `INV-2`, `INV-6` |
| `services/core/tests/statement.rs` `ac_a5_3_…` | Service (the statement) | `AC-A5.3`, `REQ-CV-4` |
| `services/evm-endpoint/tests/fork.rs` `fork_ac_a5_1_ac_a5_2_…` | Chain + service, on an anvil fork of Polygon | A convertible arrives; Swaps' member approves and swaps through the vault's role; the endpoint records a convertible arrival (never an arrival), the swap's input and its output, the output carrying both amounts and equal to the vault's balance change |

The seam test is in `services/core` with the ingress as a dev-dependency, as `services/kreivo-endpoint/tests/delivery.rs`
takes the core. The observations are written as the owner, as in the other core tests; the EVM endpoint's own writes are
`services/evm-endpoint/tests/` and the store's grants for it are `crates/store/tests/`. It was checked to bite: with the
core's mint reading the conversion input's amount (`detail.conversion.amount_in`) instead of the output's, the cycle that
orders the mint fails (the store refuses an order of an amount that is not the transfer's), so the test fails.

No migration, grant or `grants.rs` entry is added: nothing here touches the store's schema.

## What the binding rules do not do

Found in review of this pull request. `source_bindable()` (migration 0034) and `decide_deposit` accept a final observation
of the right kind at the asset's reserve, once. They do not tie it to a requester, a route or an expected arrival, and the
destination is the request's. Grants are per kind (`REQ-PN-2`). So:

- any requester holding the deposit grant can bind any final, unconsumed `conversion_output` at the reserve, whoever's swap
  made it, and mint it to a destination it chooses; the first to name the coordinate wins and the rightful requester is
  refused `ERR-SourceConsumed`. The mint is backed (the output's amount, once); the loss is attribution and direction;
- an **ordinary** arrival has the same exposure: there are no deposit addresses (`DEC-9`) and an inbound transfer carries no
  reference, so nothing binds it to its requester either. Checked against the real ingress with a second requester, and kept as
  `services/ingress/tests/requester_binding_today.rs`, which pins today's behaviour (not a desired one, no criterion) and goes when
  `d-requester-arrivals` is ruled;
- one mainnet instance serves Bloque Test and Bloque with one reserve and one route per asset and network (`PLAN.md`
  `DEC-15`, 0.6.0), so Bloque Test's deposit credential is production deposit authority over the same vault.

Nothing in this pull request closes or hides that. It needs its own decision (amendment 0018, "The exposure the binding
rules do not close"), and none of the options there closes it for ordinary arrivals.

**Since amendment 0026** (`d-requester-arrivals`, 2026-11-06: "Bind by sender"; migration 0135): a deposit binds only an
arrival whose sender is one of its requester's declared senders (`REQ-PN-3`). The test above is now
`services/ingress/tests/bind_by_sender.rs` and states the opposite. An output named directly is sent from the pool, so it
binds only for a requester that declared the pool (do not); one named through a convertible deposit is covered by the
check on that deposit's arrival. Naming another party's output for one's own awaiting deposit stays open (0026, Open 1).

## What waited (amendment 0018, proposed)

The spec did not say which convertible arrival a conversion's output belongs to, or what object holds
`CND-AwaitingConversion` (conditions sit on transfers, and the store refused a transfer on a convertible arrival). The
engine's `Source-Final` + `CND-AwaitingConversion` branch could not be reached in production, and `unassociated_arrivals`
listed a final convertible arrival with `CND-Unassociated`. `amendments/0018-conversion-record.md` set out the options.

## Amendment 0018, A1b (ruled 2026-11-05, `d-cv-conversion-link`)

Built by migration 0120 and the ingress and core changes below. `SPEC.md` v0.5.5 carries the text.

| Requirement | Where it is built | Where it is tested (layer, SPEC §9) |
|---|---|---|
| A deposit may name a final convertible arrival; nothing is minted against it (`REQ-RQ-3`, `REQ-CV-1`, `INV-6`) | `source_bindable()`, `transfers_observed_amount()` (0120); `decide_deposit` (`services/ingress/src/intake.rs`); the mint order's guard (0049, 0056) refuses a token not admitted | Store: `crates/store/tests/arrival_kind.rs` `m2_…`, `m1_inv_2_req_cv_1_…`. Service: `services/core/tests/conversions.rs` `ac_a5_1_req_cv_1_inv_6_…` (the core's own login is refused a hand-written mint) |
| It holds `CND-AwaitingConversion` at `Source-Final`, taken on the edge from `Accepted`, with no hand-inserted transition (`AC-A5.1`) | `Core::accepted` | Service: `conversions.rs` `ac_a5_1_…`; `conditions.rs` `cnd_awaiting_conversion_…` |
| Its requester names the output by the deposit's request reference, once (`REQ-RQ-4`) | `POST /v0/deposits/conversion` (`decide_conversion`); `deposit_conversion_record()` | Store: `deposit_conversions.rs` `req_rq_4_only_the_deposits_requester_names_its_output`. Service: `conversions.rs` `req_rq_4_inv_2_an_output_named_by_a_different_requester_is_refused`, and `ERR-ReferenceConflict` in the happy path |
| The source moves once from the arrival to the output, with its amount; the arrival is recorded with both amounts; each is consumed once (`REQ-CV-2`, `INV-2`) | `deposit_conversions`, `deposit_conversion_record()`, `transfers_guard_update`, `transfers_source_bindable()` (0120) | Store: `deposit_conversions.rs` `req_cv_2_req_rq_4_inv_2_…`, `inv_2_an_arrival_recorded_by_a_conversion_is_never_bound_again`, `inv_2_an_output_already_consumed_is_refused`. Service: `conversions.rs` `inv_2_an_arrival_named_twice_is_refused`, `inv_2_an_output_already_consumed_is_refused` |
| Only a final conversion output at the same reserve, of the deposit's asset (`REQ-CV-2`, `INV-6`) | `deposit_conversion_record()`, `decide_conversion` | Store: `deposit_conversions.rs` `req_cv_2_inv_6_…` |
| The mint is the output's, under the reference derived from the output (`AC-A5.2`, `REQ-CV-5`, `REQ-IN-4`) | `Core::conversion`, `Core::source_final` | Service: `conversions.rs` `ac_a5_2_req_cv_2_req_cv_5_req_rq_4_inv_2_inv_6_the_request_comes_first_then_the_output_then_the_mint` |
| Past its deadline, `Needs-Review`, never a refund; an output from after the deadline does not complete it; none named after review until 0140; see the ruling of 2026-11-06 below (`REQ-CV-6`, `REQ-RF-1`) | `Core::conversion`, `Core::source_final`; `decide_conversion` | Store: `deposit_conversions.rs` `req_cv_6_no_move_once_the_deposit_is_in_review`. Service: `conversions.rs` `req_cv_6_req_rf_1_…`, `req_cv_6_an_output_after_the_deadline_…` |

The direct path stays as it was: a deposit may name a conversion output itself (`ac_a5_2_req_cv_2_req_cv_5_inv_6_an_output_named_directly_is_bound_and_minted`),
and records no convertible arrival. What the ruling did not settle (this path, which amount the route's limits measure while
a deposit awaits, the conversion's input amount, which convertibles an asset admits) and the
exposure of "What the binding rules do not do" above are open in the amendment. Issue #10 stays open on them.

## Amendment 0018, Open 3 and Open 6 (ruled 2026-11-06, `d-cv-open-choices`)

Built by migration 0140, the ingress's `decide_conversion` and one check in the core's `evidence_holds`. `SPEC.md` v0.5.10
carries the text.

| Requirement | Where it is built | Where it is tested (layer, SPEC §9) |
|---|---|---|
| An output may be named for a deposit in `Needs-Review` (sent there from `Source-Final` holding the condition) while no refund of it is recorded; the deposit stays in review (`REQ-RQ-4`, `REQ-CV-2`, `REQ-CV-6`, `REQ-RV-2`) | `deposit_conversion_record()`, `transfers_guard_update()`, `review_entered_from()`, `deposit_refund_recorded()` (0140); `decide_conversion` | Store: `deposit_conversions.rs` `req_cv_2_req_rv_2_inv_2_an_output_is_named_for_a_deposit_in_review`, `req_cv_2_req_cv_6_a_review_that_is_not_the_conversions_takes_no_output`. Service: `conversions.rs` `req_cv_6_req_rf_1_past_its_deadline_…` |
| After a refund is recorded, naming is refused (`REQ-RF-1`) | `deposit_refund_recorded()`; `decide_conversion` | Store: `deposit_conversions.rs` `req_cv_2_req_rf_1_req_rv_2_no_output_is_named_after_a_refund`. Service: `conversions.rs` `ac_a5_4_req_cv_2_req_rf_1_req_rv_2_an_output_is_not_named_once_a_refund_is_recorded_and_not_refused` |
| The review ends by a recorded resolution into `Delivering` with a second approver; the mint is ordered once, at the output's amount; no edge is added (`REQ-RV-2`, `REQ-GV-5`, `REQ-CV-5`, `INV-6`) | `Core::resolve` refuses one whose approval recorded no output or another one (`manual_approvals.approved_output_*`, migration 0141); `Core::evidence_holds` refuses one while the source is still the convertible arrival | Service: `conversions.rs` `ac_a5_4_req_cv_2_req_rv_2_req_gv_5_inv_6_an_output_named_in_review_mints_once_after_a_two_person_resolution` |
| The output's amount is checked against the route's minimum, maximum and window, with the route's errors (`REQ-CV-2`, `REQ-LM-1`) | `decide_conversion` | Service: `conversions.rs` `ac_a5_5_req_lm_1_req_lm_2_req_cv_2_the_output_is_checked_against_the_routes_limits` |

Left open: the window counts a deposit's arrival amount, in the convertible's units, while it awaits; an output named at
`Source-Final` from a block past the deadline (Open 7) also stays in review and ends the same way.

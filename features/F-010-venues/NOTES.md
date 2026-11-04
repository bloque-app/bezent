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
| A transfer in `Source-Final` holding `CND-AwaitingConversion` mints nothing and goes to `Needs-Review` past its deadline (`REQ-CV-6`) | `services/core/src/engine.rs` (`source_final`) | `services/core/tests/conditions.rs`, with the condition put there by a hand-inserted transition |

## Added

| Test | Layer (SPEC §9) | Criterion |
|---|---|---|
| `services/core/tests/conversions.rs` `req_cv_1_the_store_refuses_…` | Store | `REQ-CV-1`: no transfer names a convertible arrival; it stays an unassociated arrival |
| `services/core/tests/conversions.rs` `req_cv_2_req_cv_5_the_store_refuses_…` | Store | `REQ-CV-2`, `REQ-CV-5`: a transfer's amount is the output's, not the input's |
| `services/core/tests/conversions.rs` `ac_a5_1_ac_a5_2_…_the_ingress_binds_the_output_and_the_core_mints_it` | Service, across the seam: the real ingress router on its own login, the core on its own, observations in the shape the EVM endpoint records them | `AC-A5.1` (a request naming the convertible arrival or the swap's input is refused, nothing minted), `AC-A5.2` (not final: nothing; final: bound for the output's amount and minted for it, never the input's; the coordinate is consumed once), `REQ-CV-1`, `REQ-CV-2`, `REQ-CV-5`, `INV-2`, `INV-6` |
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
  reference, so nothing binds it to its requester either. Checked against the real ingress with a second requester;
- one mainnet instance serves Bloque Test and Bloque with one reserve and one route per asset and network (`PLAN.md`
  `DEC-15`, 0.6.0), so Bloque Test's deposit credential is production deposit authority over the same vault.

Nothing in this pull request closes or hides that. It needs its own decision (amendment 0018, "The exposure the binding
rules do not close"), and none of the options there closes it for ordinary arrivals.

## What waits (amendment 0018, proposed)

The spec does not say which convertible arrival a conversion's output belongs to, or what object holds
`CND-AwaitingConversion` (conditions sit on transfers, and the store refuses a transfer on a convertible arrival). Two findings
from reading the code:

- The engine's `Source-Final` + `CND-AwaitingConversion` branch cannot be reached in production: nothing puts a transfer there.
  Its test inserts the transition by hand.
- `unassociated_arrivals` lists a final convertible arrival, with `CND-Unassociated` (`REQ-IN-6`), not `CND-AwaitingConversion`.
  Reporting the second there would stay true forever, because nothing links an arrival to the conversion that spent it.

`amendments/0018-conversion-record.md` sets out the options without choosing. Until it is ruled: the record of the arrival
with its amount against a deposit, `CND-AwaitingConversion` held by an arrival, and the `REQ-CV-6` deadline on a real
conversion are not built. The pull request is `Part of #10`.

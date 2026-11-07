<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# Alerts: what is a page, and whether it reaches anyone

`F-014`, `REQ-IC-2`, `REQ-ST-6`. The services serve no port except the ingress, so every alert is either a **log
line** a metric filter matches, or a **metric** the core writes to its log (`observables`, below), or a platform alarm.

## A page is a log line

An ERROR event whose fields include `kind=<kind>`, **unquoted**. Two emitters wrote the kind differently until
`F-014`: the reserve check's pager (`kind = %page.kind`, rendered `kind=reserve-divergence`) and the engine's literals
(rendered `kind="payout-refusal-alert"`, which a filter on `kind=payout-refusal-alert` never matched). Every page now
records its kind with Display, and `services/core/tests/pages.rs` fails on a quoted one. Most pages are also an
`event_log` entry of the same kind, so the operator can read the detail with `SELECT`.

The services' logs are plain text, no colour, no target. The deployment sets `NO_COLOR=1` in every task definition (the
infrastructure repository's `app_bezent_services.tf`, `bezent_env`, on its main branch on 2026-11-04), which is why a
deployed line was plain. Nothing in **this** repository set it, and `tracing-subscriber` writes ANSI escapes around the field
names unless `NO_COLOR` is set, terminal or not (`\x1b[3mkind\x1b[0m\x1b[2m=\x1b[0mreserve-divergence`, which no `kind=`
filter matches). So that the format does not depend on a setting in another repository, every service now starts its log
with `bezent-logging` (`crates/logging`), which turns colour off itself; `services/core/tests/pages.rs` shows the default
setup's coloured line, the services' plain one, The filter for a kind is the string `"kind=<kind>"`.

## Does a page reach a human today?

**No.** Established on 2026-11-04 from the infrastructure repository's alarm definitions on its main branch (the live
account was not readable that day: the single-sign-on session had expired, so *live state is unverified*):

1. The alarms exist as code: one metric filter per log pattern on the service's log group, one alarm per metric, a
   not-running alarm per service and a rule for every stopped task.
2. The **log-based, database and ingress alarms have their actions disabled**: `actions_enabled` follows a variable whose
   default is false, and nothing in that repository sets it. The header says why: the log patterns "are written from the
   code, not from a live log" and must be tested first. The not-running alarms and the stopped-task rule are always on,
   but silent: every service's desired count is zero, so there is no task to run or stop.
3. Whatever notifies goes to a topic whose subscriber posts to a chat channel. That is a message in a shared channel: no
   on-call rota, no acknowledgement, no escalation, no phone. Nobody is woken.
4. The patterns cover some pages only (table below).

So today a page is a line in a log nobody is watching. What it needs is in "What infrastructure must add".

## Page kinds

"Filter" is whether the infrastructure repository's alarms on its main branch match the kind. "Base" says whether the
kind exists on `develop` at this commit.

| Kind | Emitted by | Runbook | Base | Filter today |
|---|---|---|---|---|
| `reserve-divergence` | reserve check | `pages/reserve.md` | yes | yes (ReservePages) |
| `supply-mismatch-pause` | reserve check | `pages/reserve.md` | yes | yes (ReservePages) |
| `reserve-alert` | reserve check | `pages/reserve.md` | yes | yes (ReservePages) |
| `book-quarantine` | reserve book | `pages/reserve.md` | yes | yes (ReservePages) |
| `reserve-blind` | reserve check (amendment 0028: the asset held blind beyond its bound, or a finality stall) | `pages/reserve.md` | yes | **no**: the filter must be added |
| `reserve-check-stalled` | reserve check (only when the failure could not be recorded, so nothing is held) | `pages/reserve.md` | yes | yes (ObserverLagPages) |
| `reserve-check-incomplete` | reserve check: since amendment 0028 recorded only (`recorded=`), never paged | `pages/reserve.md` | yes | yes (ObserverLagPages), and now matches nothing |
| `reserve-check-failed` | reserve check (paged only when the failure could not be recorded; otherwise recorded only) | `pages/reserve.md` | yes | yes (ObserverLagPages) |
| `reserve-hold` | reserve check: a hold placed (amendment 0028); recorded only (`recorded=`), never paged | `pages/reserve.md` | yes | no, and none wanted |
| `reserve-hold-lifted` | reserve check: a hold lifted by an evaluation that holds; recorded only, never paged | `pages/reserve.md` | yes | no, and none wanted |
| `key-compromise` | reserve check (supply mismatch); the core's signals step (every other alert) | `pages/key-compromise.md` | yes | yes (KeyCompromisePages), but before `F-014` only the supply-mismatch case ever logged it |
| `lane-use-unknown` | EVM endpoint (the lane detectors, amendment 0022 P2); a log line only, once an hour per lane and read | `pages/key-compromise.md` | yes | **no** |
| `statement-overdue` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-stuck` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-abandoned` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-publish-failed` | statements | `pages/statements.md` | yes | **no** |
| `payout-window-alert` | engine | `pages/payout.md`, `conditions/CND-PayoutWindow.md` | yes | yes, by message text |
| `payout-refusal-alert` | engine | `pages/payout.md` | yes | **no** (and quoted before `F-014`) |
| `resolution-blocked-by-inclusion` | engine | `pages/payout.md` | yes | **no** (and quoted before `F-014`) |
| `resolution-unprepared-alert` | engine | `pages/payout.md` | yes | **no** |
| `attested-withdrawal-unrelayed` | engine | `pages/attested.md` | **no, PR #66** | no |
| `attested-withdrawal-awaiting-head` | engine | `pages/attested.md` | **no, PR #66** | no |
| `attested-report-refused` | attested reports | `pages/attested.md` | **no, PR #66** | no |
| `attested-instructions-refused` | engine | `pages/attested.md` | **no, PR #66** | no |
| `fee-position-low` | core (`services/core/src/fees.rs`); once on crossing below a floor, then daily, from the log | `pages/float-top-up.md` | yes | **no** |
| `late-mint-prover-silent` | late-mint check | `pages/late-mint.md` | yes | **no** |
| `late-mint-watch-failed` | late-mint check | `pages/late-mint.md` | yes | **no** |
| `late-mint-check-failed` | late-mint check | `pages/late-mint.md` | yes | **no** |

The kinds of PR #66 are written in the code of that open pull request, quoted (`kind = "..."`), so it will fail
`pages.rs` and `runbooks.rs` when it rebases until it records the kind with `%` and the runbook drops its
`not_on_base` marker. That is intended. The late-mint kinds of PR #72 (on `develop`) were quoted too and are fixed in this change.

Platform alarms with no kind (`pages/platform.md`): a service not running, the store unreachable, the signer refusing an
unauthenticated caller, the database's storage and CPU.

## Metrics

The core writes the observables of `REQ-ST-6` once a minute as CloudWatch embedded-metric-format lines, namespace
`bezent`, dimensions `Asset`, `Network`, `Direction` where they apply (`services/core/src/observe.rs`). The Kreivo
endpoint writes one more of its own, in the same format and namespace, below (`submission_gate_state`). A value that is
unknown is left out and named in the line's `unknown` list: an alarm on **missing data** catches it, and a read that fails
writes nothing, which is the same alarm. Values pass through a 64-bit float: `reserve_amount` is exact up to 2^53 minor
units and approximate beyond (an 18-decimal token above about 0.009 units); `ops_observables()` holds the exact value
(`INV-11`) and the runbooks read that. Alarm suggestions, each from a spec bound (no number here is a threshold of the
deployment):

| Metric | Alarm | Spec |
|---|---|---|
| `observer_lag_estimate_seconds` | above 60 s for a network (alert); missing data is a breach. An **estimate** (see below) | `NFR-6` |
| `reserve_equation_age_seconds` | older than three evaluations (the check runs each minute) | `NFR-5` |
| `reserve_equation_outcome` | not 0 | `INV-1` |
| `open_gaps` | above 0 | `REQ-ST-6`, `REQ-OB-3` |
| `oldest_nonfinal_transfer_age_seconds` | above the route's completion budget | `NFR-2` |
| `unknown_legs_oldest_age_seconds` | above the route's deadline | `REQ-RV-1` |
| `unassociated_arrivals_oldest_age_seconds`, `reviews_oldest_age_seconds` | above one business day | `REQ-IN-6`, `REQ-RV-1` |
| `undelivered_notifications_oldest_age_seconds`, `dead_notifications` | growing, above 0 | `REQ-ST-5` |
| `fee_short_transfers` | above 0 | `CND-FeeShort` |
| `fee_position_amount` | the floor is in the core's configuration and the core pages on it (`fee-position-low`, below); this alarm is for **missing data**, which the page does not cover: a breach. Every network | `REQ-ST-6`, `REQ-TR-5` |
| `fee_position_age_seconds` | growing without bound: the endpoint is not recording a reading | `REQ-ST-6` |
| `pauses_in_force` | above 0 (information) | `REQ-GV-1` |
| `submission_gate_state` (dimension `Network`; written by the Kreivo endpoint) | not 0 for a few minutes (alert); missing data is a breach. `0` open, `1` closed | issue #104, `REQ-ST-6` |

**The Kreivo submission gate** (issue #104, `services/kreivo-endpoint/src/metric.rs`). The endpoint builds and signs a
delivery only while the runtime's transaction version and metadata are ones it writes and the contract's delivery role
is the lane's account (`features/F-007-kreivo-endpoint/NOTES.md`, item 4). **While the gate is closed the endpoint stays
healthy**: it keeps observing, so the liveness file and the container health check say nothing, and every delivery
waits. The gate's state is therefore its own signal, in two places. (1) `submission_gate_state`, once a minute, from a
fresh judgement at the finalized head: `0` open, `1` closed. It is left out of the line, and named in `unknown`, when
the gate cannot be judged (the chain does not answer, or the finalized head cannot be read), never written as a zero, so
an alarm that treats missing data as breaching catches it, as for the other metrics. No line is written at all while the
endpoint is catching up on a backlog of blocks or a pass ends early on a store error, since the line is written after
those steps: that missing data is the backlog or the store, not the gate, so look at the follower's logs before the
gate's. The line carries no reason. (2) The log: each change of a settled state is one line, at ERROR level while
closed, naming every reason (`issue #104: submissions closed: ...`), and at INFO when open. A gate that cannot be judged
is logged at WARN level once it has stayed so for five minutes, and again for each further five (`issue #104:
submissions are shut because the gate cannot be judged`). **None of these lines has a `kind=` field and none is a
page**; the alarm is on the metric, and the operator reads the reason in the log. A closed gate is not a fault of the
endpoint: live minting needs governance to set the delivery role to the lane's account, and a runtime whose format the
endpoint does not write needs a build that knows it.

`observer_lag_estimate_seconds` is an **estimate** of `NFR-6`'s quantity, the distance between an observer and its network's
own final head. The store never holds the network's head, only the heads the endpoint recorded, so the value is the clock
minus the block time of the latest head recorded, minus the network's configured `finality_seconds` (floored at zero): the
basis on which the reserve check decides a head is stale. It is wrong by whatever the network's real finality time differs
from that figure by and by any difference between the chain's clock and the store's; it is not a measurement of the network's
head. Before `F-014` it was named `observer_lag_seconds` and measured the time since `final_heads.seen_at`, which an endpoint
that records old heads promptly keeps small, so it was renamed and replaced.

**Fee positions** (`REQ-ST-6`). Each endpoint reads its network's delivery account at each final head it records and
records it with the head (`fee_position_readings`, migrations 0110 and 0130; `services/evm-endpoint/src/fee.rs`,
`services/kreivo-endpoint/src/fee.rs`). On an EVM network the position is the native balance, from both providers; on the
issuing network it is the account's **free balance less its frozen balance**, through the light client (amendment 0025,
`d-fee-position-scope`; the owner expects usage subscriptions to replace the float, and this is the stopgap until they do).
A reading that cannot be made, or that the providers disagree on, is unknown, and an unknown value is left out of the metric
and named in the line's `unknown` list, as above. `fee_position_amount` is in the network's smallest unit (wei, planck): it
is exact in `ops_observables()` and approximate above 2^53 in the metric (about 0.009 of a native unit at 18 decimals), so
read the exact value from `ops_observables()` and use the metric for the alarm.

**The floor is bezent's own configuration, and the core pages on it.** `fee_floors` in the core's configuration names, per
network, a floor in the smallest unit as a string. Each minute the core compares each network's latest fee position with its
floor, exactly (as the store's text, not the metric's float), and pages `fee-position-low` when it is below: once on
crossing, then once a day while it stays below, and at once again after a recovery (`fee-position-recovered` in the
`event_log`). The runbook is `pages/float-top-up.md`. A network with no floor is logged once at start and never paged on. An
unknown or missing reading is not low and raises no page of its own, so **while a reading stays unknown the core does not page and the infrastructure alarm on `fee_position_age_seconds` is required before value moves**: **the missing-data alarm on `fee_position_amount` and the
growing `fee_position_age_seconds` are its signals**, and a delivery that already could not pay shows as `fee_short_transfers`
(`CND-FeeShort`).

A fee position that cannot be read or written never stops the head or the reserve readings. A read that fails or exceeds
five seconds is recorded as unknown. A write the store refuses (a table not yet migrated, a grant or guard that drifted) is
logged at ERROR level as "the delivery account's fee position could not be recorded", with the network, the block and the
store's error, and has **no `kind=` field and is not a page**. No row is written for that head, so the latest row stops
moving: `fee_position_amount` keeps the last recorded value (or stays NULL, unknown, if none was ever recorded) and
`fee_position_age_seconds` grows without bound (NULL if none was ever recorded). The growing age is the signal; the alarm on it
(the table above, `fee_position_age_seconds`) is what makes a repeated write failure visible.

## What infrastructure must add

Stated as needs; this repository changes none of it.

1. Test every pattern with `aws logs test-metric-filter` against a line of the running services, then turn the alarm
   actions on.
2. Add filters for `"kind=statement-publish-failed"`, `"kind=payout-refusal-alert"`,
   `"kind=resolution-blocked-by-inclusion"`, `"kind=resolution-unprepared-alert"`, `"kind=late-mint-"` (a prefix pattern; the late-mint pages of PR #72 are on
   `develop`), `"kind=lane-use-unknown"` (on the EVM endpoint's log group) and, when its pull request merges,
   `"kind=attested-"` (also a prefix).
3. Alarm on the metrics above, with missing data treated as breaching.
4. Give the alarm topic a destination that reaches a person: an on-call service with acknowledgement and escalation, or at
   least a phone-reaching subscription, chosen by the owner. A chat post is not a page.
5. An alarm on missing `fee_position_amount` for every network, and on `fee_position_age_seconds` growing without bound (the
   floor itself is the core's configuration, and its page is `fee-position-low`, above).
6. Start the services (`desired_count` above zero) before any of this is meaningful.

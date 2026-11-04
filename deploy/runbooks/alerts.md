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
| `reserve-check-stalled` | reserve check | `pages/reserve.md` | yes | yes (ObserverLagPages) |
| `reserve-check-incomplete` | reserve check | `pages/reserve.md` | yes | yes (ObserverLagPages) |
| `reserve-check-failed` | reserve check | `pages/reserve.md` | yes | yes (ObserverLagPages) |
| `key-compromise` | reserve check (supply mismatch); the core's signals step (every other alert) | `pages/key-compromise.md` | yes | yes (KeyCompromisePages), but before `F-014` only the supply-mismatch case ever logged it |
| `statement-overdue` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-stuck` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-abandoned` | statements | `pages/statements.md` | yes | yes (StatementPages) |
| `statement-publish-failed` | statements | `pages/statements.md` | yes | **no** |
| `payout-window-alert` | engine | `pages/payout.md`, `conditions/CND-PayoutWindow.md` | yes | yes, by message text |
| `payout-refusal-alert` | engine | `pages/payout.md` | yes | **no** (and quoted before `F-014`) |
| `resolution-blocked-by-inclusion` | engine | `pages/payout.md` | yes | **no** (and quoted before `F-014`) |
| `attested-withdrawal-unrelayed` | engine | `pages/attested.md` | yes | no |
| `attested-withdrawal-awaiting-head` | engine | `pages/attested.md` | yes | no |
| `attested-report-refused` | attested reports | `pages/attested.md` | yes | no |
| `attested-instructions-refused` | engine | `pages/attested.md` | yes | no |
| `late-mint-prover-silent` | late-mint check | `pages/late-mint.md` | yes | **no** |
| `late-mint-watch-failed` | late-mint check | `pages/late-mint.md` | yes | **no** |
| `late-mint-check-failed` | late-mint check | `pages/late-mint.md` | yes | **no** |

The attested kinds are the code of PR #66, a draft on amendment 0016 (proposed, not ruled): "Base" is yes on that branch and
on `develop` when it merges. They were quoted (`kind = "..."`) there and are recorded with `%` now, so `pages.rs` and
`runbooks.rs` hold them as they hold the others. The late-mint kinds of PR #72 (on `develop`) were quoted too and are fixed
in this change.

Platform alarms with no kind (`pages/platform.md`): a service not running, the store unreachable, the signer refusing an
unauthenticated caller, the database's storage and CPU.

## Metrics

The core writes the observables of `REQ-ST-6` once a minute as CloudWatch embedded-metric-format lines, namespace
`bezent`, dimensions `Asset`, `Network`, `Direction` where they apply (`services/core/src/observe.rs`). A value that is
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
| `pauses_in_force` | above 0 (information) | `REQ-GV-1` |

`observer_lag_estimate_seconds` is an **estimate** of `NFR-6`'s quantity, the distance between an observer and its network's
own final head. The store never holds the network's head, only the heads the endpoint recorded, so the value is the clock
minus the block time of the latest head recorded, minus the network's configured `finality_seconds` (floored at zero): the
basis on which the reserve check decides a head is stale. It is wrong by whatever the network's real finality time differs
from that figure by and by any difference between the chain's clock and the store's; it is not a measurement of the network's
head. Before `F-014` it was named `observer_lag_seconds` and measured the time since `final_heads.seen_at`, which an endpoint
that records old heads promptly keeps small, so it was renamed and replaced.

Nothing reads a delivery account's balance, so there is **no alert floor on a float** (`pages/float-top-up.md`).

## What infrastructure must add

Stated as needs; this repository changes none of it.

1. Test every pattern with `aws logs test-metric-filter` against a line of the running services, then turn the alarm
   actions on.
2. Add filters for `"kind=statement-publish-failed"`, `"kind=payout-refusal-alert"`,
   `"kind=resolution-blocked-by-inclusion"`, `"kind=late-mint-"` (a prefix pattern; the late-mint pages of PR #72 are on
   `develop`) and `"kind=attested-"` (also a prefix; the attested pages of PR #66, once it merges).
3. Alarm on the metrics above, with missing data treated as breaching.
4. Give the alarm topic a destination that reaches a person: an on-call service with acknowledgement and escalation, or at
   least a phone-reaching subscription, chosen by the owner. A chat post is not a page.
5. A scheduled read-only check of each delivery account's native balance, with an alert floor set in configuration
   (PLAN `DEC-18`), until the services read it.
6. Start the services (`desired_count` above zero) before any of this is meaningful.

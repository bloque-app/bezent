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

The container's logs are plain text (`NO_COLOR=1`, no target). The filter for a kind is the string `"kind=<kind>"`.

## Does a page reach a human today?

**No.** Established on 2026-11-04 from the infrastructure repository's alarm definitions on its main branch (the live
account was not readable that day: the single-sign-on session had expired, so *live state is unverified*):

1. The alarms exist as code: one metric filter per pattern on the service's log group, one alarm per metric, and a
   not-running alarm per service.
2. Their actions are **disabled** (`actions_enabled` follows a variable whose default is false, and nothing sets it),
   because the services have no task yet (every service's desired count is zero) and the patterns "are written from the
   code, not from a live log".
3. Even enabled, an alarm notifies a topic whose subscriber posts to a chat channel. That is a message in a shared
   channel: no on-call rota, no acknowledgement, no escalation, no phone. Nobody is woken.
4. The patterns cover some pages only (table below).

So a page today is a line in a log nobody is watching. What it needs is in "What infrastructure must add".

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
| `attested-withdrawal-unrelayed` | engine | `pages/attested.md` | **no, PR #66** | no |
| `attested-withdrawal-awaiting-head` | engine | `pages/attested.md` | **no, PR #66** | no |
| `attested-report-refused` | attested reports | `pages/attested.md` | **no, PR #66** | no |
| `attested-instructions-refused` | engine | `pages/attested.md` | **no, PR #66** | no |
| `late-mint-prover-silent` | late-mint check | `pages/late-mint.md` | **no, PR #72** | no |
| `late-mint-watch-failed` | late-mint check | `pages/late-mint.md` | **no, PR #72** | no |
| `late-mint-check-failed` | late-mint check | `pages/late-mint.md` | **no, PR #72** | no |

The kinds of PR #66 and PR #72 are written in the code of those open pull requests, quoted
(`kind = "..."`), so they will fail `pages.rs` when they merge until they record the kind with `%`. That is intended.

Platform alarms with no kind (`pages/platform.md`): a service not running, the store unreachable, the signer refusing an
unauthenticated caller, the database's storage and CPU.

## Metrics

The core writes the observables of `REQ-ST-6` once a minute as CloudWatch embedded-metric-format lines, namespace
`bezent`, dimensions `Asset`, `Network`, `Direction` where they apply (`services/core/src/observe.rs`). A value that is
unknown is left out and named in the line's `unknown` list: an alarm on **missing data** catches it, and a read that fails
writes nothing, which is the same alarm. Alarm suggestions, each from a spec bound (no number here is a threshold of the
deployment):

| Metric | Alarm | Spec |
|---|---|---|
| `observer_lag_seconds` | above 60 s for a network (alert); missing data is a breach | `NFR-6` |
| `reserve_equation_age_seconds` | older than three evaluations (the check runs each minute) | `NFR-5` |
| `reserve_equation_outcome` | not 0 | `INV-1` |
| `open_gaps` | above 0 | `REQ-ST-6`, `REQ-OB-3` |
| `oldest_nonfinal_transfer_age_seconds` | above the route's completion budget | `NFR-2` |
| `unknown_legs_oldest_age_seconds` | above the route's deadline | `REQ-RV-1` |
| `unassociated_arrivals_oldest_age_seconds`, `reviews_oldest_age_seconds` | above one business day | `REQ-IN-6`, `REQ-RV-1` |
| `undelivered_notifications_oldest_age_seconds`, `dead_notifications` | growing, above 0 | `REQ-ST-5` |
| `fee_short_transfers` | above 0 | `CND-FeeShort` |
| `pauses_in_force` | above 0 (information) | `REQ-GV-1` |

Nothing reads a delivery account's balance, so there is **no alert floor on a float** (`pages/float-top-up.md`).

## What infrastructure must add

Stated as needs; this repository changes none of it.

1. Test every pattern with `aws logs test-metric-filter` against a line of the running services, then turn the alarm
   actions on.
2. Add filters for `"kind=statement-publish-failed"`, `"kind=payout-refusal-alert"`,
   `"kind=resolution-blocked-by-inclusion"`, and, when their pull requests merge, `"kind=attested-"` and
   `"kind=late-mint-"` (prefix patterns).
3. Alarm on the metrics above, with missing data treated as breaching.
4. Give the alarm topic a destination that reaches a person: an on-call service with acknowledgement and escalation, or at
   least a phone-reaching subscription, chosen by the owner. A chat post is not a page.
5. A scheduled read-only check of each delivery account's native balance, with an alert floor set in configuration
   (PLAN `DEC-18`), until the services read it.
6. Start the services (`desired_count` above zero) before any of this is meaningful.

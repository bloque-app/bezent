<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: statements
kind: pages
covers: [REQ-PR-1, REQ-PR-2, REQ-IC-2]
pages: [statement-overdue, statement-stuck, statement-publish-failed, statement-abandoned]
observables: [reserve_equation_age_seconds]
tests: [services/core/tests/statement.rs]
exercised: no
---
# Statement pages

## What it is

The core publishes a signed statement of each bridged asset's reserve every day to an object-locked store
(`REQ-PR-1`, `REQ-PR-2`; `deploy/statements.md` has the configuration and the policy). A missing statement is not a
safety event for value, but it is a promise to the public that is broken, so it pages. The signed statement is always
kept and written again every cycle; it is never skipped.

| Page | Meaning |
|---|---|
| `statement-overdue` | A statement fell due an hour ago and is not published. The backstop for the three below. |
| `statement-publish-failed` | The store did not take the signed statement (bucket, role, key, object lock). Once at once, then daily while it lasts. |
| `statement-stuck` | The next statement's place holds a document bezent did not sign and cannot re-derive; nothing was signed. |
| `statement-abandoned` | The core set a statement's place aside, recording its hash and key, and moved to the next place; the next statement names it. |

## Pages

ERROR lines `page kind=statement-... asset=...` and `event_log` entries of the same kind.

## Confirm

```sql
SELECT seq, kind, payload FROM event_log WHERE kind LIKE 'statement-%' ORDER BY seq DESC LIMIT 10;
SELECT asset, sequence, issued_at FROM reserve_statements ORDER BY id DESC LIMIT 5;
SELECT * FROM reserve_statement_abandoned ORDER BY id DESC LIMIT 5;
```

The page's payload carries the reason in words (the bucket's answer, the policy to check) and, for a failure, whether the
store refused an overwrite.

## Do

1. `statement-publish-failed`: fix what the reason says, from `deploy/statements.md` ("The task role's policy", "What the
   core does when it cannot write"). The signed statement is written again at the next cycle with no action.
2. `statement-stuck`: do not sign or write a statement by hand. Establish who wrote the document at that place; if it is
   not bezent's, that is a custody question, so follow `pages/key-compromise.md` for the statement key.
3. `statement-abandoned`: read the reason and the hash; confirm the next statement names the abandoned place. No action
   unless the reason is unexplained.
4. If the cause is a stuck signer link, see `pages/platform.md`. Record any change made outside the store with
   `operator_record('other', 'statement store', '{}'::jsonb, '<why>')`.

## Never

- Never delete or overwrite a published statement or lift its object lock; the store is append-only on purpose
  (`REQ-PR-2`).
- Never write a statement by hand, or backdate one, or sign with a key other than the statement key.
- Never treat a late statement as a reserve problem, or the reverse: the reserve is `pages/reserve.md`.
- Never edit the book, a status or a reserve reading to make a statement come out; no administrative path moves value or
  edits a status (`REQ-GV-5`).

## Exercise

On the test bed: remove the task role's list permission, see `statement-publish-failed`, restore it, and see the same
bytes written with no page left open.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

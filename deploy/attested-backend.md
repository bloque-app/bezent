<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# Operating an attested backend

For an operator running a backend whose facts are a named party's reports (`F-011`). The contract the party follows is
`features/F-012-swaps-module/attesting-contract.md`.

## Settings

The core's configuration (`services/core/fixtures/core.toml`), both optional:

| Key | Default | Meaning |
|---|---|---|
| `attested_snapshot_interval_seconds` | `600` | How often the party is expected to send a `positions` report. Past it, the alert `attested-snapshot-overdue:<network>` and nothing else |
| `attested_snapshot_max_age_seconds` | `3600` | How old the last one may be. Past it, a `final_head` difference: the asset's routes pause (cause `REQ-BK-1`) and a human is paged |

Both values are a working assumption until the owner rules on them (`features/F-011-attested-backend/NOTES.md`). Lifting
that pause is the operator's, as for any divergence, once a current snapshot agrees with the book.

## Runbook: a report the core refused

**You see:** the page `attested-report-refused`, with `network`, `report_id`, `kind` and `reason`, in the service's log and
in `event_log`. Then, within the bound above, the asset's routes pause because the backend's head stopped.

**What it means:** the core records a report only if the signature verifies over the exact bytes, by a key declared for
the backend, and the row says what those bytes say. This one failed. Everything after it on that network waits, on
purpose: nothing later may reach the book around it. Other networks go on. Nothing has moved because of this report.

1. Read `reason`.
   - **`its signature does not verify`, `its digest is not of the bytes signed`, `its body is not the document signed`:** the
     ingress accepted these only after checking the same things, so **a row was written that did not come through it, or
     it changed.** Treat it as a compromise of the store or the ingress: stop here and follow the key-compromise runbook
     (`REQ-IC-4`). Do not repair the queue until that is closed.
   - **`its key is not declared for this backend`:** a key was retired too early. Go to the next runbook, step 3.
   - **Anything from the store (`BZ001`):** the report names something the store cannot record, such as a declaration that
     changed. Fix the declaration if it was wrong; the core retries every cycle and the queue moves by itself.
2. Ask the party whether it sent that `report_id`, from its own record.
3. **There is no repair through the services, by design:** `attested_reports` is append-only, and no administrative path
   edits an observation (`REQ-GV-5`). None of these rows is an observation yet, and nothing has moved on them. If the row
   is not the party's and the queue must move, the migration owner removes it **and every later unrecorded row of that
   network**, since numbers run without a gap, lifting the table's append-only trigger for that one statement, under the
   incident's record, with a second person.
4. The party re-sends what it did send **under new `report_id`s**: the old ones stay spent in `intake`, and asking for them
   again answers `internal`. A deposit that named a removed report's coordinate was answered `not-final` and bound nothing;
   it names the new coordinate.
5. When the queue moves, the next `positions` report brings the head current. Lift the pause once the check holds.

**Reversal of this choice:** V0 keeps this manual because a failed signature should stop the backend (`NOTES`, question 5).

## Runbook: rotating the party's signing key

A report is checked twice: by the ingress when it arrives, and by the core when it records it, a cycle later
(`cycle_seconds`). A key retired between the two refuses a real report, and the queue stops.

1. **Declare the new key beside the old one**, by migration (`attesting_keys` is a declaration, like every other).
2. **The party switches** to the new key.
3. **Before retiring the old key, confirm nothing by it waits:**

   ```sql
   SELECT count(*) FROM attested_reports r
   WHERE r.network = '<network>' AND r.public_key = '\x<old key>'
     AND NOT EXISTS (SELECT 1 FROM observations o
                     WHERE o.network = r.network AND o.block_number = r.seq);
   ```

   It must read 0, after the party has stopped using the old key. A few cycles suffice.
4. **Retire the old key**, by migration. A report that still arrives under it is refused at the ingress, with
   `ERR-NotAuthorised` and nothing recorded, which is what you want.

If a key was retired too early and a report is refused (`its key is not declared for this backend`): declare the old key
again. The core retries the report next cycle, the queue moves, and you retire it again by the steps above.

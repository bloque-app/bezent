<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: attested
kind: pages
covers: [REQ-BE-4, REQ-BE-5, REQ-RQ-9, REQ-IC-2]
pages: [attested-withdrawal-unrelayed, attested-withdrawal-awaiting-head, attested-report-refused, attested-instructions-refused]
not_on_base: [attested-withdrawal-unrelayed, attested-withdrawal-awaiting-head, attested-report-refused, attested-instructions-refused]
observables: [oldest_nonfinal_transfer_age_seconds, nonfinal_transfers]
tests: []
exercised: no
---
# Attested-backend pages (open PR #66; not on this base)

## What it is

On an attested backend, an attesting party reports the reserve and relays a withdrawal's payout instructions; bezent
verifies, never believes (`REQ-BE-4`, `REQ-BE-5`, `REQ-RQ-9`). The value of a redemption is destroyed when it is final;
the pages say the party's half did not happen. They are written from the code of PR #66 and are **not on `develop`
yet**; verify the kinds against the merged code and its amendment 0016.

| Page | Meaning |
|---|---|
| `attested-withdrawal-unrelayed` | A redemption is final and its value destroyed, and nobody has relayed its instructions within the route's budget. State unchanged. |
| `attested-withdrawal-awaiting-head` | The instructions are relayed, but the attested network has no final head yet, so the payout cannot be handed over. State unchanged. |
| `attested-report-refused` | A party's report was not recorded (signature, digest or body does not match); its network's queue is held behind it and the asset's reserve check will pause. Once. |
| `attested-instructions-refused` | The relayed instructions do not hash to the redemption's commitment; nothing is handed over. Once. |

## Pages

ERROR lines `kind=<kind>` with the transfer or the report named, and `event_log` entries of the same kind.

## Confirm

```sql
SELECT seq, kind, payload FROM event_log WHERE kind LIKE 'attested-%' ORDER BY seq DESC LIMIT 10;
SELECT bridge_reference, state, condition FROM transfer_status WHERE state IN ('Source-Final', 'Delivering');
```

## Do

1. `attested-withdrawal-unrelayed` and `-awaiting-head`: the value is destroyed and only the party can complete the
   payout. Contact the party through the arrangement in the deployment's own notes; the transfer waits, it is not failed
   and not refunded (`REQ-IC-3`, `REQ-RF-1`). Whether it eventually goes to review is unruled (amendment 0016): do not act
   on a guess.
2. `attested-report-refused`: the party's signed report did not verify. Ask the party for a fresh report signed with a
   key declared for the backend. Do not record it yourself.
3. `attested-instructions-refused`: the instructions relayed are not the redemption's. Ask for the correct relay; the
   bridge accepts instructions only when they hash to the commitment (`REQ-RQ-9`).

## Never

- Never accept instructions, a report or an outcome on a caller's word (`REQ-RQ-7`, guardrail 3); never type a report in.
- Never refund, or mint, while the party's half is outstanding (`REQ-RF-1`, `INV-8`).
- Never edit an observation, a relayed instruction or a status (`REQ-GV-5`).

## Exercise

Not possible on this base. On the test bed after PR #66 merges: redeem on an attested backend and relay nothing; relay
instructions that do not hash to the commitment; send a report with a bad signature.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: late-mint
kind: pages
covers: [REQ-IC-4, REQ-IC-2, AC-I1.2, INV-8, INV-9, INV-16]
pages: [late-mint-prover-silent, late-mint-watch-failed, late-mint-check-failed]
not_on_base: [late-mint-prover-silent, late-mint-watch-failed, late-mint-check-failed]
observables: [pauses_in_force]
tests: []
exercised: no
---
# Late-mint pages (open PR #72; not on this base)

## What it is

The core checks every final `Minted` and `RefundMinted` of the issuing network against what bezent signed. A mint of a
reference **after** bezent proved that reference not executed, or of a reference whose order was never signed, is a late
mint: a leaked delivery key, governance, or an attempt bezent did not record. It raises `key-compromise` (see
`pages/key-compromise.md`). The three pages below are the check telling you it cannot do its job. They are written from
the code of PR #72 and are **not on `develop` yet**; verify the kinds against the merged code.

| Page | Meaning |
|---|---|
| `late-mint-prover-silent` | A mint waits on an attempt the endpoint's prover does not decide though its era is over (the prover is stuck, down or out of range), or a key is compromised. Never a pause by itself. |
| `late-mint-check-failed` | The check for one mint failed repeatedly. Once per observation. |
| `late-mint-watch-failed` | The core cannot list the mints to check, cycle after cycle: no late mint would be seen. |

## Pages

ERROR lines `kind=<kind>` with a `detail` and `event_log` entries of the same kind (once per observation, durable across
restarts).

## Confirm

```sql
SELECT seq, kind, payload FROM event_log WHERE kind LIKE 'late-mint-%' OR kind = 'key-compromise' ORDER BY seq DESC LIMIT 10;
```

The payload names the reference, the attempt and the signed transaction. Check the mint on the issuing network from
public chain data, and the endpoint's health for the prover (`deploy/health.md`).

## Do

1. `late-mint-prover-silent`: find out whether the prover is stuck (restart cannot mend a poisoned row: read its log) or
   the key is compromised. If you cannot tell within the hour, pause (`operator_pause`) and follow
   `pages/key-compromise.md`. The row stays open and the alert still raises if the prover later decides against the mint.
2. `late-mint-check-failed` and `late-mint-watch-failed`: the check is blind. Fix the store, the grant or the row it
   fails on (the payload names it). Until it is fixed no late mint would be seen: pause the issuing routes if the blindness
   lasts (`operator_pause`).

## Never

- Never refund a redemption whose reference may have been minted: `INV-8` allows a refund mint only when every payout
  attempt is proven not executed at finality (`REQ-RF-1`).
- Never decide "not minted" from a head that has moved on; only the prover's own record ends the wait (`INV-16`).
- Never edit an observation or a proof (`REQ-GV-5`).

## Exercise

Not possible on this base. On the test bed after PR #72 merges: mint a reference after its proof, and again with the
prover stopped.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

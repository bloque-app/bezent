<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: key-compromise
kind: pages
covers: [REQ-IC-4, REQ-IC-2, REQ-KY-3, REQ-KY-4, AC-I1.2, INV-4, INV-9, INV-14]
pages: [key-compromise]
observables: [pauses_in_force]
tests: [services/core/tests/signals.rs::req_ic_4_an_alert_in_the_log_becomes_one_page_with_kind_unquoted, services/core/tests/lifecycle.rs::ac_i1_2_a_signature_not_over_the_recorded_bytes_raises_the_alert_and_moves_nothing, services/core/tests/cancel.rs::ac_i1_2_a_cancel_signature_not_over_its_bytes_raises_the_alert, services/core/tests/running_core.rs::inv_4_a_supply_mismatch_raises_key_compromise_once_and_pauses_until_the_operator_lifts_it]
exercised: no
---
# Key compromise (a signature bezent did not record)

## What it is

A signature by one of bezent's keys that answers no recorded intent, or a signature the signer returned that is not over
the exact bytes the core asked for, or a supply that the on-chain component does not explain (`REQ-IC-4`, `AC-I1.2`,
`INV-4`, `INV-9`). It is treated as a compromise: **pause, rotate, report**. The key's maximum loss is already bounded
on chain per asset and window (`REQ-KY-3`, `AC-I1.1`); that is why this is an incident and not yet a loss.

## Pages

`key-compromise`, an ERROR line `page kind=key-compromise ...` and an `event_log` entry of the same kind. **Nothing has
paused when you read it**, unless the cause was a supply mismatch (`supply-mismatch-pause`, which pauses the asset by
itself). The signer and the endpoints raise the alert and a `pause-requested` entry in the log; no process applies that
request yet (`features/F-014-operations/NOTES.md`, "Open"). The pause is yours, and it is the first thing to do.

## Confirm

```sql
SELECT seq, kind, payload FROM event_log
WHERE kind IN ('key-compromise', 'pause-requested', 'signal-surfaced') ORDER BY seq DESC LIMIT 10;
SELECT id, asset, network, direction, cause FROM pauses ORDER BY id;
```

The alert's payload names the detector (signer, an endpoint or the core), the key's public identifier and the
coordinate where it was seen, or the intent the signature answered. Check that coordinate on the network from public
chain data: the transaction, its sender, its recipient and its amount. Establish whether value moved and whether any
reserve address was involved.

## Do

1. **Pause.** `SELECT operator_pause(NULL, NULL, NULL, 'key-compromise alert <seq>');` A global pause stops new work only;
   transfers in flight are still observed and completed (`REQ-GV-1`, `INV-15`). Do it before anything else.
2. Preserve the evidence: the alert, the signature, the coordinate, the intent and the signer's log. Do not restart the
   signer to "see if it clears".
3. Rotate the key per `REQ-KY-4`: keys are separate per network and role and rotate without stranding transfers in
   flight. The mechanics (the custody capability, the on-chain role) are in the deployment's own notes and
   `docs/key-custody.md`. A rotation on chain is a governed act with several signers (`REQ-KY-5`).
4. Record each act: `SELECT operator_record('key-rotation', '<key name, network>', '{}'::jsonb, '<alert seq>');`.
5. Report: the operator reports to the governance and to the requesters whose routes were paused (`REQ-IC-5`, once per
   condition, not per transfer). In-flight transfers are reported as unknown or delayed, never failed (`REQ-IC-3`).
6. Lift the pause with two people (`operator_request_unpause`, `operator_approve_unpause`) only after the key is
   rotated, the cause is understood, and the next reserve evaluation holds.

## Never

- Never leave the bridge unpaused while "checking". A bounded key is not a reason to wait.
- Never re-sign on silence, retry as a new transaction, or refund a transfer because of the alert (`REQ-RF-1`, `INV-8`).
- Never put key material in a log, a URL, an environment variable or an error, or paste it to a ticket (`REQ-KY-1`).
- Never give a test key authority on a production address (`REQ-KY-6`).
- Never edit the book or a status to hide the loss, and do not move value by hand; there is no path (`REQ-GV-5`).

## Exercise

On the test bed: have the core record an intent, make a signature over other bytes, see the alert and the log entry,
then follow steps 1 to 6 with two operators. The pause-request consumer is missing; until it exists the pause is by hand.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

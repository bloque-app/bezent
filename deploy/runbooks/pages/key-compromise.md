<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: key-compromise
kind: pages
covers: [REQ-IC-4, REQ-IC-2, REQ-KY-3, REQ-KY-4, AC-I1.2, INV-4, INV-9, INV-14]
pages: [key-compromise, lane-use-unknown]
observables: [pauses_in_force]
tests: [services/core/tests/signals.rs::req_ic_4_an_alert_in_the_log_becomes_one_page_with_kind_unquoted, crates/store/tests/compromise_pause.rs::ac_i1_2_each_detector_s_alert_pauses_the_bridge, services/core/tests/lifecycle.rs::ac_i1_2_a_signature_not_over_the_recorded_bytes_raises_the_alert_and_moves_nothing, services/core/tests/cancel.rs::ac_i1_2_a_cancel_signature_not_over_its_bytes_raises_the_alert, services/core/tests/running_core.rs::inv_4_a_supply_mismatch_raises_key_compromise_once_and_pauses_until_the_operator_lifts_it, services/evm-endpoint/tests/submit.rs::ac_i1_2_p2_one_provider_reporting_the_lane_used_outside_bezent_pauses_nothing_and_both_do, services/core/tests/conditions.rs::ac_i2_1_inv_15_a1b_an_intent_recorded_before_the_pause_is_not_signed_until_it_is_lifted]
exercised: no
---
# Key compromise (a signature bezent did not record)

## What it is

A signature by one of bezent's keys that answers no recorded intent, or a signature the signer returned that is not over
the exact bytes the core asked for, or a supply that the on-chain component does not explain (`REQ-IC-4`, `AC-I1.2`,
`INV-4`, `INV-9`). It is treated as a compromise: **pause, rotate, report**. The key's maximum loss is already bounded
on chain per asset and window (`REQ-KY-3`, `AC-I1.1`); that is why this is an incident and not yet a loss.

## Pages

`key-compromise`, an ERROR line and an `event_log` entry of the same kind. Two emitters write the line:

- the core's signals step, for every alert except a supply mismatch (an unrecorded signature seen by the signer or an
  endpoint, a mint after its proof of non-execution): `page: a signature bezent did not record ... kind=key-compromise alert_seq=<seq> detector=<detector>`;
- the reserve check, for a supply mismatch (`INV-4`): `page kind=key-compromise asset=<asset> detail=<json>`, which is
  followed by `supply-mismatch-pause`.

`lane-use-unknown`, an ERROR line of the EVM endpoint only (no `event_log` entry: the endpoint's login appends no other
kind): `page: the providers do not agree, or one failed, on whether the delivery key was used outside bezent ...
kind=lane-use-unknown dedupe=<network>:lane-code|lane-nonce:<lane> detail=<json>`. The read before a preparation or a
signature that would raise `key-compromise` (code at the lane, or its nonce past every nonce bezent signed) found it on one
provider only, or a provider failed. **Nothing is raised and nothing is paused** on one provider's word (amendment 0022, P2,
ruled 2026-11-06), and nothing is prepared or signed from the lane until both providers answer alike. At most once an hour
per lane and read. The `detail` names each provider's answer (`a`, `b`): read the lane's nonce and code on a public
explorer. If it shows what the provider that reported the use shows, the key was used outside bezent: pause by hand
(`SELECT operator_pause(NULL, NULL, NULL, 'lane-use-unknown <lane>');`) and follow "Do" below; the second provider will raise
the alert itself once it catches up. If it does not, fix or replace that provider (`deploy/compromise-pause.md`, step 3).

**The pause is already applied.** A `key-compromise` alert pauses the whole bridge in the transaction that records it
(migration 0095; `deploy/compromise-pause.md` has the reasoning and the recovery). It stops new work only: new requests,
orders, signing intents and attempts, and the signature of a signing intent recorded before the pause whose attempt has
no signature yet: the signer refuses it until the pause is lifted (amendment 0022, A1b). It does **not** stop a transaction
already signed, and it **cannot stop a stolen key**, which signs by itself:
only the on-chain pause and the bounds of `REQ-KY-3` do, and those are a person's act (step 1 below). The exception is a
supply mismatch (`INV-4`), which pauses that asset's routes.

**No page reaches anyone yet**: the alarms are code with their actions disabled and no destination that wakes a person
(`deploy/runbooks/alerts.md`, "Does a page reach a human today?"). The bridge is quiet, but the line is in a log nobody is
watching, and the chain half is waiting for a person.

## Confirm

```sql
SELECT seq, kind, payload FROM event_log
WHERE kind IN ('key-compromise', 'pause-requested', 'signal-surfaced') ORDER BY seq DESC LIMIT 10;
SELECT id, asset, network, direction, cause FROM pauses ORDER BY id;
```

The alert's payload carries a `detector`, which the store stamps with the role of the login that wrote the alert (`signer`,
`kreivo-endpoint`, `evm-endpoint` or `core`), the key's public identifier and the coordinate where it was seen, or the
intent the signature answered. A late mint (a mint of a reference after its proof of non-execution) is written by the core,
so its `detector` is `core`; what tells it apart is its `source`, which is `late-mint`, and its `reason`. Check that coordinate on the network from public
chain data: the transaction, its sender, its recipient and its amount. Establish whether value moved and whether any
reserve address was involved.

## Do

1. **The chain half.** The off-chain pause is already in force (`pauses_in_force`; the query above). It does not stop a stolen
   key: pause the on-chain component and, on an origin network, have the guardian disable the delivery module
   (`deploy/compromise-pause.md`, step 1). Do this before anything else. If no pause row names the alert (an alert written
   before 0095, or by the schema owner), `SELECT operator_pause(NULL, NULL, NULL, 'key-compromise alert <seq>');`.
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
then follow steps 1 to 6 with two operators. The pause is applied by the store; the chain half in step 1 is by hand, and the page is not yet delivered to anyone.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# When a compromise alert pauses the bridge

`REQ-IC-2`, `REQ-IC-4`, `AC-I1.2`, `AC-I2.2`. The reasoning is in `features/F-005-signer/NOTES.md`, "The compromise pause".

## What already happened

Any `key-compromise` alert pauses the whole bridge in the transaction that records it (migration 0095). No person, and
no other service, was involved. One row in `pauses`, `cause = 'REQ-IC-4'`, names the alert in `alert_seq`.

- **Stopped:** new requests (`ERR-RoutePaused`), new orders and signing intents, new attempts of a transfer in flight.
- **Not stopped:** observation; transfers in flight; refund mints, cancels and statements; a signing intent recorded
  before the pause, which is still signed and submitted (amendment 0022, proposed).
- **Not stopped, and bezent cannot stop it:** a stolen key signing by itself.

The exception is the supply mismatch (`INV-4`): an alert with `scope` `asset` pauses that asset's routes, under
`cause = 'INV-4'`, not the bridge.

## 1. Do this first: the chain half

`pauses` is bezent's own. If a key is really out, only these refuse its transactions:

- **Issuing network:** the on-chain component's `pause(All)`, by the pause role or governance (`AC-I2.1`). It stops mints
  and redemptions, not a refund mint. `lower_bounds` is immediate as well.
- **Origin network:** the vault's guardian disables the delivery module; the owners can lower the allowance at once.

Assume compromise until shown otherwise; the on-chain pause is a separate act with its own, heavier unpause.

## 2. Confirm

```sql
SELECT p.id AS pause_id, p.alert_seq, e.appended_at, e.payload ->> 'detector' AS detector,
       e.payload ->> 'detection_key' AS detection, e.payload ->> 'reason' AS reason
FROM pauses p JOIN event_log e ON e.seq = p.alert_seq
WHERE p.cause = 'REQ-IC-4' ORDER BY p.alert_seq;
```

Each alert has its own row, so a detection made while you were reviewing another stays paused after that one is lifted.
One event seen by two detectors is two alerts and two rows. Read every alert from the chain's public data: the
coordinate, the sender, the recipient, the amount.

## 3. Real, or a false alarm

| The alert says | A false alarm looks like | Before lifting |
|---|---|---|
| `lane's nonce on chain is past every nonce bezent signed` (EVM) | One provider answered high: the check takes the higher of the two. The other, and a block explorer, show the lower | Fix or replace the provider. If the nonce really moved, it is real |
| `the delivery key holds code` | One provider returned code: any code on either counts | Same |
| `transactions of the lane at its nonce in the consuming block` | Zero or several, from an incomplete block read | Read the block from a public source |
| `an extrinsic of the delivery key that the endpoint cannot decode` (Kreivo) | A runtime upgrade changed the format of bezent's own extrinsics. The extrinsic is one bezent recorded | Ship the endpoint that decodes it, or every such extrinsic raises again |
| `the bytes asked to be signed do not answer the core's order` (signer) | A new format or version skew between the endpoint and the signer's decoder | Align the versions |
| `a signature that is not the lane key's`; `signer-link`, `signer-key` | The key was rotated in the signer and not in the endpoint, or the other way round | Finish the rotation |
| A signature or a mint "bezent did not record" | The store was restored to before it was recorded (`NFR-7`) | Compare with the signer's log and the chain |

A signature that truly answers nothing bezent recorded is a compromise: rotate (`REQ-KY-4`), report, and do not lift until
the rotation is done. Never refund or fail a transfer because of the alert (`REQ-IC-3`, `REQ-RF-1`).

## 4. Lift

Only by the spec's rule (`AC-I2.2`): two people, on their own logins, one pause at a time, through the operator surface
(`operator_request_unpause`, then `operator_approve_unpause` by the other login, pull request #76). There is no other path:
no service can delete a pause, no false alarm is cleared by a flag or a restart, and this change adds no new way out. Until
pull request #76 is merged only the schema owner can delete the row, which is not a path to use in production.

```sql
-- One login, once per pause in force, naming what was checked:
SELECT operator_request_unpause(<pause_id>, '<why; what was checked>');
-- Another login:
SELECT operator_approve_unpause(<request id>, '<why>');
```

Before each approval, run the query in step 2 again and confirm `pauses` no longer holds the row afterwards.

Held work resumes by itself at the core's next cycle. The same detection does not pause again, ever (0049's one alert per
detector and detection key); a new one does, which is correct.

Lifting does not clear what the detection found: an EVM lane whose nonce is past what bezent signed, or that holds code,
is refused by the endpoint until the lane itself is dealt with, pause or no pause. That usually means rotating the key.

## Before deploying this

An alert recorded before migration 0095 has no pause, and its repeat never gets one. Run
`SELECT seq, payload FROM event_log WHERE kind = 'key-compromise'` on the target first and account for each.

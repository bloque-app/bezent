<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: token-upgraded
kind: pages
covers: [REQ-IC-2, REQ-AS-11, REQ-AS-3]
pages: [origin-token-upgraded]
observables: [reserve_equation_outcome, pauses_in_force]
tests: [services/core/tests/directed_alerts.rs::req_as_11_an_origin_tokens_upgrade_pages_and_pauses_nothing, services/evm-endpoint/tests/observe.rs::req_as_11_an_admitted_tokens_upgrade_is_recorded_as_its_own_kind, services/evm-endpoint/tests/observe.rs::req_as_11_native_usdcs_non_indexed_upgraded_is_recorded]
exercised: no
---
# An origin token's implementation changed

## What it is

An admitted origin token that is a proxy (native USDC on Polygon is: its implementation sits in the EIP-1967 slot) can
have its code replaced by its issuer. The proxy then emits `Upgraded(address implementation)` (native USDC's with the
address not indexed, in the data; others with it indexed: the endpoint reads both). The EVM endpoint watches every
admitted origin token for that event and records it as an observation of kind `token_upgrade` (migration 0147), which
binds nothing. The core pages on it.

The token's behaviour after an upgrade is the issuer's: `permit`, `transfer`, the events the endpoint pairs, a blacklist.
The directing code (amendment 0029) and the vault rely on that behaviour, and directed deposits are open to anyone. So an
upgrade is a reason for a person to look at once. **It pauses nothing and holds nothing.** Whether directed intake should
also hold on an upgrade until someone has reviewed it is an open ruling of the owner (`d-f018-usdc-upgrade`); until it is
ruled, only this page exists.

## Pages

`origin-token-upgraded`: an ERROR line
`page: an admitted origin token's implementation changed (its proxy emitted Upgraded). ... kind=origin-token-upgraded observation=<id> at=<coordinate> token=<token> implementation=<implementation>`
and an `event_log` entry of the same kind. Once per observation.

### A page for a log that is not final

The page fires when the observation is recorded, before it is final: an upgrade is worth a look at once. A reorganised
log is observed again at its new coordinate, and that new observation pages too, so **one upgrade can page twice**. Two
pages naming the same token and the same implementation, from the same transaction, are one upgrade; check the one whose
observation is final (`observation_outcomes.outcome = 'final'`).

## Confirm

```sql
SELECT seq, appended_at, payload FROM event_log WHERE kind = 'origin-token-upgraded' ORDER BY seq DESC LIMIT 10;
```

Read the new implementation from the origin network's public data: the proxy's EIP-1967 implementation slot at the
block of the event, from both providers, and the implementation's verified source. Compare it with the facts the design
rests on (`features/F-018-directed-deposits/NOTES.md`, "Native USDC on Polygon supports permit"): the EIP-712 domain, the
permit forms, `transferWithAuthorization`, the `Transfer` event's shape.

## Do

1. Read the issuer's announcement of the upgrade, if any, and the new implementation's verified source.
2. If anything the bridge relies on changed (the events, `permit`, the domain, a new fee on transfer), **pause** the asset's
   routes (`operator_pause`) until it is understood: a pause stops new work and keeps observing; transfers in flight
   complete (`REQ-GV-1`).
3. If nothing changed that the bridge relies on, record what you checked (`operator_record`), naming the implementation.
4. Watch the reserve check (`reserve_equation_outcome`) for a divergence after the upgrade; it pages by itself
   (`reserve.md`).

## Never

- Never treat the upgraded token as another token, or the old one as still admitted elsewhere: it is one token of one
  issuer (`REQ-AS-3`).
- Never hold or return a directed deposit by hand because of an upgrade: no such path exists, and whether intake should
  hold is unruled (`d-f018-usdc-upgrade`). Pause the route instead.
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed: upgrade a test token's proxy on a fork, see the observation recorded as `token_upgrade`, the page made
once, and nothing paused.

Status: not exercised (NFR-9). Behaviour proven by the tests listed above, not by a run of this runbook.

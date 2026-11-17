<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
---
id: directed-return-failed
kind: procedure
covers: [REQ-RF-1, REQ-RF-2, REQ-RV-1, REQ-RV-2, REQ-GV-5, REQ-IC-3]
pages: [payout-refusal-alert, directed-returns-alert]
observables: [reviews_open, reviews_oldest_age_seconds, nonfinal_transfers, oldest_nonfinal_transfer_age_seconds, reserve_amount]
tests: [services/full-chain/tests/full_chain/directed.rs::s8_req_rf_2_a_return_that_fails_waits_in_review_and_is_paid_once_to_its_refund_address, services/core/tests/directed.rs::ac_a6_4_a_never_mintable_directed_arrival_is_returned_once_to_its_refund_address, services/core/tests/cancel.rs::req_rv_1_issue_35_a_repeated_refusal_pages_once_and_reaches_review_at_the_deadline]
exercised: yes
---
# A directed return that fails

## What it is

A directed arrival that can never be minted (a recipient bezent refuses by rule, an amount outside the route's limits)
is returned by bezent to the refund address its caller named in the `Directed` event, and to no other (amendment 0029,
A5; `REQ-IN-11`, `REQ-RF-2`). The return is a payout from the reserve like any other refund of an unassociated arrival,
with its own bridge reference (`bz-u-…`) and a `directed:` request reference ending in `:return`.

The payout can fail where a withdrawal's would not, because bezent did not choose the address: the token may refuse to
pay it. Native USDC reverts a transfer **to a blacklisted address**; a contract at the address cannot make a token
transfer revert, but a token with hooks could. Then the payout's dry run is refused on both providers, every time:
nothing is signed, nothing is sent, and the value stays in the reserve. The return is not lost and not failed: it is
**delayed**, with an owner (`REQ-IC-3`). At its deadline it goes to review, held at `Source-Final`, and waits there.

What bezent never does about it: pay the value anywhere else, return it on a timer, mint it, or count it as surplus. The
store refuses a return to any address but the recorded refund address, whoever writes (`REQ-RF-2`, `INV-2`).

## Pages

- `payout-refusal-alert`: the return's payout preparation was refused ten times in a row with no named condition. Once
  per attempt (`pages/payout.md`). Its `event_log` payload names the transfer (`bz-u-…`); the endpoint's dry run records
  the revert, which is the Roles modifier's, not the token's (below).
- `directed-returns-alert`, when returns cross their threshold (`pages/directed-returns.md`): a failing return is one of
  them.
- No page when it enters review: `reviews_open` rises and `reviews_oldest_age_seconds` grows.

## Confirm

The return, its state and why it was made:

```sql
SELECT t.id, t.bridge_reference, t.state, t.condition, t.destination, e.payload ->> 'reason' AS why
FROM transfers t JOIN event_log e ON e.transfer_id = t.id AND e.kind = 'directed-return'
WHERE t.kind = 'refund' AND t.request_reference LIKE 'directed:%' AND t.state NOT IN ('Completed', 'Refused', 'Refunded', 'Expired', 'Withdrawn')
ORDER BY t.id;
```

What the dry run said, on both providers, for its latest preparations:

```sql
SELECT p.id, p.dry_run_block_number, p.dry_run_passed, p.dry_run ->> 'reason' AS reason, p.dry_run -> 'runs' AS runs
FROM prepared_transactions p JOIN intents i ON i.id = p.order_intent_id
WHERE i.transfer_id = <id> ORDER BY p.id DESC LIMIT 5;
```

The dry run does **not** name the token's reason. The vault's Roles modifier runs the payout with `shouldRevert`, so a
transfer the token refuses comes back as the modifier's own error, `ModuleTransactionFailed()` (selector `0xd27b44a9`,
shown as `custom error 0xd27b44a9`), whatever the token said. So read the cause on the origin network's public data,
from two providers: for native USDC, `isBlacklisted(<refund address>)` on the token is `true`. A reason that is not the token's refusal of the address (a fee, a provider, the
delivery allowance) is not this runbook: follow `pages/payout.md` or the condition it names.

Check the reserve is whole: the value is still in the vault and the reserve check holds.

```sql
SELECT metric, value FROM ops_observables() WHERE metric IN ('reserve_amount', 'reserve_equation_outcome', 'reviews_open');
```

## Do

1. **Leave it.** A return the token refuses waits; there is nothing to pause and nothing to move. Nothing is signed
   while it is refused.
2. Record what you found the same day (`operator_record`): the transfer, the refund address, the token's reason, and
   that the value is held in the reserve.
3. Tell whoever asks about the deposit that its return is **delayed**, not failed, and why: the address it named for
   refunds cannot receive the token. Only the token's issuer can lift a blacklist; bezent cannot, and does not ask.
4. At its deadline it enters review, held at `Source-Final`. When the address can receive again (read `isBlacklisted`
   again, on two providers), resolve the review into a **new attempt**: one operator records the resolution
   (`manual_actions`, `kind = 'review-resolution'`, `to_state = 'Delivering'`, on a final head of the origin network,
   naming what changed), and a second operator approves it (`manual_approvals`). The core then orders the return again,
   to the same address, and it is paid once.
5. If the address will never receive again, the review stays open and the value stays in the reserve. Escalate to the
   spec owner with the transfer's reference, the refund address, the token's reason and the arrival's coordinate. What
   becomes of it is the owner's decision; no path in bezent sends it elsewhere.

## Never

- Never pay a return to any address but the refund address the `Directed` event named, not "the sender", not the
  user's new address, not a support address: there is no path, and the store refuses one (`REQ-RF-2`, `INV-2`).
- Never resolve the review into a new attempt while the address still cannot receive: it is refused again and pages
  again.
- Never mint the arrival instead, or call it surplus: it was returned because it can never be minted.
- Never report it as failed (`REQ-IC-3`), and never refund anything on a timer (`REQ-RF-1`).
- No administrative path moves value or edits a status (`REQ-GV-5`).

## Exercise

On the test bed (`s8` of the full-chain suite does each step): declare a recipient refused, blacklist a refund address on
the forked token (its blacklister impersonated), send a directed deposit naming both; see the return ordered, its dry run
refused (`ModuleTransactionFailed()`) and nothing signed, one `payout-refusal-alert`, review at the deadline with the value in
the vault and the reserve check at zero divergence; lift the blacklist, resolve into a new attempt with two operators (one
alone moves nothing), and see the return paid once to the same address.

Status: exercised on the full-chain bed on 2026-11-17 (`EXERCISES.md`; NFR-9), by an automated scenario, with the Kreivo half a
model. The `payout-refusal-alert` is the core's own, raised by its payout loop in the scenario. Not yet on Kreivo's own runtime.

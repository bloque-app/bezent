<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-006 — notes

Reading of §7.F (orchestrator, 2026-10-13): "Pause / unpause — the pause role / governance" is read as pause by
either, unpause by governance only, because `AC-I2.2` needs more than one approver for an unpause.

## Readings made while building (implementer, 2026-10-13)

Each is a reading of the spec or plan, not a ruling. The ones marked **for review** are where the plan and the spec
could be read apart.

1. **Refund mint is not stopped by a pause (for review).** The plan's table refuses `refund_mint` when the asset is
   paused. §7.F lists only "refunded before" and "the mint window would be exceeded" for a refund mint, and
   `REQ-GV-1` says transfers in flight MUST keep being completed under a pause. A refund mint completes a withdrawal
   already in flight, so the contract follows the spec: no pause stops it. The pause role can still stop refunds at
   once by lowering the asset's mint window (`lower_bounds`). Test: `req_gv_1_a_pause_does_not_stop_a_refund_mint_in_flight`.
   If the plan's reading is wanted, it is one line in `refund_mint` (`ensure_running(asset, Direction::Mint)`).
2. **Refund mint respects the supply ceiling.** §7.F does not list it, but `INV-3` holds at all times.
3. **Redemption minimum per asset, not per route.** §7.F says "the route's minimum"; the plan keeps the minimum per
   asset. A route is (asset, origin network, direction), and since amendment 0007 every payout is made on the
   reserve's network (`REQ-OT-4`), one per asset (`REQ-AS-8`). So each asset has one withdrawal route, and the asset's
   minimum is that route's minimum.
4. **Pause scopes.** §7.F says per asset, per direction, or all; the plan says one asset and direction. The contract
   has all four: `All`, `Direction(d)` (every asset), `Asset(a)`, `AssetDirection(a, d)`. A pause per route is the
   core's (the contract has no routes).
5. **What "lowering" means per field.** `lower_bounds` takes effect at once only if no field loosens: supply ceiling,
   window amount and per-transfer maximum equal or lower; window length equal or longer; redemption minimum equal or
   higher. Anything else is a raise and goes through `schedule_raise` / `apply_raise` after the delay.
6. **A lowering cancels a scheduled raise** of the same asset (event `RaiseCancelled`), so a raise decided before an
   incident is not applied over the incident's lowering. Governance schedules again afterwards. Stated plainly (review
   N1): a "lowering" to the same bounds also cancels, so the pause key alone can veto every raise of a known asset
   for as long as it keeps doing so. Governance's answer is to replace the pause key (`set_roles`).
7. **Unknown assets.** An asset is known once it has a configuration (bounds, backends, initial supply): from
   instantiation, or through `schedule_raise` and `apply_raise` after the delay (adding an asset is a raise from
   nothing).
8. **The raise delay** is set at instantiation and changes only through `schedule_raise_delay` / `apply_raise_delay`,
   which wait the current delay. It is never zero, and never above `MAX_RAISE_DELAY` (see S6 below).
9. **Governance's "more than one approver"** (`REQ-GV-2`, `AC-I2.2`) is the governance account's own property: on
   Kreivo, the account of the community dedicated to the contract, whose calls pass its vote (`REQ-HN-10`). The
   contract enforces that only that account unpauses, raises and sets roles, and that it is not the delivery or
   pause key (`REQ-HN-9`).
10. **Supply** is kept by the contract, since Kreivo's assets API gives contracts no total-issuance read: the
    asset's initial supply when added, plus what the contract minted, less what it destroyed. The initial supply is
    the issuance that existed before the asset's Issuer and Admin moved to the community (bUSDC, bEURC, bBTC on
    mainnet, `DEC-7`; COPM, `M-3`). F-015 must pass the asset's total issuance at that moment.
11. **Zero mints are refused** (`ZeroAmount`): they would only consume a reference.
12. **Asset id** is `Here(u32)` only, encoded as Kreivo's `FungibleAssetLocation::Here`: contracts can mint and burn
    only local assets (kreivo#512, `Config::MintableAssets`).
13. **`kreivo-apis` is a dev-dependency**, pinned at the kreivo#512 merge (`87f8728`, tag 0.17.1), used to pin the
    contract's own binding (status codes, asset id). The contract does not link it: it pulls `frame-support`, which
    does not build into the contract's WASM (as in aledgent's settlement contract).

## Review fixes (PR #20, 2026-10-13)

What changed after the security review on PR #20, and why. The storage layout changed with it; nothing is deployed
yet, so no migration is needed.

- **M1: separate reference namespaces** (`REQ-ID-4`, `INV-2`). Mints and redemptions shared one set of used
  references, so anyone who computed a deposit's on-chain reference (derived from the arrival's public coordinate)
  could redeem the minimum under it and make that deposit's mint fail forever. Mint references now have their own
  set, which only the delivery role writes. A redemption is keyed by **(redeemer, reference)**: nobody can consume a
  mint reference or another redeemer's, and the same redeemer still cannot reuse one (`AC-B5.2`). `refund_mint`
  takes the redeemer and the reference, and the core binds a withdrawal by that pair (still by reference, never by
  amount or time, `REQ-OT-2`). `Redeemed` and `RefundMinted` carry the redeemer as a topic. This also ends the
  redemption front-running listed under limits.
- **M2: a sliding mint window** (`INV-3`, "mints in **any** window"). The fixed window let the delivery key mint twice
  the bound in two blocks across a window edge. The window is now up to 11 buckets of `ceil(window_blocks / 10)`
  blocks: a mint at block `t` is let through only if it, plus every bucket whose last mint lies in the
  `window_blocks` blocks ending at `t`, stays within `window_amount`. No span of `window_blocks` blocks can exceed
  the bound; a mint may be refused up to one bucket (a tenth of the window) late. Refund mints count. The proof is
  on `types::Window`. An amendment to make the window fixed was the alternative; it is not needed.
- **S1: the raise delay is never zero** (`REQ-GV-3` needs a delay), at instantiation and on change.
- **S2: each asset declares its backends** (`REQ-OT-4`). An asset's configuration is its bounds plus the backends
  its redemptions may name (one to four), changed only through the delayed path; `redeem` refuses any other backend
  and an EVM payout to the zero address. bUSDC naming chain 1, or an attested backend that does not exist, is
  refused before anything moves.
- **S3: governance cannot lock itself out by accident** (`REQ-HN-9`, `REQ-GV-3`). No role may be the all-zero account
  or the contract's own. `set_roles` replaces delivery and pause at once but never governance; a new governance
  account is scheduled (`schedule_governance`), waits the raise delay, and takes over only when it accepts
  (`accept_governance`), which proves it can act. Until then the current governance governs and can withdraw the
  change by scheduling itself.
- **S4: initial supply** (`INV-3`). Option (a) of the review: the configuration takes the supply that already exists
  of the asset, at instantiation or when a delayed raise adds it, and the counter starts from it. A known asset's
  counter is never set by governance (`REQ-GV-5`).
- **Whole-balance redemption** (orchestrator, plan level). `redeem` first tries `Assets::transfer_approved` through
  `call_runtime` (Kreivo's contracts `CallFilter` allows any `Assets` call; pallet-assets runs it with
  `keep_alive: false`), out of the caller's approval of the contract. If the caller has no approval covering the
  amount, the dispatch is refused and changes nothing, and `assets__deposit` pulls as before. Both paths keep the
  order: pull, check the contract's own balance rose by exactly the amount, burn from the contract's own account
  (`REQ-HN-8`). A holder redeems everything by batching `approve_transfer` and `redeem`; they pay the approval
  deposit, returned when the approval is used up. If the approved pull would leave a rest below the minimum balance,
  pallet-assets moves the rest too, the balance check fails and the redemption reverts: redeem the exact balance.
  ink!'s off-chain engine cannot dispatch runtime calls, so the unit tests send the encoded call to the mock; the
  real dispatch belongs in F-013's zombienet run, with the review's N3 (a forced `DepositMismatch` reverts).

## Second review (PR #20, 2026-10-13)

- **S5: a longer window stalls mints** (`INV-3`). Buckets are dropped under the window in force, so lengthening the
  window (a lowering, or a raise that also lengthens it) used to let new mints into a span that already held the
  bound: at 100 000 per 10 blocks, 100 000 at block 10 and 1 at block 20, then a 100-block window, let 99 999 more
  through at block 21. The window now remembers the newest mint it dropped, at block `d`. When the window grows from
  `W` to `W'` at block `n` and `d` is inside the new window, the asset mints nothing, refund mints included, until
  block `d + W' - 1`: at most `W' - W` blocks. It is a bucket counting `Balance::MAX` at block `d`, which ages out on
  its own, so a later shortening ends it sooner and a later lengthening keeps it as long as needed;
  `minted_in_window` reads `Balance::MAX` meanwhile. Deposits and refunds wait, they do not fail (`REQ-GV-1`). The
  alternative, amending `INV-3` to count a lengthened window only from `W` blocks before the change, was not needed.
  Tests: `inv_3_lengthening_the_window_stalls_mints_until_what_it_dropped_has_left_it`,
  `inv_3_mints_in_any_window_never_exceed_the_bound_when_the_window_changes_length`.
- **S6: the raise delay is capped** at `MAX_RAISE_DELAY` = 403 200 blocks, 28 days of 6-second blocks, at
  instantiation and in `schedule_raise_delay` (`DelayTooLong`). Every delayed path waits the delay: raises, new
  assets, a change of the delay itself, a governance handover. With no cap, a delay near `BlockNumber::MAX` made
  every later schedule overflow, and a merely long one (years) froze them just the same, since shortening the delay
  waits the current one; only pausing and lowering would have been left until a code upgrade. 28 days is a
  placeholder long enough for any review a raise needs; F-015 may set another value. Test:
  `req_gv_3_a_raise_delay_above_the_cap_is_refused_at_instantiation_and_on_change`.

Not taken here: N4 (lengthening the delay could apply at once) and N5 (pin workflow actions by SHA); N6 (no other
contract under the community may use `call_runtime` on `Assets`) belongs to F-015's deployment checklist. The
feature's `plan.md` message table predates these changes: `refund_mint(redeemer, reference)`,
`schedule_governance` / `accept_governance`, the asset configuration and the reads `mint_reference_used`,
`redemption(redeemer, reference)`, `backends` are the contract's now.

## Limits stated plainly

- **A whole-balance redemption takes two calls** (approve, then redeem, which can be batched) and an approval deposit.
- **Stray value sent to the contract** is burnt with the next redemption only if what is left falls below the
  minimum balance (Kreivo's burn is expendable); the contract counts what Kreivo reports burnt.

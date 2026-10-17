<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Traceability: the requirements note → SPEC.md

Informative. Maps every requirement of `notes/BRIDGE-REQUIREMENTS.md` (`BR-*`, aledgent PR #100) to the
identifiers of `SPEC.md` v0.1.0 that satisfy it. Where the spec departs from the note, the row says so
and why. The note is written against Kreivo and aledgent by name; the spec names neither (`notes/CONSTRAINTS-FOR-PLAN.md`
binds them).

## Requirements

| BR | SPEC | Note |
|---|---|---|
| `BR-KR-1` | `REQ-LG-1`, `REQ-AR-2` | |
| `BR-KR-2` | `REQ-IN-5`, `REQ-LG-2`, §7.F "mint into a ledger" | Answers note Q4 as *yes*, atomically with the mint. @pandres95 to confirm |
| `BR-KR-3` | `REQ-PV-4`, `INV-12` | |
| `BR-KR-4` | `REQ-OT-1`, `REQ-OT-2`, `REQ-ID-2` | |
| `BR-OUT-1` | §2 Disposition, `US-C1` | |
| `BR-OUT-2` | `REQ-NX-3`, `REQ-IC-3` | |
| `BR-OUT-3` | `REQ-RF-1`, `REQ-NX-1`, `INV-8` | |
| `BR-OUT-4` | `REQ-ST-3`, `REQ-RQ-7` | |
| `BR-OUT-5` | `REQ-TR-4` | |
| `BR-OUT-6` | `REQ-TR-1`, `INV-5` | |
| `BR-OUT-7` | `REQ-TR-5`, `REQ-RB-5`, `REQ-BK-4`, `DEF-4` | **Stricter:** bezent charges no fee at all; costs fall on surplus |
| `BR-OUT-8` | `REQ-OB-6`, `REQ-TR-3` | |
| `BR-NX-1` | `REQ-NX-1`, `REQ-HN-3`, `REQ-RN-2` | |
| `BR-NX-2` | `REQ-NX-2` | |
| `BR-NX-3` | `REQ-NX-3` | |
| `BR-NX-4` | `REQ-NX-4`, `NFR-2` | **Partial:** origin networks get a forced nonce consumption; the issuing network's expiry window is a plan choice bounded by `NFR-2` |
| `BR-ID-1` | `REQ-ID-1`, `REQ-RQ-1` | |
| `BR-ID-2` | `REQ-ID-2`, `REQ-RN-5`, `REQ-HN-7` | Arrivals from funders carry no reference; the deposit request names their coordinate, which derives bezent reference (`REQ-IN-3`, `REQ-RQ-3`) |
| `BR-ID-3` | `REQ-ID-3` | |
| `BR-ID-4` | `REQ-ID-4` | |
| `BR-EO-1` | `REQ-EO-1`, `REQ-EO-6`, `INV-9` | |
| `BR-EO-2` | `REQ-EO-2`, `REQ-AR-4`, `INV-2` | |
| `BR-EO-3` | `REQ-EO-3`, `REQ-OB-3` | |
| `BR-EO-4` | `REQ-IN-1`, `INV-10` | |
| `BR-EO-5` | `REQ-ST-4` | |
| `BR-EO-6` | `REQ-EO-4` | |
| `BR-EO-7` | `REQ-EO-5` | |
| `BR-FIN-1` | `REQ-OB-1` | |
| `BR-FIN-2` | `REQ-OB-2`, `REQ-IN-2` | |
| `BR-FIN-3` | `REQ-OB-4`, `REQ-IN-7` | |
| `BR-FIN-4` | `REQ-OT-3`, `INV-6`, `INV-7` | |
| `BR-FIN-5` | `DEF-1`, `OQ-2` | Fast path deferred |
| `BR-FIN-6` | `REQ-OB-5`, `INV-16` | |
| `BR-RF-1` | `REQ-RF-1`, `INV-8` | |
| `BR-RF-2` | `REQ-RF-2`, `REQ-RQ-4` | Answers note Q5: the requester names it per request |
| `BR-RF-3` | `REQ-RF-3` | |
| `BR-RF-4` | `REQ-RV-1` | |
| `BR-OBS-1` | `REQ-ST-1` | |
| `BR-OBS-2` | `REQ-ST-2`, `REQ-ST-5` | |
| `BR-OBS-3` | `REQ-TR-2` | |
| `BR-OBS-4` | `REQ-ST-7` | |
| `BR-OBS-5` | `REQ-ST-6` | |
| `BR-LIM-1` | `REQ-LM-1` | |
| `BR-LIM-2` | `REQ-LM-2` | |
| `BR-LIM-3` | `REQ-LM-3`, `REQ-OT-5` | |
| `BR-LIM-4` | `REQ-LM-4` | |
| `BR-LIQ-1` | `REQ-BK-1`, `INV-1` | The note's "backing = supply + in flight" becomes the reserve equation of §5.3, with surplus |
| `BR-LIQ-2` | `REQ-BK-2` | |
| `BR-LIQ-3` | `REQ-BK-5` | Every reserve address is published. `REQ-PV-5` (deposit addresses in aggregate) was withdrawn by amendment 0007 (A1): deposit addresses are the requester's |
| `BR-LIQ-4` | `REQ-IN-4`, `REQ-AR-4`, `INV-3` | **Departs:** the issuing network cannot see reserves, so on-chain mint authority is bounded by a governance-set ceiling and window, not by backing. §4.3 says so |
| `BR-LIQ-5` | `REQ-IN-6` | |
| `BR-LIQ-6` | `REQ-PR-1`, `REQ-PR-2` | |
| `BR-LIQ-7` | `REQ-RB-*` | Rebalancing is per request, not per transfer |
| `BR-LAT-1` | `NFR-2` | |
| `BR-LAT-2` | `NFR-3`, `REQ-TR-2` | |
| `BR-LAT-3` | `NFR-3` | |
| `BR-LAT-4` | `NFR-4` | |
| `BR-KEY-1` | `REQ-KY-1`, `REQ-CU-1` | |
| `BR-KEY-2` | `REQ-AR-3`, `INV-9` | |
| `BR-KEY-3` | `REQ-KY-3`, `REQ-RN-4`, §7.F bounds | |
| `BR-KEY-4` | `REQ-KY-4`, `REQ-HN-9` | |
| `BR-KEY-5` | `REQ-KY-5` | |
| `BR-KEY-6` | `REQ-KY-6` | |
| `BR-GOV-1` | `REQ-GV-1`, `INV-15` | |
| `BR-GOV-2` | `REQ-GV-2` | |
| `BR-GOV-3` | `REQ-GV-3` | |
| `BR-GOV-4` | `REQ-GV-4` | |
| `BR-GOV-5` | `REQ-GV-5`, §7.D | |
| `BR-PRV-1` | `REQ-PV-1`, `INV-12` | |
| `BR-PRV-2` | `REQ-PV-2` | |
| `BR-PRV-3` | `REQ-PV-3` | |
| `BR-INC-1` | `REQ-IC-1` | |
| `BR-INC-2` | `REQ-IC-2` | |
| `BR-INC-3` | `REQ-IC-3` | |
| `BR-INC-4` | `REQ-IC-4` | |
| `BR-INC-5` | `REQ-IC-5` | |
| `BR-INC-6` | `REQ-IC-6`, `NFR-9` | |

## Scope differences

- The note's §1 lists **Kusama Asset Hub** among remote networks. Owner ruling 2026-10-01: Asset Hub is out
  of scope; XCM serves it (`SPEC.md` §1.6, §13). Every `BR-*` still applies to the networks bezent does
  serve.

## The note's open questions

| Note | SPEC |
|---|---|
| Q1 fast path | `OQ-2`, `DEF-1` |
| Q2 latency budgets (answered 2026-09-29) | `REQ-LG-3`, `NFR-2` |
| Q3 pass-through or inventory | `OQ-13` |
| Q4 direct delivery into aledgent | `REQ-IN-5` (proposed yes) |
| Q5 refund destination | `REQ-RF-2` (the requester names it per request) |
| Q6 review owner | `OQ-3` |
| Q7 mint authority on Kreivo | `OQ-5` |

## What the spec adds that the note does not ask for

The reserve equation with surplus (§5.3), escrow-until-final on withdrawals (§5.6), successor backing
(`REQ-AS-6`), and the issuer constraints `REQ-AS-3` and `REQ-AS-4`.

Amendment 0007 withdrew deposit addresses and sweeps (§5.2, A1) and payouts through a venue to another network
(§5.7, A5): getting value into a reserve, and delivering it to another network, are the requester's.

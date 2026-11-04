<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-012 — the attesting contract

What the `bezent` module in the Swaps platform must send for COPM, MXNB and BRLB (`DEC-14`, `F-011`). The module stays a
draft by ruling (`RISK-8`), so this is the contract its reports side implements later; bezent's suite drives the same
surface directly. `services/ingress/openapi.json` is the authoritative shape of the HTTP surface.

## Who may report

The requester must hold the `attest` grant and be the declared attesting party of the backend the report names. A
requester declared for one backend is refused on another, so Bloque Test's grant on the one instance (`DEC-15`) can
never report for a production backend. To relay instructions it also needs the `withdrawal` grant.

## The envelope

`POST /v0/reports`, with the usual `Authorization: Bearer <credential>`:

```json
{ "report": "<the report, a JSON document, as a string>",
  "signature": "0x<64 bytes>",
  "public_key": "0x<32 bytes>" }
```

- `signature` is ed25519 over `bezent-attested-report-v1`, one zero byte, then the UTF-8 bytes of `report`, exactly as
  sent. There is no canonical form to reproduce: the bytes are signed as they are.
- `public_key` must be one declared for the backend. A new key is declared beside the old one, then the old one retired,
  so reports in flight survive a rotation.
- Swaps' signing key belongs to Swaps. bezent holds only the public half.

## The report

Common fields of the document. Unknown fields are refused.

| Field | Type | Meaning |
|---|---|---|
| `version` | number | `1` |
| `report_id` | string | Swaps' own identifier, at most 256 bytes. It is the request reference: unique for this requester forever (`REQ-ID-1`) |
| `network` | string | The backend, as bezent declares it |
| `kind` | string | One of the kinds below |
| `ids` | object of strings | Optional. Swaps' other identifiers (order, rail reference), recorded with the observation (`REQ-BE-4`). At most 16 entries. **Never personal data**: the module sends references, not people |
| `occurred_at` | RFC 3339 | Optional. When it happened, by Swaps' clock; recorded, never used to decide anything |

Amounts are decimal strings of exact integer minor units of the origin token (`INV-11`).

| `kind` | Fields | When to send it |
|---|---|---|
| `arrival` | `asset`, `token`, `amount`, `position`; optional `funder` | The payment is final under its rail's rules, and not before (`REQ-OB-7`) |
| `payout` | `asset`, `token`, `amount`, `position`, `bridge_reference` | The fiat payout of a withdrawal is final on its rail. The amount is the withdrawal's |
| `positions` | `asset`, `token`, `positions` (array of `position`, `amount`), `through` | Regularly, within the backend's declared bound |
| `reversal` | `asset`, `token`, `amount`, `position`, `reverses` | A payment reported final was reversed: a chargeback or a fraud recall (`REQ-BE-6`) |
| `restoration` | `asset`, `token`, `amount`, `position`, `restores` | The reserve a reversal took has been put back, in full |

- `position` names one of Swaps' accounts (`REQ-AS-8`). The account's name is public in the statement.
- `funder` marks the operator's own money: an arrival from an address declared as a funding address of the reserve is
  a contribution to surplus and no deposit can bind it (`REQ-BK-5`).
- `reverses` is the `report_id` of an `arrival` of the same asset; several reversals of one arrival may not exceed it.
- `restores` is the `report_id` of a `reversal`, and `amount` must equal it. Only the party that reported it restores it.
- `through` is the `report_id` of the last report the balances reflect; `null` when none. **A snapshot must be computed
  from Swaps' own record of what it reported, not read from the bank.** A payment the bank shows and Swaps has not
  reported yet would read as a difference, and any difference pauses the asset (`REQ-BK-1`).

## Answers

| Answer | Meaning |
|---|---|
| 201, `state: "Recorded"` | Taken in. Carries the report's `coordinate` |
| 200 | A repeat of the same bytes: the original answer |
| 201, `state: "Refused"` with `error` | Recorded as refused, the reference is spent: `ERR-UnknownRoute` (the asset, token and network do not go together), `ERR-SourceNotFound` (it names something that is not there), `ERR-SourceMismatch` (the amount does not fit), `ERR-SourceConsumed` (already paid out, or already restored) |
| 400 `ERR-Malformed` | Nothing recorded |
| 401, 403 `ERR-NotAuthorised` | No credential, no grant, not this backend's party, an undeclared key or a signature that does not verify. Nothing recorded |
| 409 `ERR-ReferenceConflict` | The `report_id` was used with other bytes |
| 503 `not-final` | A `payout` for a redemption that is not final yet. Nothing recorded; send the same bytes again |
| 503 `ERR-Overloaded`, `outcome-unknown` | Send the same bytes again |

## The flows

**Deposit.** Report the `arrival`; take `coordinate` from the answer; `POST /v0/deposits` naming it. A deposit sent before
the core has recorded the report gets `not-final`, with nothing recorded and the reference unspent: send it again.
bezent mints only what the report says, within the contract's mint window and ceiling.

**Withdrawal.** The redeemer redeems on the issuing network with a commitment: BLAKE2b-256 of the SCALE-encoded payout
instructions. bezent never decodes the bytes, so their layout is Swaps'; only the hash binds. Personal and bank data never
reach the issuing network (`REQ-PV-1`). Relay the bytes with `POST /v0/withdrawals/extras`, by the redemption's on-chain
reference (`instructions`, hexadecimal). Its answer carries the withdrawal's `bridge_reference`; from then on the
withdrawal reads through the status and the log like any of the module's. Pay on the rail, and report the `payout` under
that `bridge_reference`. `ERR-CommitmentMismatch` means the bytes are not the ones committed to: do not pay. A relay
that names a redemption bezent has not observed yet is refused with `ERR-SourceNotFound` and spends its reference, so wait
for finality on the issuing network before relaying.

**After the payout.** The withdrawal holds at `Source-Final` with its payout in the book, until the owner rules on the
state after `Source-Final` for an attested backend (`amendments/0016-attested-delivery.md`). A report of a payout that
failed on the rail, and the refund mint behind it, wait on the same ruling; no such kind is taken yet.

**Reversal.** Report it as soon as it is known; restore within one business day (`REQ-BE-6`). Past that, bezent pages. While
the surplus is under its floor, new mints of the asset hold `CND-Paused`. Holders keep what they hold.

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Licensing

Which licence covers which part of bezent, and why. Every tracked file carries an SPDX header
(`tools/oss/spdx.py --check`) or, where its format takes no comment, an annotation in `REUSE.toml` (`reuse lint`). The
copyright holder is Pagos Cube LLC, Bloque's operating company, as in aledgent. **Status: DEC-20's default, to be
confirmed by counsel.**

## 1. The table

| Part | Path | Licence | Why |
|---|---|---|---|
| Services | `services/**` | **AGPL-3.0-or-later** | DEC-20: whoever runs the bridge as a network service owes its source to its users. They link smoldot (GPL with the Classpath exception, §4) |
| The store | `crates/store/**` | **AGPL-3.0-or-later** | bezent's own schema and triggers, not a reusable library. Its migrations carry an AGPL header they were applied with: sqlx records each file's checksum and refuses a database that applied a file whose bytes later changed, so the header cannot be edited. Counsel or the owner may decide to re-header them with a database migration plan |
| The Kreivo contract (source) | `contracts/bezent/**` | **AGPL-3.0-or-later** (source) | DEC-20. The *built* contract is a GPL-3.0-only combined work: §4.3 |
| Reusable crate | `crates/core/**` | **Apache-2.0 OR MIT** | Types for amounts, ids, references and coordinates; depends on no AGPL code |
| EVM contracts | `contracts/evm/{src,script,test}/**` | **Apache-2.0 OR MIT** | Reusable vault glue; the libraries under `lib/` are submodules with their own licences |
| Test bed, tooling, workflows, research scripts | `e2e/**`, `tools/**`, `.github/**`, `deploy/**`, `research/**-scripts` | **Apache-2.0 OR MIT** | Nothing in them links AGPL code |
| Documents | `*.md`, `docs/*.html` | **CC-BY-4.0** | Prose, wherever it lives |
| Kreivo's chain specification | `services/kreivo-endpoint/chain-specs/kreivo.json` | **GPL-3.0-only** (Kreivo's) | Published by Kreivo, kept verbatim |
| Marks | `brand/*.svg` | `LicenseRef-Trademark-Marks` | Pagos Cube LLC's; not licensed with the code |
| Test fixtures | `services/*/fixtures`, `crates/core/tests/ui` | as the code beside them | Published test values, never a deployment's |

No crate outside `services/`, `crates/store` and `contracts/bezent` depends on AGPL code, so the permissive licences
are real; `cargo deny check licenses` (the `licensing` job of CI) keeps every dependency on an allow list.

## 2. Contributions

Inbound = outbound, under the Developer Certificate of Origin (`CONTRIBUTING.md`). There is no contributor licence
agreement.

## 3. The migrations

The files in `crates/store/migrations/` must not be edited once any deployment has applied them. They keep their
AGPL-3.0-or-later header; `REUSE.toml` adds the copyright line. `tools/oss/spdx.py` skips the directory.

## 4. Third-party code

### 4.1 Light client

`smoldot` and `smoldot-light` (GPL-3.0-or-later WITH Classpath-exception-2.0), from `bloque-app/smoldot` pinned by
commit (`Cargo.toml`), link into `services/kreivo-endpoint`. The exception allows linking into an independent module
under any licence; the services are AGPL-3.0-or-later, which is compatible.

### 4.2 Foundry libraries

`contracts/evm/lib/` holds forge-std, Safe's `safe-smart-account` and Zodiac's `zodiac-modifier-roles` as submodules,
each under its own licence (MIT or LGPL-3.0 for Safe's contracts). They are used as dependencies, not copied.

### 4.3 Flagged: the built ink! contract is GPL-3.0-only

`contracts/bezent` depends on `ink` 5.1.1, which pulls in `staging-xcm`, `xcm-procedural` (both GPL-3.0-only) and
`array-bytes` (GPL-3.0). The compiled contract (`.wasm`, `.contract`) is therefore a combined work: its source is
AGPL-3.0-or-later, GPLv3 section 13 lets it be combined with the GPL parts, and the artefact is distributed with the
GPL-3.0-only terms of those parts. `contracts/bezent/deny.toml` names the exceptions. Counsel confirms.

## 5. Obligations of whoever runs the services

Offer the source of the AGPL programs to the users who interact with them (AGPL section 13). The public repository
is that source; a deployment that runs a modified version offers the modified source. Container images are built
only from this repository's workflows and carry the revision label `org.opencontainers.image.revision`.

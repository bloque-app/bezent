<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Licensing

Which licence covers which part of bezent, and why. Every tracked file carries an SPDX header
(`tools/oss/spdx.py --check`) or, where its format takes no comment, an annotation in `REUSE.toml` (`reuse lint`). The
copyright holder is Pagos Cube LLC, Bloque's operating company, as in aledgent. **Status: DEC-20's table, signed off
by @olanod on 2026-10-17.**

## 1. The table

| Part | Path | Licence | Why |
|---|---|---|---|
| Services | `services/**` | **GPL-3.0-or-later** | DEC-20: whoever distributes the bridge, or an image of it, owes its source. They link smoldot (GPL with the Classpath exception, §4) |
| The store | `crates/store/**` | **GPL-3.0-or-later** | bezent's own schema and triggers, not a reusable library. Its migrations carry the same header (§3) |
| The Kreivo contract (source) | `contracts/bezent/**` | **GPL-3.0-or-later** (source) | DEC-20. The *built* contract is a GPL-3.0-only combined work: §4.3 |
| Reusable crate | `crates/core/**` | **Apache-2.0 OR MIT** | Types for amounts, ids, references and coordinates; depends on no GPL code |
| Reusable crate | `crates/health/**` | **Apache-2.0 OR MIT** | The liveness heartbeat the services' `health` subcommands read; std only, depends on no GPL code |
| EVM contracts | `contracts/evm/{src,script,test}/**` | **Apache-2.0 OR MIT** | Reusable vault glue; the libraries under `lib/` are submodules with their own licences |
| Test bed, tooling, workflows, research scripts | `e2e/**`, `tools/**`, `.github/**`, `deploy/**`, `research/**-scripts` | **Apache-2.0 OR MIT** | Nothing in them links GPL code |
| Documents | `*.md`, `docs/*.html` | **CC-BY-4.0** | Prose, wherever it lives |
| Kreivo's chain specification | `services/kreivo-endpoint/chain-specs/kreivo.json` | **GPL-3.0-only** (Kreivo's) | Published by Kreivo, kept verbatim |
| Marks | `brand/*.svg` | `LicenseRef-Trademark-Marks` | Pagos Cube LLC's; not licensed with the code |
| Test fixtures | `services/*/fixtures`, `crates/core/tests/ui` | as the code beside them | Published test values, never a deployment's |

No crate outside `services/`, `crates/store` and `contracts/bezent` depends on GPL code, so the permissive licences
are real; `cargo deny check licenses` (the `licensing` job of CI) keeps every dependency on an allow list.

## 2. Contributions

Inbound = outbound, under the Developer Certificate of Origin (`CONTRIBUTING.md`). There is no contributor licence
agreement.

## 3. The migrations

The files in `crates/store/migrations/` must not be edited once any deployment has applied them: sqlx records each
file's checksum and refuses a database that applied a file whose bytes later changed. They were re-headered from
AGPL-3.0-or-later to GPL-3.0-or-later once, before any production deployment (@pandres95 accepted re-running them
on test databases). `REUSE.toml` adds the copyright line; `tools/oss/spdx.py` skips the directory.

## 4. Third-party code

### 4.1 Light client

`smoldot` and `smoldot-light` (GPL-3.0-or-later WITH Classpath-exception-2.0), from `bloque-app/smoldot` pinned by
commit (`Cargo.toml`), link into `services/kreivo-endpoint`. The exception allows linking into an independent module
under any licence; the services are GPL-3.0-or-later, which is compatible.

### 4.2 Foundry libraries

`contracts/evm/lib/` holds forge-std, Safe's `safe-smart-account` and Zodiac's `zodiac-modifier-roles` as submodules,
each under its own licence (MIT or LGPL-3.0 for Safe's contracts). They are used as dependencies, not copied.

### 4.3 Flagged: the built ink! contract is GPL-3.0-only

`contracts/bezent` depends on `ink` 5.1.1, which pulls in `staging-xcm`, `xcm-procedural` (both GPL-3.0-only) and
`array-bytes` (GPL-3.0). The compiled contract (`.wasm`, `.contract`) is therefore a combined work: its source is
GPL-3.0-or-later, the combination is distributed under GPL-3.0-only, the narrower of the terms involved. `contracts/bezent/deny.toml` names the exceptions. 

## 5. Obligations of whoever distributes the services

Whoever distributes the GPL programs (or an image of them) offers the corresponding source (GPL section 6). The public repository
is that source; a deployment that runs a modified version offers the modified source. Container images are built
only from this repository's workflows and carry the revision label `org.opencontainers.image.revision`.

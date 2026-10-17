<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-007 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-007-01 | Service skeleton; subxt + patched smoldot; pinned chain spec; genesis-hash check | `DEC-5`, `AC-I4.1` | Wrong network refused at start-up |
| T-007-02 | Finalized-head follower; contract events as observations by coordinate; gap detection and fill | `REQ-OB-*`, `AC-D2.3`, `INV-10` | Gap test passes |
| T-007-03 | Mint and refund-mint submission: era, dry run, recorded bytes, signer link, signature verification, nonce lane | `REQ-EO-1`, `REQ-EO-4`, `REQ-HN-3`, `AC-I1.2` | No signature without a dry run; bytes persisted before submit |
| T-007-04 | Non-execution proof from chain data | `REQ-NX-1`, `REQ-NX-2`, `AC-C2.1` | Proof test passes |

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-005 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-005-01 | `bezent_signer` store role and signature table (migration), append-only | `REQ-AR-3`, `INV-9` | A store test shows the role can read intents and insert signatures, nothing else |
| T-005-02 | Authenticated core↔signer link | `REQ-AR-3`, `REQ-CU-1` | An unauthenticated caller is refused |
| T-005-03 | Key backend trait; file backend with network tag; Kreivo and EVM signing | `REQ-KY-1`, `REQ-CU-1`, `AC-I4.2` | A test key refuses a production network id |
| T-005-04 | Guards: unrecorded intent, reused one-time reference, window cap, Kreivo destroy-only-own | `INV-9`, `DEC-3` | One test per guard |
| T-005-05 | Restart and replay; unrecorded signature → compromise path | `AC-D2.1`, `AC-I1.2` | Both pass |
| T-005-06 | No key bytes in logs, errors or config dumps | `REQ-KY-1` | Log-scan test passes |

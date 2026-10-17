<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# F-002 — tasks

| ID | Task | Traces | Done when |
|---|---|---|---|
| T-002-01 | HTTP skeleton (axum or equivalent), config, health, graceful shutdown, OpenAPI generation | `DEC-15` | Service starts against the store; OpenAPI committed |
| T-002-02 | Requester credentials and per-kind grants | `REQ-PN-1`, `REQ-PN-2`, `REQ-ID-4` | Unauthenticated and out-of-grant requests refused; cross-requester reads return nothing |
| T-002-03 | Deposit, withdrawal-extras and refund requests, durable before answer, idempotent per (requester, reference) | `REQ-RQ-1`, `REQ-RQ-3`, `REQ-RQ-4`, `REQ-RQ-6`, `REQ-RQ-9`, `REQ-ID-1`, `REQ-RQ-7` | `AC-D1.1`, `AC-D1.2` pass; claimed outcomes refused; relayed instructions refused unless they hash to the commitment |
| T-002-04 | Route table, limits, backpressure with named errors | `REQ-LM-*`, §8.4 | `AC-G1.1`–`AC-G1.3` pass |
| T-002-05 | Queries: status by three keys, route table, reserve and equation reads, log from a position | `REQ-RQ-8`, `REQ-ST-4` | `AC-H1.1`–`AC-H1.3` pass |
| T-002-06 | Intake latency benchmark | `NFR-1` | Reported in the PR |
| T-002-07 | Webhooks: registration, signed at-least-once delivery with a dead-letter, conditions once per (condition, route) | `REQ-ST-5`, `REQ-IC-5`, `REQ-ID-4`, `DEC-15` | `services/ingress/tests/webhooks.rs` passes; OpenAPI committed (issue #2, M-1) |

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# Container health checks

The service images are distroless: no shell, no `curl`. So each service binary carries a `health`
subcommand, and a container health check runs the binary itself (issue #61). `health` exits 0 only
when the running instance in the same container is healthy, and 1 when it is not, within 5 s. It prints one
line, `<service>: healthy` or `<service>: unhealthy: <reason>`. The reason is fixed text: never a
path, a URL, a credential or a configured value, and a configuration it cannot read is reported as
such, without the parser's message (which could quote a line of the file). It opens no connection
to the store, a provider or the signer, and reads no secret file or key.

## How each service says it is healthy

| Service | Mechanism | Healthy means |
|---|---|---|
| `bezent-ingress` | `GET /health` on the loopback, at the port of `listen` in the same configuration file | The running ingress answers 200: it bound its address only after reaching the store and checking its login, and `/health` asks the store a query |
| `bezent-core` | A liveness file the cycle rewrites | The last cycle and the reserve check's listing both completed against the store, within three cycles plus 120 s |
| `bezent-signer` | A liveness file the signer rewrites every 10 s while its store answers, and the link socket | Login checked, socket bound and still there, store answering within the last 35 s |
| `bezent-kreivo-endpoint` | A liveness file each pass rewrites | The pass reached the finalized head through the light client (`CaughtUp`) and the store answered throughout, within three poll intervals plus 120 s |
| `bezent-evm-endpoint` | A liveness file each pass rewrites | The pass reached the final point both providers agree on (`CaughtUp`) and the store answered throughout, within three poll intervals plus 120 s |

Why a file, and not a probe: the core and the endpoints serve no port, and the signer's link refuses
a caller that runs as the signer's own user (`REQ-CU-1`, `REQ-AR-3`), which is the user the health
check runs as; a connection without a token is also a refusal the signer logs. Only the running
instance knows whether its store answers and whether it is at the head of its network, so it records
that, and `health` reads the file's age. The running service removes the file when it starts, so a file
left on a volume by an earlier run never passes.

A pass that is behind the head (still reading a backlog), that found a gap or no final point, or whose
store failed, does not beat; the file goes stale and `health` fails. The core's signer link is not part of
its health: the link is used when a statement falls due, once a day, and an overdue statement is
already paged (`REQ-PR-1`); restarting the core would not mend a signer that is down.

## The liveness file

Each service takes an optional `health_file` in its configuration (TOML for the core and the endpoints,
JSON for the signer). Left out, it is `/tmp/bezent-<service>.health`, which exists in the images
and is writable by their users. A task definition that sets `readonlyRootFilesystem` must mount a
writable volume and name a path on it as `health_file`. A beat that cannot be written is logged
at error level, and the check then reports the service unhealthy, which is true of it.

## The command

`health` takes the configuration path the service takes, and reads it the same way (so it finds the same
port, socket and `health_file`). Left out, the path is the one the image's `CMD` names. ECS does not
run the image's `ENTRYPOINT` for a health check, so the command names the binary:

| Service | `healthCheck.command` |
|---|---|
| ingress | `["CMD", "/usr/local/bin/bezent-ingress", "health", "/etc/bezent/ingress.toml"]` |
| core | `["CMD", "/usr/local/bin/bezent-core", "health", "/etc/bezent/core.toml"]` |
| signer | `["CMD", "/usr/local/bin/bezent-signer", "health", "/etc/bezent-signer/signer.json"]` |
| kreivo-endpoint | `["CMD", "/usr/local/bin/bezent-kreivo-endpoint", "health", "/etc/bezent/kreivo-endpoint.toml"]` |
| evm-endpoint | `["CMD", "/usr/local/bin/bezent-evm-endpoint", "health", "/etc/bezent/evm-endpoint.toml"]` |

Pass the path your task actually mounts; the ones above are the images' defaults, and `["CMD",
"/usr/local/bin/bezent-<service>", "health"]` alone uses them. Suggested timings: `interval` 30,
`timeout` 5, `retries` 3.

`startPeriod` must cover the service's start. The kreivo endpoint's first start syncs the light client
and walks the backlog from `start_block` to the finalized head before it is at the head and beats
(`max_walk`, `archive_rpc`); that can take longer than ECS's longest `startPeriod` (300 s). Until then
`health` fails, and ECS will replace the task once the start period is over. Size `start_block` and
`max_walk` so a first start fits, or start a new deployment's endpoint with the check turned off and
turn it on once it has caught up. A restart resumes from the anchors the store keeps.

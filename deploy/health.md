<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# Container health checks

The service images are distroless: no shell, no `curl`. So each service binary carries a `health`
subcommand, and a container health check runs the binary itself (issue #61). `health` exits 0 only
when the running instance in the same container is healthy, and 1 when it is not. It prints one
line, `<service>: healthy` or `<service>: unhealthy: <reason>`. The reason is fixed text: never a
path, a URL, a credential or a configured value, and a configuration it cannot read is reported as
such, without the parser's message (which could quote a line of the file). It opens no connection
to the store, a provider or the signer, and reads no secret file or key. The ingress command bounds
itself to 4 s; the file-based ones only read a small file, and ECS's `timeout` bounds them.

## How each service says it is healthy

| Service | Mechanism | Healthy means |
|---|---|---|
| `bezent-ingress` | `GET /health` on the loopback, at the port of `listen` in the same configuration file | The running ingress answers 200: it bound its address only after reaching the store and checking its login, and `/health` asks the store a query. A pool whose connections are all busy answers 200 `busy` (alive and loaded, not a reason to replace it); a store that cannot be reached answers 503 |
| `bezent-core` | A liveness file the cycle rewrites | The last cycle and the reserve check's listing both completed against the store, within three cycles plus 120 s |
| `bezent-signer` | A liveness file the signer rewrites every 10 s while its store answers, and the link socket | Login checked, socket bound and still there, store answering within the last 35 s |
| `bezent-kreivo-endpoint` | A liveness file each pass rewrites, and the light client's warm-up | The pass reached the finalized head through the light client (`CaughtUp`), or a walk toward it advanced (a step read and recorded blocks), and the store answered, within three poll intervals plus 120 s. During the light client's first sync it beats while the sync advances |
| `bezent-evm-endpoint` | A liveness file each pass rewrites | The pass reached the final point both providers agree on (`CaughtUp`), or a catch-up toward it advanced (a step read and recorded blocks), and the store answered, within three poll intervals plus 120 s |

Why a file, and not a probe: the core and the endpoints serve no port, and the signer's link refuses
a caller that runs as the signer's own user (`REQ-CU-1`, `REQ-AR-3`), which is the user the health
check runs as; a connection without a token is also a refusal the signer logs. Only the running
instance knows whether its store answers and whether it is at the head of its network, so it records
that, and `health` reads the file's age. The running service removes the file when it starts, so a file
left on a volume by an earlier run never passes.

An endpoint beats on every step that read and recorded blocks, on each prepared or signed order at the
head, and when a pass reaches the head, but only while the pass before it did not fail: a store or prover
that keeps failing cannot stay green on a walk's progress. So a
walk that moves is healthy, a first start that walks a long backlog included, and a walk that is stuck
(a gap, no finalized head or no final point both providers agree on, or a store that fails) stops
beating and the file goes stale. A stuck endpoint needs the alarm, not restarts: a persistent `Gap` or
provider disagreement is not mended by replacing the task, so the remedy is to act on the alert, and the
task is replaced meanwhile only because its check fails.

## The first start of the kreivo-endpoint

A first start can outlast ECS's longest `startPeriod` (300 s), in two stretches, and both beat while they
make progress.

- **The light client's first sync.** Kreivo is a parachain: its own sync reports ready at once, and the
  long part is the relay chain's warp sync. The endpoint watches the light client's bootstrap state of
  both chains and beats when either moves forward: the first state with peers, a warp sync that reached a
  higher block, the sync that finished with peers. A bootstrap without peers, a stalled one (no peers, or
  a warp sync that has not advanced for 45 s) and one that stands still do not beat.
- **The walk from the finalized head to `start_block`.** The walk keeps an anchor every few blocks in the
  store as it goes, so every 5 s the endpoint reads the lowest anchor stored, and beats when it went
  down (the query answering also shows the store is up). A walk that stands still does not beat.

A warm-up with no progress for the usual window (three poll intervals plus 120 s) goes stale and the
task is replaced. The evm-endpoint has no comparable warm-up: its start-up is a handful of HTTP calls to
the providers, each with its own timeout, and no multi-minute sync.

What is not covered: the start-up checks between the two (the chain and the contract) and the history
sources' check after the walk (`check_history`, over the archive nodes) do not beat. They are bounded
by their own timeouts, but `startPeriod` should leave room for them.

## What the checks do not cover

- **The signer**: healthy means its store answers and its socket exists. It does not mean the keys sign,
  or that a caller can complete a round trip.
- **The ingress** ignores the webhook dispatcher: a dispatcher that is stuck or failing does not make
  `/health` fail. And `busy` (every store connection in use) cannot tell load from a store that hangs
  with all the connections checked out.
- **A poisoned row** (one an endpoint or the core fails on every pass) makes the task fail its check and
  be replaced, and the replacement fails on the same row: a restart loop. The remedy is to find and fix
  the row on the alert, not to restart.

## Why the core's health leaves out the signer link

The core's health does not include its signer link, and no handshake probe is made. Statements are
daily, and an overdue statement already pages (`REQ-PR-1`). Restarting the core does not mend a signer
outage, so failing the core's check for it would only churn the task. A raw probe from the core's user
would also be logged by the signer as a refused connection on every interval. The signer has its
own check.

## The liveness file

Each service takes an optional `health_file` in its configuration (TOML for the core and the endpoints,
JSON for the signer). Left out, it is `/tmp/bezent-<service>.health`, which exists in the images
and is writable by their users. A task definition that sets `readonlyRootFilesystem` must mount a
writable volume and name a path on it as `health_file`. A relative path is resolved against the configuration file's directory, like the other path fields.
A beat that cannot be written is logged at error level once, and again on recovery, not on every pass,
and the check then reports the service unhealthy, which is true of it. The file holds the time of the
beat, written to a temporary file beside it and renamed over it; `health` reads the time from the
content, not from the modification time, and a beat dated more than 5 s ahead of the clock is stale.

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

`startPeriod` is bounded by ECS at 300 s. See "The first start of the kreivo-endpoint" for what it has to cover there.

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# bezent in another repository's end-to-end harness

`ghcr.io/bloque-app/bezent-sidecar` is bezent for a CI service container: the ingress, the core's
cycle and a test signer in one process, on a PostgreSQL the harness provides. It is **test only**.
It signs with the published test keys, serves two test networks (`kreivo-local`, `polygon-amoy`) and
one bridged asset (`bUSDC`, 6 decimals, `Here(101)` on the issuing network, a deposit and a
withdrawal route), and refuses to start unless `BEZENT_SIDECAR_ENVIRONMENT=test` and every network
already in the store is one of those two. There is no chain behind it: the harness makes the
networks do things through the [chain-event driver](#the-chain-event-driver), which writes what an
endpoint would have seen. The real services (`bezent-ingress`, `bezent-core`, `bezent-signer`,
`bezent-kreivo-endpoint`, `bezent-evm-endpoint`) have their own images, built by the same workflow,
for deployments.

## Pinning by digest

Images are built by `.github/workflows/images.yml`. A pull request builds and smoke-tests them and
pushes nothing. A merge to `main` pushes each as `sha-<commit>` and `edge`, and the run's summary
lists `ghcr.io/bloque-app/bezent-sidecar:sha-<commit>@sha256:<digest>`. A harness pins that digest,
never `edge`:

```yaml
services:
  bezent:
    # sha-<commit> (bezent main, #<pr>)
    image: ghcr.io/bloque-app/bezent-sidecar@sha256:<digest>
    ports:
      - 8081:8080
    env:
      BEZENT_SIDECAR_ENVIRONMENT: test
      BEZENT_SIDECAR_DATABASE_URL: postgres://bezent:bezent@postgres:5432/bezent
      BEZENT_SIDECAR_REQUESTERS: swaps=ci-swaps.<a secret of at least 16 characters>
    options: >-
      --health-cmd "bezent-sidecar health"
      --health-interval 2s
      --health-timeout 3s
      --health-retries 30
```

The packages are public, so no credentials are needed. To bump the pin, take the digest from the summary of
bezent's "Images" run on `main` and change it with the commit that needs it.

## Access

The six packages (`bezent-sidecar`, `bezent-ingress`, `bezent-core`, `bezent-signer`, `bezent-kreivo-endpoint` and
`bezent-evm-endpoint`) are public on `ghcr.io/bloque-app`. The package exists after the first push to `main`; the
maintainers set its visibility to public once, in the organisation's package settings. A public image of the sidecar
is safe: it signs only with the published test keys and refuses to start outside the test environment.

## Environment

| Variable | | Default |
|---|---|---|
| `BEZENT_SIDECAR_ENVIRONMENT` | exactly `test`; anything else, or unset, and it exits | required |
| `BEZENT_SIDECAR_DATABASE_URL` | the owner login of an empty database (a superuser, or a role that can create roles and tables); migrations run as it. Or a path in `BEZENT_SIDECAR_DATABASE_URL_FILE` | required |
| `BEZENT_SIDECAR_REQUESTERS` | `name=<credential id>.<secret>;…`: each requester is granted deposit, withdrawal and refund; the secret is at least 16 characters. A request carries `Authorization: Bearer <credential id>.<secret>` | required |
| `BEZENT_SIDECAR_LISTEN` | the ingress address; `health` probes this port on the loopback | `0.0.0.0:8080` |
| `BEZENT_SIDECAR_SIGNER_SOCKET` | the test signer's Unix socket inside the container | `/tmp/bezent-signer.sock` |
| `BEZENT_SIDECAR_SIGNER_TOKEN` | the signer link's token; a random one, kept in memory, if unset | unset |
| `BEZENT_SIDECAR_CYCLE_SECONDS` | seconds between core cycles (and driver rounds) | `2` |
| `BEZENT_SIDECAR_DRIVER_TOKEN` | binds the [chain-event driver](#the-chain-event-driver); at least 16 characters. Unset, there is no driver | unset |
| `BEZENT_SIDECAR_DRIVER_LISTEN` | the driver's address, a port of its own | `0.0.0.0:8090` |

Start-up migrates, checks the networks, seeds the declarations (idempotent: a store seeded by an
earlier sidecar gains the issuing identity, the backend and the withdrawal route) and the
requesters, and creates one database login per service, each a member of its own role and of
nothing else (`bezent_ingress` for the ingress, `bezent_core` for the core, `bezent_signer` for the
signer, `bezent_webhooks` for the webhook dispatcher, and with the driver `bezent_kreivo_endpoint`
and `bezent_evm_endpoint`), with random passwords that live in memory only. Each login is checked
as its service checks it at start-up (#46, `crates/store/README.md`); a login an earlier sidecar
made a member of the retired `bezent_service` is brought back to its own role alone. The ingress listens last, so a healthy container is a ready one. The query surface is
`/v0/…` (the OpenAPI document: `bezent-ingress openapi`); `/health` needs no credential.

## The chain-event driver

PLAN `DEC-19` (`pl-pr-e2e`, 2026-10-15): every pull request of a requester runs the sidecar with no
chain, and a test-only driver inside it injects the chain events a flow needs. The driver **plays the
endpoints, never the requester**: it writes observations, final heads, prepared and signed
transactions, proofs and readings, as the endpoints' own logins and under the store's own rules,
and the core decides every transfer from them exactly as it would on a real network. It never
writes a transfer's state or an intent, and a requester's request still only points at a
coordinate (guardrail 3, `REQ-RQ-7`).

**Test only.** It exists only in the sidecar, which runs only with `BEZENT_SIDECAR_ENVIRONMENT=test`
on a store of its two test networks. It is bound only when `BEZENT_SIDECAR_DRIVER_TOKEN` is set,
and refuses to bind on a store that declares any other network (the store's rule,
`bezent_store::environment`, shared with the core). Every call checks that rule again before it
writes, and answers `404 absent` once a production network is declared; the sidecar itself stops
at its next cycle.

Expose the port next to the ingress's and give the token:

```yaml
    ports:
      - 8081:8080
      - 8091:8090
    env:
      # …as above, plus:
      BEZENT_SIDECAR_DRIVER_TOKEN: ci-driver-<at least 16 characters>
```

Every call is `POST`, JSON in and out, with `Authorization: Bearer <driver token>` (`401` without
it). Amounts are decimal strings of minor units. A refusal is `{"error", "detail"}`:
`400 malformed`, `404 absent` (a production network is declared), `409 not-ready` (what the call
needs is not there yet: poll and call again), `409 conflict` (it would contradict what is recorded),
`409 refused` (a store rule refused it), `500 store`.

The driver's chains have no clock of their own: each call that observes something makes a new block
on its network (one past the highest the store holds; its hash is derived from the height, so a
height always has the same hash). Nothing is final until `finality` says so. The endpoints' own half
of a delivery runs by itself every cycle, as it does in a deployment: each order the core records is
prepared (its dry run passes, unless `dry-run-failures` says otherwise), each signing intent is
signed by the sidecar's test signer over its link and the signed bytes persisted. Nothing is
submitted: `deliveries/include` or `deliveries/not-executed` says what the chain did with it.

| Call | Body | What it writes | Answer |
|---|---|---|---|
| `/driver/v0/arrivals` | `amount`; `sender`, `token` optional | on `polygon-amoy`, a new block with an arrival at the reserve, kind `arrival` (or `other_arrival` for a token not admitted), not final | `coordinate`, as `/v0/deposits` takes it |
| `/driver/v0/redemptions` | `redeemer` (SS58 or `0x` 32 bytes), `amount` (the asset's minor units on Kreivo), `payout_address`; `reference` optional (random) | on `kreivo-local`, a new block with a `Redeemed` event, kind `redemption`, backend `evm:80002`, not final | `coordinate`, `redeemer`, `reference`, and the withdrawal's `bridge_reference` |
| `/driver/v0/finality` | `network`; `coordinate` and `outcome` (`final`, or `withdrawn` with a coordinate) optional | the network's tip as a final head, every block up to it as read, and each open observation (or the one named) decided at it | `final_head`, `decided` |
| `/driver/v0/deliveries/include` | `bridge_reference`; `amount`, `recipient` optional | the signed delivery of the transfer's current attempt, included: a new block with the leg the core matches by its reference (`Minted`, `RefundMinted` or `Payout`), the ordered amount to the ordered recipient unless the body says otherwise (the core then sends it to review), not final. `409 not-ready` until the stand-in has signed it | `coordinate`, `kind`, `amount`, `recipient` |
| `/driver/v0/deliveries/not-executed` | `bridge_reference` | for every signed transaction of the current attempt, a proof of non-execution (`nonce-unconsumed`) at a final head past its validity window; the core then orders the next attempt (or, for a withdrawal past its deadline, the refund mint) | `final_head`, `proofs` |
| `/driver/v0/dry-run-failures` | `network`, `reason` | nothing in the store: the next preparation on that network fails its dry run with `reason`, once. `Kreivo: …` holds `CND-DestinationRefused`, `AboveWindow` `CND-MintWindow`, one naming a fee `CND-FeeShort`; the core orders again after a minute | `next_preparation: "fails"` |
| `/driver/v0/readings` | `network` optional (both); `set: [{subject, holder, amount}]` optional | at the point the reserve check reads each network (the latest final head at or below the read watermark and below every open observation), supply of `Here(101)` on `kreivo-local` and the reserve's balance of `usdc` on `polygon-amoy`, as the driver's final observations add up there, or as `set` gives them. The sidecar does not run the reserve check; these are for a harness, or a core, that does | `readings` |

### A deposit, from end to end

```sh
D='curl -s -H "Authorization: Bearer $DRIVER_TOKEN" -H "Content-Type: application/json"'
# 1. The chain: 1 USDC arrives at the reserve on polygon-amoy, and becomes final.
$D -d '{"amount":"1000000"}' localhost:8091/driver/v0/arrivals        # → {"coordinate": C, …}
$D -d '{"network":"polygon-amoy"}' localhost:8091/driver/v0/finality
# 2. The requester: a deposit at that coordinate (503 not-final before step 1's finality).
#    POST /v0/deposits {"request_reference", "coordinate": C, "destination": {"network":
#    "kreivo-local", "address": <SS58>}, "refund_destination": {"network": "polygon-amoy", …}}
#    → 201, state Accepted, bridge_reference B.
# 3. bezent: the core takes it to Source-Final and orders the mint; the stand-in endpoints prepare
#    and sign it. Poll GET /v0/transfers/by-bridge/B until state is Delivering (a few cycles).
# 4. The chain: the mint is included, then final.
$D -d '{"bridge_reference":"B"}' localhost:8091/driver/v0/deliveries/include   # → Delivered
$D -d '{"network":"kreivo-local"}' localhost:8091/driver/v0/finality           # → Completed
```

Variations: `deliveries/not-executed` instead of `include` at step 4 proves the attempt not
executed, and the transfer goes back to `Delivering` on attempt 2 (include that one); `include` with
another `amount` sends it to `Needs-Review` with `CND-AmountMismatch`; `dry-run-failures` before
step 3 leaves it in `Source-Final` under the condition the reason names.

### A withdrawal, from end to end

A withdrawal is made by the core from a redemption, so it needs reserve in the book: run a deposit
first.

```sh
# 1. The chain: the redeemer redeems 0.4 bUSDC to a payout address, and it becomes final.
$D -d '{"redeemer":"<SS58>","amount":"400000","payout_address":"0x22…22"}' \
   localhost:8091/driver/v0/redemptions          # → {"bridge_reference": W, "reference": R, …}
$D -d '{"network":"kreivo-local"}' localhost:8091/driver/v0/finality
# 2. bezent: the core makes the withdrawal W and orders the payout; the stand-in endpoints prepare
#    an EIP-1559 transaction on chain 80002 and sign it. A requester reads W once it adds to it:
#    POST /v0/withdrawals/extras {"request_reference", "redemption_reference": R,
#    "expires_at": …}; then poll GET /v0/transfers/by-bridge/W until state is Delivering. (Without
#    the extras request, poll deliveries/include: it answers 409 not-ready until the payout is signed.)
# 3. The chain: the payout is included, then final.
$D -d '{"bridge_reference":"W"}' localhost:8091/driver/v0/deliveries/include   # → Delivered
$D -d '{"network":"polygon-amoy"}' localhost:8091/driver/v0/finality           # → Completed
# 4. Optionally, the readings at both networks' final points: supply 600000, reserve 600000.
$D -d '{}' localhost:8091/driver/v0/readings
```

Two details a harness needs (found wiring a requester's integration):

- The leg kinds the driver confirms are `mint` (on Kreivo) and `payout_leg` (on the origin network).
- A withdrawal exists only after the core has processed its redemption; extras for it (expiry, relayed
  instructions) must be retried until the withdrawal is known, rather than sent once right after the redemption.

The driver's tests (`services/sidecar/tests/driver.rs`) run both walk-throughs, one test per call,
and the refusals.

> **Run the sidecar on its own PostgreSQL server (or container), one that hosts no non-test bezent database.**
> Its database logins are server-wide: it creates logins that are members of `bezent_kreivo_endpoint` and
> `bezent_evm_endpoint`, roles that exist across the whole server, and those logins may write observations and
> outcomes (mint evidence). The sidecar's network check covers only its own database, so a fresh database on a
> server that also hosts a real bezent store passes it. Never point it at such a server.

The images workflow smoke-tests the sidecar against PostgreSQL 18, in a throwaway container. Give the database a
name of its own: a second sidecar on the same server re-keys the logins.

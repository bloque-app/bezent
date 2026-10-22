# syntax=docker/dockerfile:1.10
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# bezent's long-running processes from the root workspace, one image each (targets):
#
#   ingress           bezent-ingress: the request and query surfaces        ghcr.io/bloque-app/bezent-ingress
#   core              bezent-core: the state machine and its cycle          ghcr.io/bloque-app/bezent-core
#   signer            bezent-signer: the key boundary; its own user (65533)  ghcr.io/bloque-app/bezent-signer
#   kreivo-endpoint   bezent-kreivo-endpoint: the issuing network's light
#                     client, with the pinned Kreivo chain spec baked in      ghcr.io/bloque-app/bezent-kreivo-endpoint
#   evm-endpoint      bezent-evm-endpoint: observes the reserve vault on an
#                     EVM origin network; holds no key                       ghcr.io/bloque-app/bezent-evm-endpoint
#   migrate           bezent-migrate: brings the store to the release's
#                     migrations as the migration owner; seeds nothing        ghcr.io/bloque-app/bezent-migrate
#   sidecar           bezent-sidecar: ingress, core and a test signer in one
#                     container, on an external PostgreSQL, TEST networks
#                     only, for other repositories' e2e harnesses             ghcr.io/bloque-app/bezent-sidecar
#
# Built in CI only (.github/workflows/images.yml), for linux/amd64, from the repository root:
#
#   docker buildx build -f deploy/docker/services.Dockerfile --target ingress .
#
# Every base image is pinned by digest; the Rust toolchain by version (RUSTUP_TOOLCHAIN overrides
# rust-toolchain.toml, which would otherwise fetch components on every build; keep the two in step);
# every crate by Cargo.lock (`--locked`). The runtime is distroless (glibc, CA certificates, no
# shell), run as its `nonroot` user (65532); the signer as 65533, so that its caller is never its
# own user (`REQ-AR-3`, `REQ-CU-1`).
#
# No secret is baked into any image. Each service reads its configuration from the file named by
# its command (mount it at the path in `CMD`), and the files that file names (database URL, signer
# token, keys) are mounted too (`REQ-KY-1`). The signer reads test keys only, from a mounted file.
# No image carries a database CA bundle (a deployment specific): the deployment mounts it and names it in the
# database URL (`sslrootcert`; crates/store/README.md, "Connecting over TLS").

ARG RUST_IMAGE=docker.io/library/rust:1.99.0-slim-bookworm@sha256:2c3a22f0a5533ea2dd5a16627bc841228151faa2d4de2644ac9987e4a2f1f2fa
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
# The sidecar's runtime: the same distroless base, its `debug` variant, which adds busybox, so that a
# CI job's `--health-cmd` (always run through a shell) works. Test only; never a production image.
ARG SIDECAR_IMAGE=gcr.io/distroless/cc-debian12:debug-nonroot@sha256:923320b891f20d5f4bd43ed3a72eeee2f3323d481d6f4bd8d0b2c96d1c0758bc

# ---------------------------------------------------------------------------------------------
# The binaries, in one build: the workspace compiles once.
# ---------------------------------------------------------------------------------------------
FROM ${RUST_IMAGE} AS build
ENV RUSTUP_TOOLCHAIN=1.99.0 \
    CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0 \
    CARGO_NET_RETRY=10
RUN apt-get update \
 && apt-get install -y --no-install-recommends git pkg-config \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,target=/src/target,sharing=locked \
    cargo build --release --locked \
      -p bezent-ingress --bin bezent-ingress \
      -p bezent-core-service --bin bezent-core \
      -p bezent-signer --bin bezent-signer \
      -p bezent-kreivo-endpoint --bin bezent-kreivo-endpoint \
      -p bezent-evm-endpoint --bin bezent-evm-endpoint \
      -p bezent-migrate --bin bezent-migrate \
      -p bezent-sidecar --bin bezent-sidecar \
 && mkdir -p /out/bin \
 && cp target/release/bezent-ingress target/release/bezent-core target/release/bezent-signer \
       target/release/bezent-kreivo-endpoint target/release/bezent-evm-endpoint \
       target/release/bezent-migrate target/release/bezent-sidecar /out/bin/

# ---------------------------------------------------------------------------------------------
# Runtime images.
# ---------------------------------------------------------------------------------------------
FROM ${RUNTIME_IMAGE} AS ingress
LABEL org.opencontainers.image.title="bezent-ingress" \
      org.opencontainers.image.description="bezent ingress: the request and query surfaces" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-ingress /usr/local/bin/bezent-ingress
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/bezent-ingress"]
CMD ["/etc/bezent/ingress.toml"]

FROM ${RUNTIME_IMAGE} AS core
LABEL org.opencontainers.image.title="bezent-core" \
      org.opencontainers.image.description="bezent core: the state machine, conditions, deadlines and review" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-core /usr/local/bin/bezent-core
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/bezent-core"]
CMD ["/etc/bezent/core.toml"]

FROM ${RUNTIME_IMAGE} AS signer
LABEL org.opencontainers.image.title="bezent-signer" \
      org.opencontainers.image.description="bezent signer: signs only intents the core recorded; test keys from a mounted file" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-signer /usr/local/bin/bezent-signer
# Its own user, apart from every caller's: callers are told apart by the socket's peer credentials.
USER 65533:65533
ENTRYPOINT ["/usr/local/bin/bezent-signer"]
CMD ["/etc/bezent-signer/signer.json"]

FROM ${RUNTIME_IMAGE} AS kreivo-endpoint
LABEL org.opencontainers.image.title="bezent-kreivo-endpoint" \
      org.opencontainers.image.description="bezent Kreivo endpoint: observes the issuing network through a light client" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-kreivo-endpoint /usr/local/bin/bezent-kreivo-endpoint
# The pinned chain spec (public data): name it as `chain_spec` in the mounted configuration.
COPY services/kreivo-endpoint/chain-specs/kreivo.json /usr/share/bezent/chain-specs/kreivo.json
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/bezent-kreivo-endpoint"]
CMD ["/etc/bezent/kreivo-endpoint.toml"]

FROM ${RUNTIME_IMAGE} AS evm-endpoint
LABEL org.opencontainers.image.title="bezent-evm-endpoint" \
      org.opencontainers.image.description="bezent EVM endpoint: observes the reserve vault on an EVM origin network; decides nothing, holds no key" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-evm-endpoint /usr/local/bin/bezent-evm-endpoint
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/bezent-evm-endpoint"]
CMD ["/etc/bezent/evm-endpoint.toml"]

# Run once per release that carries a migration, with every service stopped (crates/store/README.md,
# "Deploying"): `bezent-migrate --url-file /run/secrets/owner-db-url [--check] --login <name>...`. The
# owner's URL is mounted (`--url-file`, preferred; `--url-env` leaves the credential in the process
# environment and is discouraged), never baked in or passed as an argument. A failure at version N
# leaves the versions before N applied; run again to resume (the services stay down until it is
# done). `--lock-timeout <seconds>` (default 300) bounds any wait for a lock. It seeds nothing.
FROM ${RUNTIME_IMAGE} AS migrate
LABEL org.opencontainers.image.title="bezent-migrate" \
      org.opencontainers.image.description="bezent migration runner: applies the store's migrations as the migration owner, refuses a diverging history, checks the grants" \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-migrate /usr/local/bin/bezent-migrate
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/bezent-migrate"]

# Test only (deploy/e2e/README.md): signs with the published test keys, serves the test networks
# `kreivo-local` and `polygon-amoy`, and refuses to start unless BEZENT_SIDECAR_ENVIRONMENT=test.
FROM ${SIDECAR_IMAGE} AS sidecar
LABEL org.opencontainers.image.title="bezent-sidecar" \
      org.opencontainers.image.description="bezent for CI service containers: ingress, core and a test signer on an external PostgreSQL, TEST networks only. Never for production." \
      org.opencontainers.image.source="https://github.com/bloque-app/bezent" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"
COPY --from=build /out/bin/bezent-sidecar /usr/local/bin/bezent-sidecar
# A health check's shell form (`--health-cmd`) always runs through /bin/sh, whatever the image's
# SHELL: give the debug image's busybox that name. Exec-form RUNs, so no shell is needed to do it.
USER 0:0
RUN ["/busybox/mkdir", "-p", "/bin"]
RUN ["/busybox/ln", "-sf", "/busybox/sh", "/bin/sh"]
USER 65532:65532
EXPOSE 8080
# Exec form: works with no options at all. `--health-cmd "bezent-sidecar health"` works as well.
HEALTHCHECK --interval=2s --timeout=3s --start-period=60s --retries=10 \
  CMD ["/usr/local/bin/bezent-sidecar", "health"]
ENTRYPOINT ["/usr/local/bin/bezent-sidecar"]
CMD ["serve"]

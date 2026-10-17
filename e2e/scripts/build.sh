#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Build `spawn_network` (e2e/network, zombienet-sdk) inside a `rust:1-bookworm` container on the
# runner, and run the crate's unit tests (the ruled asset ids and decimals).
#
# Forked from aledgent's `e2e/scripts/build.sh`, without its contract and driver builds (the bezent
# contract is built by the workflow's `contract` job on a hosted runner, T-013-05). Layout, all
# under the job-local directory mounted at /w:
#   /w/src            a copy of the commit under test (the container never writes the checkout)
#   /w/target         the cargo target
#   /w/dist           the output: spawn_network

set -euo pipefail

export CARGO_TERM_COLOR=always CARGO_INCREMENTAL=0
mkdir -p /w/dist /w/target

echo "::group::system packages"
# litep2p's build script (zombienet-sdk -> sc-network) needs protoc.
apt-get update -qq
apt-get install -y -qq --no-install-recommends protobuf-compiler >/dev/null
protoc --version
echo "::endgroup::"

echo "::group::spawn_network"
start=$(date +%s)
CARGO_TARGET_DIR=/w/target cargo test --locked --profile ci --lib \
  --manifest-path /w/src/e2e/network/Cargo.toml
CARGO_TARGET_DIR=/w/target cargo build --locked --profile ci \
  --manifest-path /w/src/e2e/network/Cargo.toml --bin spawn_network
cp /w/target/ci/spawn_network /w/dist/spawn_network
echo "METER phase=build_network s=$(( $(date +%s) - start ))"
echo "::endgroup::"

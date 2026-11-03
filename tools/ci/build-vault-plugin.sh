#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Build blockchain-signer-vault-plugin reproducibly, for the Vault nodes (linux/amd64: t3 instances),
# into the directory given as $1: the binary and a SHA256SUMS file. Used by the `vault-dev` job (the
# bytes it tests) and by .github/workflows/vault-plugin.yml (the bytes it publishes), so they are the
# same bytes. The expected SHA-256 is pinned in deploy/vault-plugin.sha256.
#
# What is fixed: the source commit, the two dependency bumps (past what govulncheck reported for that
# commit: GO-2026-6603, 6610, 6611, 6612, 6617 in golang.org/x/net v0.58.0; GO-2026-6348, 6443 in
# google.golang.org/grpc v1.83.0), the Go toolchain (exactly, no download), the target
# (GOOS/GOARCH/GOAMD64), no cgo, no VCS stamping, no build paths, no build id. Module and build
# caches are fresh, inside a scratch directory at a fixed path.
set -euo pipefail

out=${1:?usage: build-vault-plugin.sh OUTDIR}
PLUGIN_REPO=https://github.com/MaximFischuk/blockchain-signer-vault-plugin.git
PLUGIN_COMMIT=6c5921761f5939aa5adb042e6fd30b48a68bf6da
PLUGIN_X_NET=v0.60.0
PLUGIN_GRPC=v1.83.2
PLUGIN_COMMAND=blockchain-signer-vault-plugin
GO_VERSION=go1.26.1

# A fixed path, not mktemp: some of the plugin's dependencies (gnark-crypto's assembly) leave the
# absolute path of the module cache in the binary even with -trimpath, so a random scratch directory
# gives different bytes on each run.
scratch=${BEZENT_PLUGIN_BUILD_ROOT:-/tmp/bezent-vault-plugin-build}
rm -rf "$scratch"
mkdir -p "$scratch"
trap 'chmod -R u+w "$scratch" 2>/dev/null; rm -rf "$scratch"' EXIT

export GOTOOLCHAIN=local GOFLAGS= GOWORK=off GOENV=off GOPATH="$scratch/gopath" \
  GOCACHE="$scratch/gocache" GOMODCACHE="$scratch/gomod"
export GOOS=linux GOARCH=amd64 GOAMD64=v1 CGO_ENABLED=0

have=$(go env GOVERSION)
if [ "$have" != "$GO_VERSION" ]; then
  echo "need $GO_VERSION, have $have" >&2
  exit 1
fi

src="$scratch/src"
git init -q "$src"
git -C "$src" remote add origin "$PLUGIN_REPO"
git -C "$src" fetch -q --depth 1 origin "$PLUGIN_COMMIT"
git -C "$src" checkout -q --detach FETCH_HEAD
test "$(git -C "$src" rev-parse HEAD)" = "$PLUGIN_COMMIT"

cd "$src"
go get "golang.org/x/net@$PLUGIN_X_NET" "google.golang.org/grpc@$PLUGIN_GRPC"
go mod tidy
go list -m golang.org/x/net google.golang.org/grpc

mkdir -p "$out"
go build -trimpath -buildvcs=false -ldflags="-s -w -buildid=" -o "$out/$PLUGIN_COMMAND" .
(cd "$out" && sha256sum "$PLUGIN_COMMAND" > SHA256SUMS && cat SHA256SUMS)

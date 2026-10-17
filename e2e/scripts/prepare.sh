#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Fetch what the zombienet network needs, from release artifacts only:
#
#   1. Kreivo's runtime wasm, from a virto-network/kreivo release (KREIVO_RELEASE; blank = the
#      latest release). zombienet builds the chain-spec from it, through the runtime's own
#      `GenesisBuilder`, so the released runtime is the one source of both code and genesis.
#   2. polkadot, its two workers and polkadot-parachain, from a paritytech/polkadot-sdk release
#      (PSDK_RELEASE). The relay chain uses the `rococo-local` spec built into polkadot.
#
# Nothing is built and nothing is read from a Kreivo checkout: a working tree can carry unreleased
# changes, which is what this suite exists to catch rather than inherit.
#
# Forked from aledgent's `e2e/scripts/prepare.sh` (itself from substrate-rail's), which fetches the
# same artifacts on the same fleet. Differences: Kreivo 0.17.1 (spec 134) by default, the first
# release with kreivo#512 (contract mint and burn), and polkadot-stable2606-2 by default, which
# Kreivo 0.17 needs. The release's tag and the runtime's hash go to $OUT/artifacts.env.
#
# Collator requirements substrate-rail learned (encoded in e2e/network/src/lib.rs):
#   - `--authoring slot-based` is mandatory (RelayParentOffset = 1).
#   - the parachain is registered in genesis.

set -euo pipefail

OUT="${OUT:-$(pwd)/target/zombienet-artifacts}"
PSDK_RELEASE="${PSDK_RELEASE:-polkadot-stable2606-2}"
KREIVO_RELEASE="${KREIVO_RELEASE-0.17.1}"   # set to "" for the latest release

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) SUFFIX="-aarch64-apple-darwin" ;;
  Linux/x86_64) SUFFIX="" ;;
  *) echo "unsupported platform $(uname -s)/$(uname -m); add its release-asset suffix" >&2; exit 1 ;;
esac

mkdir -p "$OUT/bin"

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKSUMS="$HERE/checksums.sha256"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# verify_pinned <file> <release> <asset>: the file's SHA-256 must equal the one committed in
# checksums.sha256 for that release's asset. No pinned line, or a different hash, is fatal.
verify_pinned() {
  local want got
  want="$(awk -v k="$2/$3" '$2 == k { print $1 }' "$CHECKSUMS")"
  if [ -z "$want" ]; then
    echo "::error::no pinned SHA-256 for $2/$3 in $CHECKSUMS; add it (from the release's own checksum) before using this release" >&2
    exit 1
  fi
  got="$(sha256_of "$1")"
  if [ "$got" != "$want" ]; then
    echo "::error::SHA-256 mismatch for $2/$3: got $got, pinned $want" >&2
    rm -f "$1"
    exit 1
  fi
}

# curl and the releases API rather than `gh`, so that this works wherever curl does. A token,
# when present, lifts the anonymous rate limit; anonymous works too.
auth_header() {
  local tok="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
  [ -n "$tok" ] && printf 'Authorization: Bearer %s' "$tok" || true
}

# release_json <repo> <tag|""> -> the release's JSON (retried; an empty answer is an error)
release_json() {
  local repo="$1" tag="$2" api hdr body attempt
  if [ -n "$tag" ]; then
    api="https://api.github.com/repos/$repo/releases/tags/$tag"
  else
    api="https://api.github.com/repos/$repo/releases/latest"
  fi
  hdr="$(auth_header)"
  for attempt in 1 2 3; do
    body="$(curl -sSL ${hdr:+-H "$hdr"} -H "Accept: application/vnd.github+json" "$api" || true)"
    # A here-string, not `printf | grep -q`: grep -q stops reading at the first match, and under
    # pipefail the writer's broken pipe fails the test on a large release body.
    if grep -q '"tag_name"' <<<"$body"; then
      printf '%s' "$body"
      return 0
    fi
    [ "$attempt" -lt 3 ] && sleep $((attempt * 5))
  done
  echo "could not read release ${tag:-latest} of $repo; API replied: $(printf '%s' "$body" | tr -d '\n' | head -c 300)" >&2
  return 1
}

# asset_url <release-json> <asset-name> -> browser_download_url
asset_url() {
  grep -o "\"browser_download_url\": *\"[^\"]*/$2\"" <<<"$1" \
    | head -1 \
    | sed 's/.*"\(https[^"]*\)"/\1/' || true
}

fetch_asset() {  # fetch_asset <release-json> <asset> <dest> <what>
  local url
  url="$(asset_url "$1" "$2")"
  if [ -z "$url" ]; then
    echo "no asset '$2' in $4" >&2
    exit 1
  fi
  curl -sSL --retry 3 --retry-connrefused -o "$3" "$url"
}

kreivo_json="$(release_json virto-network/kreivo "$KREIVO_RELEASE")"
kreivo_tag="$(grep -o '"tag_name": *"[^"]*"' <<<"$kreivo_json" | head -1 | sed 's/.*"\([^"]*\)"$/\1/')"
echo "==> Kreivo runtime wasm (release $kreivo_tag)"
fetch_asset "$kreivo_json" kreivo_runtime.compact.compressed.wasm \
  "$OUT/kreivo_runtime.compact.compressed.wasm" "Kreivo release $kreivo_tag"
verify_pinned "$OUT/kreivo_runtime.compact.compressed.wasm" "$kreivo_tag" kreivo_runtime.compact.compressed.wasm

echo "==> polkadot-sdk binaries ($PSDK_RELEASE$SUFFIX)"
psdk_json=""
for b in polkadot polkadot-execute-worker polkadot-prepare-worker polkadot-parachain; do
  if [ -x "$OUT/bin/$b" ] && [ "$(cat "$OUT/bin/.release" 2>/dev/null)" = "$PSDK_RELEASE$SUFFIX" ]; then
    verify_pinned "$OUT/bin/$b" "$PSDK_RELEASE" "${b}${SUFFIX}"   # a cache is not trusted either
    echo "    $b (cached, checksum ok)"
    continue
  fi
  [ -n "$psdk_json" ] || psdk_json="$(release_json paritytech/polkadot-sdk "$PSDK_RELEASE")"
  fetch_asset "$psdk_json" "${b}${SUFFIX}" "$OUT/bin/$b" "polkadot-sdk $PSDK_RELEASE"
  verify_pinned "$OUT/bin/$b" "$PSDK_RELEASE" "${b}${SUFFIX}"
  chmod +x "$OUT/bin/$b"
done
echo "$PSDK_RELEASE$SUFFIX" > "$OUT/bin/.release"

wasm_sha="$(sha256_of "$OUT/kreivo_runtime.compact.compressed.wasm")"
cat > "$OUT/artifacts.env" <<EOF
KREIVO_RELEASE_TAG=$kreivo_tag
KREIVO_WASM_SHA256=$wasm_sha
PSDK_RELEASE=$PSDK_RELEASE
EOF

cat <<EOF

Artifacts ready in $OUT (Kreivo $kreivo_tag, $PSDK_RELEASE). Spawn the network with:

  export ZOMBIENET_KREIVO_WASM="$OUT/kreivo_runtime.compact.compressed.wasm"
  export ZOMBIENET_POLKADOT_BIN="$OUT/bin/polkadot"
  export ZOMBIENET_POLKADOT_PARACHAIN_BIN="$OUT/bin/polkadot-parachain"
  export PATH="$OUT/bin:\$PATH"   # polkadot finds its workers on PATH
  cargo run --manifest-path e2e/network/Cargo.toml --bin spawn_network
EOF

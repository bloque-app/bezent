#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Start the three origin-network forks (F-013, T-013-03; plan DEC-19): anvil forks of Polygon, Base
# and BNB Chain at the blocks pinned in forks.env, one process each, in the background.
#
#   e2e/evm/up.sh            start, wait until each answers at its pinned block, write endpoints
#   e2e/evm/up.sh down       stop them
#
# Environment:
#   POLYGON_RPC_URL, BASE_RPC_URL, BSC_RPC_URL   upstream archive RPCs (public defaults below)
#   EVM_OUT                                      run directory (default e2e/evm/.run): logs, pids,
#                                                and endpoints.env (POLYGON_RPC=..., BASE_RPC=...)
#
# The forks keep every contract of the pinned block (USDC, EURC, BTCB, CCTP V2, ...). anvil mines a
# block per transaction. The upstream URL is never printed: it may carry a key.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck disable=SC1091
. "$HERE/forks.env"
OUT="${EVM_OUT:-$HERE/.run}"
mkdir -p "$OUT"

# Public endpoints that served state at the pinned blocks when they were chosen (2026-10-13).
POLYGON_RPC_URL="${POLYGON_RPC_URL:-https://polygon.drpc.org}"
BASE_RPC_URL="${BASE_RPC_URL:-https://mainnet.base.org}"
BSC_RPC_URL="${BSC_RPC_URL:-https://bsc-mainnet.public.blastapi.io}"

NETWORKS=(POLYGON BASE BSC)

down() {
  for n in "${NETWORKS[@]}"; do
    pidfile="$OUT/$(tr 'A-Z' 'a-z' <<<"$n").pid"
    if [ -f "$pidfile" ]; then
      kill "$(cat "$pidfile")" 2>/dev/null || true
      rm -f "$pidfile"
    fi
  done
  echo "[evm] forks stopped"
}

rpc() {  # rpc <port> <method> [params-json]
  curl -s -m 10 -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$2\",\"params\":${3:-[]}}" "http://127.0.0.1:$1" || true
}

if [ "${1:-up}" = down ]; then
  down
  exit 0
fi

command -v anvil >/dev/null || { echo "anvil not found: install Foundry (foundryup)" >&2; exit 1; }
anvil --version | head -1

# The forks must outlive a successful run (the scripts that follow use them), but not a failed or
# interrupted one: on any non-zero exit, INT or TERM, stop what this run started.
started=0
cleanup() {
  local rc=$?
  trap - EXIT INT TERM
  if [ "$started" = 1 ] && [ "$rc" -ne 0 ]; then
    echo "[evm] start failed (exit $rc); stopping the forks this run started" >&2
    down
  fi
  exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

endpoints="$OUT/endpoints.env"
: > "$endpoints"
for n in "${NETWORKS[@]}"; do
  lower="$(tr 'A-Z' 'a-z' <<<"$n")"
  url_var="${n}_RPC_URL"; block_var="${n}_BLOCK"; port_var="${n}_PORT"; chain_var="${n}_CHAIN_ID"
  port="${!port_var}"
  if rpc "$port" eth_chainId | grep -q result; then
    echo "::error::port $port is taken; run '$0 down' or free it" >&2
    exit 1
  fi
  echo "[evm] $lower: forking chain ${!chain_var} at block ${!block_var} on port $port"
  started=1
  nohup anvil \
    --fork-url "${!url_var}" \
    --fork-block-number "${!block_var}" \
    --chain-id "${!chain_var}" \
    --port "$port" \
    --host 127.0.0.1 \
    --retries 10 \
    --fork-retry-backoff 2000 \
    --timeout 60000 \
    --silent \
    > "$OUT/$lower.log" 2>&1 &
  echo $! > "$OUT/$lower.pid"
  echo "${n}_RPC=http://127.0.0.1:$port" >> "$endpoints"
done

# Ready means the fork answers, and its head is the pinned block.
for n in "${NETWORKS[@]}"; do
  lower="$(tr 'A-Z' 'a-z' <<<"$n")"
  port_var="${n}_PORT"; block_var="${n}_BLOCK"; chain_var="${n}_CHAIN_ID"
  port="${!port_var}"
  ok=""
  for _ in $(seq 1 60); do
    if ! kill -0 "$(cat "$OUT/$lower.pid")" 2>/dev/null; then
      echo "::error::the $lower fork exited" >&2
      tail -20 "$OUT/$lower.log" >&2
      exit 1
    fi
    head="$(rpc "$port" eth_blockNumber | sed -n 's/.*"result":"\(0x[0-9a-f]*\)".*/\1/p')"
    if [ -n "$head" ]; then ok=1; break; fi
    sleep 2
  done
  [ -n "$ok" ] || { echo "::error::the $lower fork did not answer" >&2; tail -20 "$OUT/$lower.log" >&2; exit 1; }
  chain="$(rpc "$port" eth_chainId | sed -n 's/.*"result":"\(0x[0-9a-f]*\)".*/\1/p')"
  if [ "$((head))" -ne "${!block_var}" ] || [ "$((chain))" -ne "${!chain_var}" ]; then
    echo "::error::the $lower fork is at block $((head)) of chain $((chain)); want ${!block_var} of ${!chain_var}" >&2
    exit 1
  fi
  echo "[evm] $lower: up at block $((head)), chain $((chain)), http://127.0.0.1:$port"
done
echo "[evm] endpoints in $endpoints"

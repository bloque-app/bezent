#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Starts a PostgreSQL container named `pg` on 127.0.0.1:5432 (user and password `postgres`) and
# waits until it is healthy: what a `services:` block did, for an image the CI pulls itself
# (.github/actions/ci-postgres).
#
#   tools/ci/pg-up.sh <image>
set -euo pipefail

image=${1:?usage: pg-up.sh <image>}
docker rm -f pg > /dev/null 2>&1 || true
docker run -d --name pg -p 5432:5432 -e POSTGRES_PASSWORD=postgres \
  --health-cmd "pg_isready -U postgres" --health-interval 2s --health-retries 30 \
  "$image" > /dev/null
for i in $(seq 1 60); do
  [ "$(docker inspect -f '{{.State.Health.Status}}' pg)" = healthy ] && exit 0
  sleep 1
done
docker logs pg
exit 1

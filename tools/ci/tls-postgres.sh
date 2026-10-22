#!/usr/bin/env bash
# Copyright (c) 2026 Pagos Cube LLC
# SPDX-License-Identifier: Apache-2.0 OR MIT
# Starts a PostgreSQL container that forces TLS, for crates/store/tests/tls.rs, and prints the
# environment that test reads. The CA and the server certificate are made fresh in <dir> and are
# never committed; the CA's private key is thrown away once the server certificate is signed.
#
#   tools/ci/tls-postgres.sh <dir> [port]      (port defaults to 5433)
#
# The server certificate is for `localhost` only, so a client that connects to 127.0.0.1 with
# sslmode=verify-full must be refused (the test checks it). Remove the container with
# `docker rm -f bezent-tls-pg`. The image is $BEZENT_CI_POSTGRES_IMAGE (the CI sets it from
# .github/ci-images.env), postgres:16 if unset.
set -euo pipefail

dir=${1:?usage: tls-postgres.sh <dir> [port]}
port=${2:-5433}
mkdir -p "$dir"
cd "$dir"

openssl req -x509 -newkey rsa:2048 -nodes -days 2 -subj "/CN=bezent test CA" \
  -keyout ca.key -out ca.pem 2> /dev/null
openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2> /dev/null
printf 'subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\n' > san.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 2 \
  -extfile san.cnf -out server.crt 2> /dev/null
rm -f ca.key server.csr san.cnf ca.srl
docker rm -f bezent-tls-pg > /dev/null 2>&1 || true
# The key must belong to the server's user and be private to it, which a bind mount cannot
# promise on every host: copy it inside the container, then start the server as the image does.
docker run -d --name bezent-tls-pg -p "127.0.0.1:$port:5432" \
  -e POSTGRES_PASSWORD=postgres \
  -v "$PWD:/tls-in:ro" \
  --health-cmd "pg_isready -U postgres" --health-interval 2s --health-retries 30 \
  --entrypoint bash \
  "${BEZENT_CI_POSTGRES_IMAGE:-postgres:16}" -c '
    mkdir -p /tls && cp /tls-in/server.crt /tls-in/server.key /tls/ \
      && chown postgres:postgres /tls/server.* && chmod 600 /tls/server.key \
      && exec docker-entrypoint.sh postgres -c ssl=on \
           -c ssl_cert_file=/tls/server.crt -c ssl_key_file=/tls/server.key' > /dev/null

for i in $(seq 1 60); do
  [ "$(docker inspect -f '{{.State.Health.Status}}' bezent-tls-pg)" = healthy ] && break
  if [ "$i" = 60 ]; then docker logs bezent-tls-pg; exit 1; fi
  sleep 1
done

echo "BEZENT_TLS_TEST_HOSTPORT=localhost:$port"
echo "BEZENT_TLS_TEST_CA=$PWD/ca.pem"

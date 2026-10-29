<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# The signer beside its caller (sidecar)

A signer can run in the task of the one service that asks it to sign, as a sidecar container, and
serve it over a Unix socket on a volume the two containers share. The service is the evm-endpoint,
whose Polygon delivery key the sidecar holds (the mechanism does not depend on which service it is;
the kreivo-endpoint uses the central signer). The endpoint reads `[signer]` as before (`socket`,
`uid`, `token_file`), and nothing in it changes. The link is the one `services/signer/src/link.rs` describes: the signer
authenticates the caller by the user id the kernel reports for the socket's peer and by a token
whose SHA-256 its configuration lists, and the caller authenticates the signer by the user id that
serves the socket. The signer and its caller never share a user id (`REQ-CU-1`, `REQ-AR-3`).

The images are distroless, so a third container prepares the volumes: `bezent-signer prepare`
(`services/signer/src/prepare.rs`). The values below are public defaults; use your own ids and
paths if the images do.

## Containers

| Container | Image | User | Essential | Starts when | Mounts |
|---|---|---|---|---|---|
| `config` | the signer's | `0` (root), command `prepare /etc/bezent-signer/prepare.json` | no | first | `signer-config` rw, `caller-token` rw, `signer-socket` rw |
| `signer` | the signer's | `65533:65533` (image default) | yes | `config` is `COMPLETE` | `signer-config` rw, `signer-socket` rw |
| the caller (`evm-endpoint`) | its own | `65532:65532` (image default) | yes | `signer` is `HEALTHY` | `caller-token` ro, `signer-socket` rw |

ECS `dependsOn`: `signer` on `config` with condition `COMPLETE`; the caller on `signer` with
condition `HEALTHY`. The signer's health check is
`["CMD", "/usr/local/bin/bezent-signer", "health", "/run/bezent/signer-config/signer.json"]`: healthy
when its store login is checked, the socket is bound and the store answered within the last 35 s,
so the caller never starts before there is a signer to reach. If the signer stops, the task stops
(essential), and a new task prepares everything again.

## Volumes

Three task-scoped volumes (ephemeral storage, no host path). A new volume is root's, which is why
`config` runs as root; it gives each directory to the one user who reads it.

| Volume | Path | Owner and mode after `prepare` | Holds | Who reads it |
|---|---|---|---|---|
| `signer-config` | `/run/bezent/signer-config` | `65533:65533`, `0700` | `signer.json` (`0400`), the liveness file | the signer |
| `caller-token` | `/run/bezent/caller-token` | `65532:65532`, `0700` | `token` (`0600`): 64 hex digits, no newline | the caller |
| `signer-socket` | `/run/bezent/signer` | `65533:65532`, `2750` (setgid) | `signer.sock` (`0660`, the caller's group) | both |

The signer mounts `signer-config` read-write: its root file system is read-only, and its
configuration names `"health_file": "/run/bezent/signer-config/signer.health"`, a file in the
directory it owns (the configuration file itself stays `0400`). The caller reads `token` only: the
signer's configuration holds the token's SHA-256, never the token.

## What `prepare` does

`prepare` takes a spec, a JSON file in the image next to the template (the signer configuration
with no callers):

```json
{ "template": "/etc/bezent-signer/sidecar.json",
  "config": "/run/bezent/signer-config/signer.json",
  "signer": { "uid": 65533, "gid": 65533 },
  "caller": { "name": "evm-endpoint", "uid": 65532, "gid": 65532 },
  "token_file": "/run/bezent/caller-token/token",
  "socket_dir": "/run/bezent/signer" }
```

- Writes 32 random bytes as 64 hex digits to `token_file`, for the caller alone.
- Renders `template` to `config` with exactly one caller (name, user id, the token's SHA-256). A
  template that names callers, binds no key, or puts its `socket` anywhere but in `socket_dir` is
  refused, and nothing is written.
- Creates `socket_dir` for the signer's user and the caller's group.
- Refuses a caller with the signer's own user id.

Running it again is safe: it replaces the token and the hash together and changes no owner or mode.
It prints paths and never the token.

## The signer's configuration

The template is a signer configuration (`services/signer/src/config.rs`) with `"callers": []`,
`"socket": "/run/bezent/signer/signer.sock"`, the `store` and `vault` tables, and `networks`,
`keys` and `bindings` for the keys of this one caller and no others: a key the template does not
bind is nobody's. Every path in the template is absolute (`keys.path`, `database_url_file` and the
rest): a relative one would be read against the directory of the rendered configuration, not of the
template. The task role is the signer container's identity for Vault and the store; the
caller needs none of them for the signer.

The caller's `[signer]` table:

```toml
[signer]
socket = "/run/bezent/signer/signer.sock"
uid = 65533
token_file = "/run/bezent/caller-token/token"
```

## Delivery keys

The signer asks for its keys the way its configuration says (`keys.backend`). A delivery key lives
in a key-management service that signs on request and never releases the key (`REQ-CU-1`): AWS KMS
(`kms`: secp256k1, which is what a Polygon key is) or, for a Kreivo ed25519 account, Vault Transit
(`transit`, mount and key names in the
configuration; the account is the public key and is declared and compared at start). The signer
logs in to Vault under its task role for each signature and verifies each signature against the
public key it read at start before it returns it (`INV-9`).

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

The signer reaches every key through one interface, a signing engine (`services/signer/src/engine.rs`), and its
configuration says which engine holds which key (`keys.backend`): `file` (test keys only), or a signing service for
the delivery keys with the other keys optionally read from Vault KV (`keys.vault`). The signing services are AWS KMS
(`kms`: secp256k1; kept working, not used by this deployment), Vault Transit (`transit`: the Kreivo ed25519
account, which the central signer holds) and a Vault signing plugin (`vault-plugin`: the Polygon key, which this
sidecar holds). Whatever the engine, the signer computes the digest itself, checks every signature against the
key's public key before it releases it (`INV-9`), and puts no key, token, payload or signature in a log line or an
error (`REQ-KY-1`). A key never leaves the component that signs with it (`REQ-CU-1`): in a signing service it never
leaves the service at all.

### The Polygon key in the Vault signing plugin

The key is created inside Vault by the secrets engine
[`blockchain-signer-vault-plugin`](https://github.com/MaximFischuk/blockchain-signer-vault-plugin), which has no path
that returns or imports a key. The signer asks it for `sign/hash` and nothing else, and does the rest itself:

1. It computes the Keccak-256 of the transaction's signing payload. A request that is not exactly a 32-byte digest is
   refused before anything is sent: the plugin itself signs whatever bytes it is given.
2. It sends the hash alone to `<mount>/keys/<key>/sign/hash`.
3. It turns the DER answer into `r ‖ s` with a low `s`, and finds `v` by recovering, for each candidate, the
   address the configuration declares. If neither recovers it, the signature is refused.
4. It verifies the signature against the declared public key before it returns `r ‖ s ‖ v`.

The template's `keys`:

```json
"keys": { "backend": "vault-plugin", "environment": "production", "mount": "signer",
          "role": "bezent-evm-signer-plugin",
          "keys": [ { "id": "polygon-delivery", "key": "bezent-polygon-delivery",
                      "address": "0x…", "public_key": "0x02…" } ] }
```

`address` and `public_key` are what the plugin reported when the key was created (`vault read
signer/keys/bezent-polygon-delivery`, by an administrator, once). The signer checks at start, offline, that the one
is the other's, and refuses a published test fixture's key in production. It reads nothing from Vault to learn the
key, so its role needs no read. A key in Vault that is not the declared one is found at the first signature, which
is refused; nothing is released or recorded. The engine's health at start is a login under its role: it signs
nothing to find out (a signature with no recorded intent is what `AC-I1.2` treats as a compromise).

**Two Vault roles, both bound to the task role.** The signer logs in to the same Vault (the `vault` table: address,
AWS auth mount, server ID) under two roles, each with one job:

| Role | Where it is named | Policy |
|---|---|---|
| `vault.role`, for example `bezent-evm-signer` | the `vault` table | `read` on the secret that holds the store's login and password, nothing else |
| `keys.role`, for example `bezent-evm-signer-plugin` | `keys.role` | `update` on its own key's `sign/hash`, nothing else |

The configuration refuses one role for both. The signing role's policy, in full:

```hcl
path "signer/keys/bezent-polygon-delivery/sign/hash" {
  capabilities = ["update"]
}
```

`update` alone signs with an existing key (the plugin's `sign/hash` is an update of an existing key; a create would
need a key that does not exist). Everything else is refused to this role: reading or listing keys, creating or
deleting one, `sign/message`, `sign/batch`, the transaction and typed-data routes, and every other key. The AWS
auth role binds the task role's principal, with a token of a minute:

```sh
vault write auth/<aws-mount>/role/bezent-evm-signer-plugin auth_type=iam \
  bound_iam_principal_arn=<task role ARN> token_policies=bezent-evm-signer-plugin \
  token_ttl=60s token_max_ttl=120s
```

The administrator's policy (create, read, list and delete under `signer/keys/`) belongs to a separate, human role,
never to the signer. Keep Vault's audit device on: it records each signature and each refusal.

**The plugin build.** Build it from a pinned, reviewed commit, never an automatic upgrade; bump `golang.org/x/net`
and `google.golang.org/grpc` past the vulnerabilities `govulncheck` reports for that commit; build it for the nodes'
architecture with `CGO_ENABLED=0`; and register that binary by its SHA-256:

```sh
vault plugin register -sha256=<sha256 of the binary> -command=blockchain-signer-vault-plugin \
  secret bezent-evm-signer
vault secrets enable -path=signer bezent-evm-signer
```

Vault refuses to run a binary whose SHA-256 is not the registered one. The `vault-dev` job of CI
(`.github/workflows/ci.yml`) does exactly this, at commit `6c5921761f5939aa5adb042e6fd30b48a68bf6da` with the two
bumps, and runs the signer's engine against the plugin (`services/signer/tests/vault_plugin.rs`): the policy above
signs and refuses everything else, and every signature recovers the declared address with an implementation other
than the signer's. Every Vault node needs `plugin_directory` set and the same binary.

**What the plugin does not do.** It has no key versions, no rotation, no export and no import. A rotation is a new
key and a new address: create the key, register the address where its outflow is bounded on chain (`REQ-KY-3`), move
the reserve, change the configuration, then delete the old key. The only backup is a Vault snapshot. The plugin
stores the key under Vault's barrier but not seal-wrapped, which matters only with an HSM or KMS seal.

### The Kreivo key in Vault Transit

For a Kreivo ed25519 account, `transit` names the mount and the key; the account is the public key, declared and
compared at start, and the key must be one Vault will not export. The signer asks Transit to sign the message itself
with the version it read at start, and verifies each signature against that version's public key.

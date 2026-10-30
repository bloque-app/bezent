<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->
# The signer's Polygon key in the Vault signing plugin

The central signer, which has its own task role, holds the Polygon delivery key through the Vault signing plugin
engine (`services/signer/src/engine.rs`, `services/signer/src/vault_plugin.rs`). There is no signer sidecar: the
evm-endpoint reaches the signer over the mutual-TLS signer link like every other caller (PLAN `DEC-11`,
`d-polygon-central-signer`). Containers in one task share one cloud IAM role, so a sidecar in the evm-endpoint's task
would let the endpoint authenticate as the plugin role and sign arbitrary hashes, bypassing the signer's intent
check (`REQ-AR-3`). Polygon delivery therefore needs the signer link (`PUB-1b`).

The signer's own role is the one bound to the signer's task role below; the caller (the evm-endpoint) has no Vault
role for the plugin and no path to it. The values below are public defaults; use your own names if your deployment
does.

## Delivery keys

The signer reaches every key through one interface, a signing engine (`services/signer/src/engine.rs`), and its
configuration says which engine holds which key (`keys.backend`): `file` (test keys only), or a signing service for
the delivery keys with the other keys optionally read from Vault KV (`keys.vault`). The signing services are AWS KMS
(`kms`: secp256k1; kept working, not used by this deployment), Vault Transit (`transit`: the Kreivo ed25519
account) and a Vault signing plugin (`vault-plugin`: the Polygon key). Whatever the engine, the signer computes the digest itself, checks every signature against the
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
"keys": { "backend": "vault-plugin", "environment": "production", "mount": "signer-bezent",
          "role": "bezent-evm-signer-plugin",
          "keys": [ { "id": "polygon-delivery", "key": "bezent-polygon-delivery",
                      "address": "0x…", "public_key": "0x02…" } ] }
```

`address` and `public_key` are what the plugin reported when the key was created (`vault read
signer-bezent/keys/bezent-polygon-delivery`, by an administrator, once). The signer checks at start, offline, that the one
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
path "signer-bezent/keys/bezent-polygon-delivery/sign/hash" {
  capabilities = ["update"]
}
```

`update` alone signs with an existing key (the plugin's `sign/hash` is an update of an existing key; a create would
need a key that does not exist). Everything else is refused to this role: reading or listing keys, creating or
deleting one, `sign/message`, `sign/batch`, the transaction and typed-data routes, and every other key. The AWS
auth role binds the task role's principal, with a token of a minute that carries this policy alone (not even
Vault's `default` policy):

```sh
vault write auth/<aws-mount>/role/bezent-evm-signer-plugin auth_type=iam \
  bound_iam_principal_arn=<task role ARN> token_policies=bezent-evm-signer-plugin \
  token_ttl=60s token_max_ttl=120s token_no_default_policy=true
```

The administrator's policy (create, read, list and delete under `signer-bezent/keys/`) belongs to a separate, human role,
never to the signer. Keep Vault's audit device on: it records each signature and each refusal.

**The plugin build.** Build it from a pinned, reviewed commit, never an automatic upgrade, for the nodes'
architecture (linux/amd64), and register that binary by its SHA-256. CI builds it for you, byte for byte the same
every time: `tools/ci/build-vault-plugin.sh` fetches commit `6c5921761f5939aa5adb042e6fd30b48a68bf6da`, bumps
`golang.org/x/net` and `google.golang.org/grpc` past the vulnerabilities `govulncheck` reports for it, and builds with
Go 1.26.1, `CGO_ENABLED=0 -trimpath -buildvcs=false -ldflags="-s -w -buildid="`. The workflow `Vault signing plugin`
(`.github/workflows/vault-plugin.yml`) builds it twice on two runners, requires the two to be identical and equal to
the SHA-256 pinned in `deploy/vault-plugin.sha256`, and uploads the binary with a `SHA256SUMS` file as the artifact
`vault-signing-plugin-linux-amd64`; a manual run also attaches both to the release `vault-signing-plugin-<short sha>`.
Putting that binary on the Vault image is the infrastructure repository's job (the bytes are pinned there by the same
SHA-256). Changing the commit or a bump changes the SHA-256: update the pin in the same change.

```sh
vault plugin register -sha256=<sha256 of the binary> -command=blockchain-signer-vault-plugin \
  -version=v0.0.1 secret bezent-evm-signer
vault secrets enable -path=signer-bezent -plugin-version=v0.0.1 bezent-evm-signer
```

Vault refuses to run a binary whose SHA-256 is not the registered one (`v0.0.1` is the version the binary reports;
Vault refuses any other). The `vault-dev` job of CI (`.github/workflows/ci.yml`) registers and mounts that same build
(mount `signer-bezent`), and runs the signer's engine against the plugin (`services/signer/tests/vault_plugin.rs`): the
policy above signs and refuses everything else, and every signature recovers the declared address with an
implementation other than the signer's. Every Vault node needs `plugin_directory` set and the same binary.

**What the plugin does not do.** It has no key versions, no rotation, no export and no import. A rotation is a new
key and a new address: create the key, register the address where its outflow is bounded on chain (`REQ-KY-3`), move
the reserve, change the configuration, then delete the old key. The only backup is a Vault snapshot. The plugin
stores the key under Vault's barrier but not seal-wrapped, which matters only with an HSM or KMS seal.

### The Kreivo key in Vault Transit

For a Kreivo ed25519 account, `transit` names the mount and the key; the account is the public key, declared and
compared at start, and the key must be one Vault will not export. The signer asks Transit to sign the message itself
with the version it read at start, and verifies each signature against that version's public key.

<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Key custody

What the signer (`services/signer`) requires of the keys it uses, whatever the deployment. `REQ-CU-1` (amendment 0015),
`REQ-AR-3` and `REQ-KY-1` to `REQ-KY-6` in `SPEC.md` are the normative statement: key material is never readable
outside the component that signs with it, that component is separate from the one that holds bezent's state and is
auditable on its own, and no key is in a process environment, a configuration, a log, a URL or an error. Where each
key rests is the plan's choice, not a requirement; the operator's own deployment definitions (accounts, roles, key
identifiers) are not part of this repository.

- **One interface, several engines.** The signer reaches every key through a signing engine
  (`services/signer/src/engine.rs`), and its configuration names which engine holds which key (`keys.backend`):
  test keys from a file (`file`); or a signing service that signs on request and never releases the key, AWS KMS
  (`kms`, secp256k1), Vault Transit (`transit`, ed25519) or a Vault signing plugin (`vault-plugin`, secp256k1 for
  EVM networks), with the keys that are not delivery keys optionally read from Vault KV. The plan keeps the Kreivo
  delivery key in Vault Transit and the Polygon delivery key in the Vault signing plugin (`deploy/signer-polygon-plugin.md`), both signed by the central signer.
  Credentials come from the platform's default credential chain (a task or instance role), never from the
  configuration or from an environment variable (`REQ-KY-1`).
- **The signer computes what is signed and checks what comes back.** It computes every digest itself, sends a signing
  service nothing but that digest or the message, and verifies each signature against the key's public key before it
  releases it (`INV-9`). An ECDSA signature leaves as `r ‖ s ‖ v` with a low `s`, `v` found by recovering the key.
- **One key per delivery lane** (`REQ-KY-4`): an EVM payout key per origin network, and a Kreivo mint key. Each is a
  signing key with the signing use only.
- **Tagged with an environment.** Every key belongs to `test` or `production`: a KMS key by its `bezent:environment`
  tag, a Vault KV secret by its `environment` field, a Transit or plugin key by the configuration that names it. The
  signer refuses a key whose environment differs from the one in its configuration, and refuses a custody whose
  environment differs from the networks it signs for or from the networks the store declares (`REQ-KY-6`,
  `AC-I4.2`). Test and production use separate accounts, or at least separate roles, so a test role cannot name a
  production key.
- **Addresses are declared and compared.** The configuration declares each key's EVM address or Kreivo account; the
  signer compares it with the key's public key at start and refuses to run on a mismatch (`REQ-KY-3`).
- **Least privilege.** The signer's role for a signing service may sign with the listed keys and do nothing else: no
  decrypt, no export, no key administration. For the Vault signing plugin that is `update` on each key's `sign/hash`,
  under a role of its own; the role that reads the store's login from Vault KV cannot sign.
- **Other keys** (pause, statements) may be read from Vault KV into the signer process only (`REQ-AR-3`).
- **Published test keys.** `services/signer/fixtures/` holds test keys derived from a public string. They are refused
  in a production environment (`REQ-KY-6`).
- **Every key is bounded on chain** (`REQ-KY-3`): the contract's per-asset bounds and the vault's roles limit what a
  compromised key can do.
- **A signer with one caller** (a local-socket link, for a test or a single-caller deployment; the Polygon key is not held this way, `deploy/signer-polygon-plugin.md`) is the same signer with a configuration naming one caller
  and the keys that caller may use.

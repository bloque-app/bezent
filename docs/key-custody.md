<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Key custody

What the signer (`services/signer`) requires of the keys it uses, whatever the deployment. Amendment 0006 and
`REQ-CU-1`, `REQ-KY-1` to `REQ-KY-6` in `SPEC.md` are the normative statement; the operator's own deployment
definitions (accounts, roles, key identifiers) are not part of this repository.

- **Delivery keys** (those that mint, pay out from a reserve, or refund) live in a key-management service that signs
  on request and never releases them. The signer's configuration names each key by its identifier
  (`keys.backend = "kms"`); credentials come from the platform's default credential chain (a task or instance role),
  never from the configuration or from an environment variable (`REQ-KY-1`).
- **One key per delivery lane** (`REQ-KY-4`): an EVM payout key per origin network, and a Kreivo mint key. Keys are
  elliptic-curve signing keys (secp256k1) with the signing usage only.
- **Tagged with an environment.** Every key carries the tag `bezent:environment` = `test` or `production`. The signer
  refuses a key with no tag, or whose tag differs from the environment in its configuration, and refuses a custody
  whose environment differs from the networks it signs for or from the networks the store declares (`REQ-KY-6`,
  `AC-I4.2`). Test and production use separate accounts, or at least separate roles, so a test role cannot name a
  production key.
- **Addresses are declared and compared.** The configuration declares each key's EVM address or Kreivo account; the
  signer compares it with the key's public key at start and refuses to run on a mismatch.
- **Least privilege.** The signer's role may read the public key, read the key's tags and sign, and only on the listed
  keys. It has no decrypt, no export and no key administration.
- **Other keys** (pause, sweep fees, statements) may be held in a secrets vault and are loaded only by the signer
  process (`REQ-AR-3`).
- **Published test keys.** `services/signer/fixtures/` holds test keys derived from a public string. They are refused
  in a production environment (`REQ-KY-6`).
- **Every key is bounded on chain** (`REQ-KY-3`): the contract's per-asset bounds and the vault's roles limit what a
  compromised key can do.

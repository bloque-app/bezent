<!--
Copyright (c) 2026 Pagos Cube LLC
SPDX-License-Identifier: CC-BY-4.0
-->

# Security policy

bezent holds reserves on origin networks and mints a bridged asset against them. We take reports about it seriously
and would rather hear about a problem privately, early, than read about it.

bezent is in progress and is not in production: no deployment holds user value yet. Reports are most useful now,
before it does.

## Reporting a vulnerability

**Do not open a public issue, discussion or pull request for a security problem.**

Report it privately through GitHub's **private vulnerability reporting**: on this repository's *Security* tab, choose
*Report a vulnerability*. That opens an advisory that only you and the maintainers can see.

Please include what is affected (component, commit or release) and how to reproduce it; what an attacker can do with
it (mint without an arrival, move or redirect a reserve, skip a bound, forge or hide a status, read what they should
not, deny service); and any proof of concept. Please do not test against a live deployment; see below.

## What to expect

- We acknowledge a report within **three working days**.
- We give a first assessment, with severity and next steps, within **ten working days**.
- We agree on a disclosure date with you. Our default is to publish an advisory when a fix is released, and within 90
  days of the report at the latest. We credit you in the advisory unless you ask us not to.

## Scope

In scope: everything in this repository. That is the core and store (`crates/`), the services (`services/`), the ink!
contract (`contracts/bezent`), the EVM contracts (`contracts/evm`) and the test bed (`e2e/`).

Problems in upstream projects (Kreivo, smoldot, the Polkadot SDK, Safe, Zodiac Roles, Foundry) should go to those
projects. If one affects bezent, tell us as well and we will coordinate.

The keys under `services/signer/fixtures/` are published test keys and are not a finding. The signer refuses them in a
production environment (`REQ-KY-6`); a way around that refusal is a finding.

## Safe harbour

We will not pursue legal action against anyone who, in good faith, researches and reports a vulnerability under this
policy, provided they do not access or modify other people's data or funds, do not degrade a live service, and give us
reasonable time to fix the problem before disclosing it. Test against your own deployment. The end-to-end suite
(`e2e/`) starts a local network for exactly this.

## Known limitations

`SPEC.md` and `PLAN.md` state what bezent does *not* yet enforce, including the trust it asks of its operator in V0
(the reserve is attested on some backends, and governance can upgrade the contract after a published delay). Reports
that restate a declared limitation are welcome as discussions, not as vulnerabilities.

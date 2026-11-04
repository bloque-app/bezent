-- SPDX-License-Identifier: GPL-3.0-or-later
-- At start-up the signer holds its keys' environment to the networks the store declares
-- (REQ-KY-6, AC-I4.2, bezent_store::environment::production_networks), reading `networks` with its
-- own login. No migration gave its role that read, so against a store with the grants the release
-- means, the signer stopped at start-up with "permission denied for table networks". The tests did
-- not show it: they build the signer's parts, not its start-up, with the role's login.
--
-- The one column that check reads, and nothing else (REQ-AR-3).

GRANT SELECT (network) ON networks TO bezent_signer;

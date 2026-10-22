// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-store
//!
//! bezent's PostgreSQL schema (`F-001`, T-001-03 to T-001-05; PLAN `DEC-4`), as `sqlx` migrations
//! under `migrations/`. The store-layer invariants of SPEC §9 are enforced by the schema's
//! triggers, constraints and grants, not by application code (`REQ-AR-5`):
//!
//! | Table | Rule | Spec |
//! |---|---|---|
//! | `observations`, `observation_outcomes` | no UPDATE, DELETE or TRUNCATE; one row per coordinate; an outcome is decided at or past the observed block | `INV-10`, `REQ-OB-2` |
//! | `book_entries`, `book_postings` | no UPDATE, DELETE or TRUNCATE; every entry balances per asset at commit; a committed entry never gains a posting | `INV-17`, `REQ-BK-7` |
//! | `transitions`, `event_log` | no UPDATE, DELETE or TRUNCATE; the log's positions are gapless; every transition and every new transfer appends its event in its own transaction | `REQ-TR-2`, `REQ-ST-4` |
//! | `transfers` | inserted only in an initial state; every later state only by a transition along an edge of §5.4, with the coordinate it rests on; milestones written by that transition only; source binding, on-chain reference, refund destination and expiry write-once; a source coordinate and an on-chain reference fund at most one transfer; a terminal transfer never changes; no DELETE | `REQ-TR-2`, `INV-5`, `INV-2`, `INV-10` |
//! | `intake` | no UPDATE, DELETE or TRUNCATE; one request reference per requester | `REQ-ST-7`, `REQ-ID-1`, PLAN `DEC-15` |
//! | `intents`, `signed_transactions` | no UPDATE, DELETE or TRUNCATE | `INV-9`, `REQ-EO-1` |
//! | `intake_answers`, `relayed_instructions` | no UPDATE, DELETE or TRUNCATE; one answer per request; instructions relayed once per withdrawal | `REQ-RQ-1`, `REQ-ST-7`, `REQ-RQ-9` |
//! | `networks`, `assets`, `admitted_tokens`, `reserves`, `routes`, `pauses`, `requesters`, `requester_grants`, `requester_credentials` | declarations: the services only read them; a credential's secret is stored as its SHA-256 | `REQ-AS-2`, `REQ-LM-1`, `REQ-GV-1`, `REQ-PN-1`, `REQ-PN-2` |
//! | `signatures` | no UPDATE, DELETE or TRUNCATE; one signature per intent, and per (network, purpose, redeemer scope, on-chain reference, attempt); the reference, amount, lane and attempt are copied from the intent; attempt n+1 only once attempt n is proven not executed | `INV-9`, `INV-2`, `REQ-EO-1`, `REQ-EO-6` |
//! | `transitions` (F-003) | the edges of the §5.4 transition table only (amendment 0009); a delivery attempt is numbered, signed and recorded, the next only after the previous is proven not executed and resting on that proof; a review ends one ordinary edge from the state before it, on evidence, run as a recorded manual action, with a second approver when value moves | `REQ-TR-6`, `REQ-EO-6`, `REQ-RV-2`, `REQ-GV-5` |
//! | `attempt_milestones`, `final_heads`, `manual_actions`, `manual_approvals` | no UPDATE, DELETE or TRUNCATE; milestones per attempt, written by their transition only, the transfer's from the attempt that completed it; one final head per height, recorded by an endpoint's role only; the actor and the approver are the session's login and the times the store's, the approver's login is not the actor's; only the operator's role records either, naming neither who nor when | `REQ-TR-4`, `NFR-3`, `REQ-OB-2`, `REQ-GV-5` |
//! | `transfers.destination_reference`, refunds (F-003 review) | a withdrawal's payout writes its own reference, unique and set once; a payout intent carries it. `Refunding` only when every attempt with an intent is proven not executed; a refunding or refunded transfer takes no further intent or signature, the two serialised on the transfer's row | `REQ-OT-2`, `INV-2`, `INV-8`, `REQ-RF-1` |
//! | `event_log` kinds `key-compromise`, `pause-requested`, `delivery-anomaly` | appended only by a login holding [`ENDPOINT_ROLE`] or [`CORE_ROLE`] (and, an alert and its pause request, [`SIGNER_ROLE`], 0049), so the ingress cannot pre-empt a signal | `AC-I1.2`, `REQ-IC-4` |
//! | `observations`, `observation_outcomes`, `signed_transactions` | inserted only by a login holding [`ENDPOINT_ROLE`] (migration 0021, issues #33, #35); the issuing network's observations and outcomes refused to any other login by trigger as well | `REQ-OB-6`, `REQ-EO-1`, `INV-8` |
//! | `book_entries`, `book_postings` (F-004, migration 0030) | one entry per movement, by (kind, reference), corrections aside; each rests on one network's block; a posting may name its item (the reserve check reads an unitemised owed or in-transit balance as a divergence); only a login holding [`CORE_ROLE`] posts | `INV-17`, `REQ-BK-7`, `REQ-BK-2` |
//! | `reserve_evaluations`, `reserve_statements` (F-004) | no UPDATE, DELETE or TRUNCATE; a divergence names its differences, an incomplete evaluation its reason; statement n names statement n-1's SHA-256, one per place in the chain | `REQ-BK-1`, `NFR-5`, `REQ-PR-2` |
//! | `reserve_statement_abandoned` (issue #42, migration 0041) | no UPDATE, DELETE or TRUNCATE; only the asset's next open place, with no published statement and a statement the statement key signed for it, is abandoned, by the login that records it; a statement then names the last published one before it and skips only abandoned places, and none is published at an abandoned place; the signer reads the rule through `statement_place_refusal` only | `REQ-PR-2` |
//! | `pauses` (F-004, migrations 0030, 0031, 0040) | a login that does not own the table inserts only the core's pause of one asset's routes, on an evaluation of that asset that diverged, one that could not be made, an observation of it the book could not post, or a supply reading of it flagged as a mismatch between the on-chain component's count and the issuance; lifting a pause stays the operator's | `REQ-BK-1`, `REQ-IC-2`, `INV-4`, `REQ-GV-1` |
//! | `book_quarantine`, `surplus_funding_addresses` (F-004, migration 0031) | no UPDATE, DELETE or TRUNCATE; a quarantined observation is set aside once and only the core records it; funding addresses are the owner's declarations, no service login writes them, each applying from a block past every one recorded on its network (0032) | `REQ-IC-2`, `INV-1`, `REQ-BK-5` |
//! | `transfers` source binding (F-004, migration 0032) | a deposit never binds, nor reaches Source-Final on, an arrival from a declared operator funding address (`funding_arrival()`): a top-up is surplus, never minted | `INV-1`, `INV-2`, `REQ-IN-3` |
//! | `observations` kind and writer (migration 0034, issue #30) | every observation carries its `kind`, from a closed set, written by the endpoint that saw it (`other` when it says none); a network records only the kinds its role produces; a login that does not own the table records observations and outcomes only of the networks its registered endpoint role covers (`endpoint_scopes`; [`KREIVO_ENDPOINT_ROLE`]: the issuing network) | `REQ-OB-6`, `INV-2`, `INV-8` |
//! | `transfers` source binding, kind (migration 0034, issue #30) | on the binding, by insert or update, and when the source becomes final, a deposit binds only an observation of kind `arrival` or `conversion_output` at a reserve, and the refund of an unassociated arrival only one of kind `arrival`, `other_arrival`, `convertible_arrival` or `conversion_output` (`source_bindable()`); a source on a declared origin network never becomes final with no observation | `REQ-RQ-3`, `REQ-RQ-6`, `REQ-IN-3`, `REQ-IN-6`, `REQ-CV-1`, `REQ-CV-2`, `INV-2` |
//! | every endpoint record (observations, outcomes, ranges, final heads, prepared and signed transactions, proofs) | a login holding [`EVM_ENDPOINT_ROLE`] writes those of EVM networks only, a prepared transaction counting as its order's network; its scope in `endpoint_scopes` is the origin networks of address format `evm` (migration 0035, F-009) | `REQ-OB-6`, `INV-8` |
//! | `prepared_transactions`, signing `intents` (two-step delivery, PLAN `DEC-3`) | a prepared transaction answers a delivery order, on that order's network; the EVM endpoint prepares EVM orders only, any other endpoint never an EVM order; a signing intent names a prepared transaction of its own order: same network, transfer, attempt, purpose, reference and scope (migration 0036, PR #39, issue #38) | `INV-9`, `REQ-EO-1` |
//! | `webhook_secrets`, `webhook_transfer_urls`, `webhook_condition_urls`, `webhook_cursor`, `webhook_condition_holds`, `webhook_deliveries` (issue #2, migration 0044) | secrets and URLs sealed by the ingress, append-only; a transfer's callback URL is written once per requester, and only by a requester whose request made or reached the transfer; the fan-out cursor only moves forward; a delivery is unique per (requester, de-duplication key), its key is the one derived from what it points at, it carries no body or URL, what it points at never changes, its attempts only grow, and once delivered or dead it never changes; only a login holding [`WEBHOOKS_ROLE`] reads the sealed columns or writes a delivery, a condition hold or the cursor | `REQ-ST-5`, `REQ-IC-5`, `REQ-ID-4` |
//! | `transitions`, `transfers` source-final milestone, `event_log` (migration 0046, review of PR #56) | a login holding [`INGRESS_ROLE`] records only a request's decision, `Received` to `Accepted` or `Refused`, of a deposit or an unassociated arrival's refund, with no coordinate; every other transition is the core's (or the owner's); whoever writes it, a transfer reaches `Source-Final` (by transition or by insert) only when the observation at its source is final: outcome `final`, decided at a recorded final head at or above the observed block, and no final head at the observed height with another hash; each log kind is written by the role whose events it records (`transfer`, `transition`: the store's triggers; `refusal`: the ingress; signals: an endpoint or the core; any other: the core) | `INV-6`, `REQ-RQ-7`, `REQ-IN-2`, `REQ-IN-7`, `REQ-ST-4`, `AC-I1.2` |
//! | `transfers` creation (migration 0047, review M1b of PR #56) | a login holding [`INGRESS_ROLE`] creates only what a request makes: a deposit or the refund of an unassociated arrival, in `Received`, its source (if any) on an origin network, its bridge reference in the request namespaces (`bz-d-`, `bz-u-`, `bz-r-`), with no redeemer, on-chain reference, destination reference or `refund_of`; a login holding [`CORE_ROLE`] creates only withdrawals (in `Received`, from a redemption on the issuing network) and refunds of withdrawals (in `Received`, of a withdrawal being delivered or in review, to its redeemer, with its on-chain reference, every attempt proven not executed at finality); the owner any row the other rules admit; nobody else any. The writer is the session's login | `INV-6`, `INV-8`, `REQ-RQ-7`, `REQ-RF-1`, `REQ-RF-3`, `REQ-RF-4` |
//! | `transfers` amount, refund of a withdrawal, id (migration 0048, review M1c, S7, S8 of PR #56) | a deposit's, or an unassociated arrival's refund's, amount is its bound observation's (the arrival, or the conversion's output) and its asset the observed token's family (or the token itself, on a refund), set from it when not given and refused otherwise, whoever writes; the core refunds a withdrawal only after at least one ordered attempt, every attempt proven not executed at finality, a review entered from `Delivering` or `Delivered`, with the withdrawal's source, asset and amount; a login's row takes the id the store's sequence gave its session | `REQ-IN-4`, `REQ-CV-2`, `REQ-RQ-7`, `INV-8`, `REQ-RF-1` |
//! | every chain record (observations, outcomes, ranges, final heads, readings, prepared and signed transactions, proofs; migration 0049, must-fix 1 of the audit of PR #56) | a login holding an endpoint role, by its session's login and the role it acts as, writes only rows of the networks every registered endpoint role it holds covers (`endpoint_scopes`): [`KREIVO_ENDPOINT_ROLE`] the issuing network, [`EVM_ENDPOINT_ROLE`] the EVM origin networks | `REQ-OB-6`, `INV-8` |
//! | `intents` orders, `transitions` id, `event_log` alerts, `transfers` destinations (migration 0049, should-fixes of the audit of PR #56) | for every login but the owner: a delivery order rests on a final, executed observation of its transfer's source and orders exactly what it carries (a mint, the arrival's amount scaled to the asset, of its issuing asset, on the issuing network; a payout, the redemption's amount scaled to the destination token, to the withdrawal's destination; a refund mint, the redemption's amount; an unassociated arrival's refund, the arrival's amount on its network); a transition's id is the one the store's sequence drew for the session; a `key-compromise` alert is written only by the role that detects it ([`CORE_ROLE`], an endpoint role on a network it covers, [`SIGNER_ROLE`]), stamped with its detector and detection key, one per pair; a bound deposit's destination is on the issuing network and its refund destination on its arrival's, an unassociated arrival's refund pays out on the arrival's network, by insert and by acceptance | `INV-6`, `INV-7`, `REQ-AS-9`, `REQ-TR-6`, `AC-I1.2`, `REQ-IC-4`, `REQ-RQ-3`, `REQ-IN-6`, `REQ-RQ-7` |
//! | `intents` orders (migration 0050, issue #36) | for every login but the owner: a mint order's recipient is the 32-byte account the deposit's destination decodes to; a payout order's asset is its transfer's and its token, if it names one, the token admitted for that asset on the destination network (the refund of an unassociated arrival: the arrival's own token); and a network admits one token per asset | `INV-6`, `INV-7`, `REQ-IN-4`, `REQ-AS-2`, `REQ-AS-3`, `REQ-OT-4` |
//! | every amount column | the `minor_units` domain: an integer in `0..=u128::MAX`, never rounded; each column states whose minor units it counts | `INV-11`, `REQ-AS-4` |
//!
//! Each service logs in as a login of its own, a member of its service's role and of nothing else
//! (migration 0045; issues #35, #46): [`CORE_ROLE`], [`INGRESS_ROLE`], [`KREIVO_ENDPOINT_ROLE`],
//! [`EVM_ENDPOINT_ROLE`], [`WEBHOOKS_ROLE`], [`SIGNER_ROLE`]; each operator logs in as their own
//! login, a member of [`OPERATOR_ROLE`] (`REQ-GV-5`). No role owns anything, so none can ALTER, DROP
//! or disable a trigger. What each may do, and all it may do, is [`grants::MATRIX`] (also in
//! `README.md`); every service checks its login against it at start-up ([`login::check_login`]),
//! and makes its pool with [`login::pool_options`], which closes a connection that comes back
//! acting as another role (#27). The shared [`SERVICE_ROLE`] holds nothing any more.
//!
//! Every refusal raises SQLSTATE [`REFUSAL_SQLSTATE`] with the spec id in its message.

pub mod environment;
pub mod grants;
pub mod login;

/// The SQLSTATE every store-rule refusal raises.
pub const REFUSAL_SQLSTATE: &str = "BZ001";

/// The role every service but the signer used to log in as a member of. Retired: it holds nothing
/// since migration 0045 (issue #35), and no login is a member of it.
pub const SERVICE_ROLE: &str = "bezent_service";

/// The role the core's login holds, and nothing else: it records intents, which no other service
/// may (`INV-9`; `F-005` review), transitions, the book and the reserve check.
pub const CORE_ROLE: &str = "bezent_core";

/// The endpoints' mark (`F-007` review S1): [`KREIVO_ENDPOINT_ROLE`] and [`EVM_ENDPOINT_ROLE`] hold
/// it without SET, and the store's guards look for it ("an endpoint wrote this"). It holds no
/// privilege since migration 0045: each endpoint role holds its own (`REQ-NX-1`, `INV-8`).
pub const ENDPOINT_ROLE: &str = "bezent_endpoint";

/// The role the Kreivo endpoint's login holds (migration 0034, issue #30): it holds the
/// [`ENDPOINT_ROLE`] mark, and records observations and outcomes of the issuing network only
/// (`endpoint_scopes`). A login holding the [`ENDPOINT_ROLE`] and no registered endpoint role
/// records no observation (`REQ-OB-6`, `INV-2`, `INV-8`).
pub const KREIVO_ENDPOINT_ROLE: &str = "bezent_kreivo_endpoint";

/// The role the EVM endpoint's login holds (`F-009`). It holds the [`ENDPOINT_ROLE`] mark and the
/// same grants as the [`KREIVO_ENDPOINT_ROLE`], narrowed by trigger to the records of EVM networks (migration 0035): it cannot write the
/// issuing network's observations or proofs (`REQ-OB-6`, `INV-8`).
pub const EVM_ENDPOINT_ROLE: &str = "bezent_evm_endpoint";

/// The role the signer uses (`F-005`): it reads intents and records signatures, nothing else.
pub const SIGNER_ROLE: &str = "bezent_signer";

/// The role the operator surface uses (`F-003`, for `F-014`): it records manual actions and their
/// approvals (`REQ-GV-5`), and nothing else a service may. Each operator logs in as their own
/// login, a member of this role; the store records that login as the actor or the approver.
pub const OPERATOR_ROLE: &str = "bezent_operator";

/// The role the ingress API's login holds (migration 0044; PR #55 re-check S-c). It is the only role that registers a webhook URL or makes a signing secret on
/// behalf of a requester: the core's and the endpoints' logins are refused.
pub const INGRESS_ROLE: &str = "bezent_ingress";

/// The role the webhook dispatcher's login holds, and nothing else (issue #2, migration 0044; PR
/// #55 review M2): it alone opens the sealed signing secrets and callback URLs, moves the fan-out
/// cursor, and writes deliveries. A member of no other role.
pub const WEBHOOKS_ROLE: &str = "bezent_webhooks";

/// bezent's migrations, embedded at build time. Up migrations only: production never reverts a
/// migration, so the down migrations are not in this crate's `migrations/` and no service binary
/// carries them. They live with the tests, under `tests/migrations/`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

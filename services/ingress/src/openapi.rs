// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The OpenAPI document of the request and query surfaces, generated from the code (T-002-01). It
//! is committed as `services/ingress/openapi.json`; a test fails when the two differ.

use crate::api::{
    CapacityOut, CoordinateBody, CoordinateOut, DepositRequest, DestinationBody, Health, LogEntry,
    LogPage, Milestone, PositionBody, PositionKind, RefundRequest, ReserveEquation, ReserveOut,
    Reserves, RouteOut, RouteTable, TransferList, TransferStatus, WindowOut,
    WithdrawalExtrasRequest,
};
use crate::error::ErrorBody;
use crate::webhooks::api::{
    CallbackRegistration, ConditionCallback, DeliveryList, DeliveryOut, TransferCallback,
    WebhookSecret,
};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

struct Bearer;

impl Modify for Bearer {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        api.components
            .get_or_insert_with(Default::default)
            .add_security_scheme(
                "bearer",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .description(Some(
                            "A requester's credential, `<credential id>.<secret>` (REQ-PN-1)",
                        ))
                        .build(),
                ),
            );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "bezent ingress",
        description = "The request surface (CTR-REQ, SPEC §7.A), the query surface (CTR-QRY, §7.B) and webhooks (CTR-EVT, §7.C). \
            Amounts are decimal strings of exact integer minor units; on-chain bytes are 0x-prefixed hexadecimal.\n\n\
            Webhooks (REQ-ST-5, REQ-IC-5): a POST with a JSON body that only points at the sequenced log, never an outcome. \
            A transfer notification is {\"type\": \"transfer\", \"seq\", \"dedupe_key\", \"bridge_reference\", \"request_reference\"}; \
            a condition notification is {\"type\": \"condition\", \"seq\", \"dedupe_key\", \"condition\", \"route\": {\"asset\", \"network\", \"kind\"}}, \
            sent to every requester granted on the route (a grant of the route's kind), whether or not it has a transfer there, once when a shared gate condition starts on the route and once when it clears. \
            Headers: bezent-timestamp (Unix seconds), bezent-signature (v1=<hex HMAC-SHA256 of '<timestamp>.<raw body>' under the requester's secret>), bezent-dedupe-key. \
            To verify a delivery, in this order: (1) reject it unless bezent-timestamp is within a bound of your clock (for example 5 minutes either way), so a captured delivery cannot be replayed later; \
            (2) compute the signature over the timestamp and the raw body and compare it with bezent-signature in constant time; \
            (3) de-duplicate by dedupe_key, keeping the keys you have seen for at least the whole retry window (about 70 minutes with the default bound, and longer if the operator configures more attempts or a longer back-off), not just the timestamp bound, since a retry carries the same key and body with a new timestamp and signature and can arrive that late. \
            Delivery is at-least-once: answer 2xx to acknowledge; anything else is retried with back-off and dead-lettered after a bound. \
            Rotating the secret (POST /v0/webhooks/secret) takes effect at once, retries included: accept either the old or the new secret until your receiver holds the new one; a delivery you reject meanwhile is retried, and counts toward the bound. \
            Then re-read the status or the log from your last position; no notification is needed to stay current.",
        license(name = "GPL-3.0-or-later")
    ),
    paths(
        crate::query::health,
        crate::intake::deposit,
        crate::intake::withdrawal_extras,
        crate::intake::refund,
        crate::query::by_request,
        crate::query::by_bridge,
        crate::query::by_coordinate,
        crate::query::routes,
        crate::query::reserves,
        crate::query::reserve_equation,
        crate::query::log,
        crate::webhooks::api::rotate_secret,
        crate::webhooks::api::register_conditions,
        crate::webhooks::api::register_by_request,
        crate::webhooks::api::register_by_bridge,
        crate::webhooks::api::deliveries,
    ),
    components(schemas(
        CapacityOut, CoordinateBody, CoordinateOut, DepositRequest, DestinationBody, ErrorBody,
        Health, LogEntry, LogPage, Milestone, PositionBody, PositionKind, RefundRequest,
        ReserveEquation, ReserveOut, Reserves, RouteOut, RouteTable, TransferList, TransferStatus,
        WindowOut, WithdrawalExtrasRequest, CallbackRegistration, ConditionCallback, DeliveryList,
        DeliveryOut, TransferCallback, WebhookSecret,
    )),
    modifiers(&Bearer),
)]
struct Document;

/// The OpenAPI document, as pretty JSON with a final newline.
pub fn document() -> String {
    let mut json = Document::openapi()
        .to_pretty_json()
        .expect("the OpenAPI document serialises");
    json.push('\n');
    json
}

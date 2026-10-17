// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The webhook dispatcher (`REQ-ST-5`, `REQ-IC-5`): fans the sequenced log out into the outbox,
//! then posts what is due.
//!
//! **Its own login.** The dispatcher's pool logs in as a member of
//! [`bezent_store::WEBHOOKS_ROLE`] and of nothing else (PR #55 review M2). Only that role opens the
//! sealed secrets and URLs, moves the cursor, keeps the holds and writes deliveries; the store
//! refuses a delivery from any other role.
//!
//! **Fan-out.** In one transaction: lock the cursor (another dispatcher holding it means this one
//! skips the fan-out), read the log from the cursor up to the committed head, append a delivery for
//! every notification an entry makes, and move the cursor. A delivery is unique per (requester,
//! de-duplication key), so an entry fanned out again adds nothing. A delivery records only what it
//! points at (the log position, the transfer or the condition), never a body or a URL.
//!
//! An entry of a transfer is notified to the callback URL each requester set for it (the one
//! current when the delivery is attempted: a requester may replace its own), except an in-place
//! hold or release of a shared gate condition (`REQ-IC-5`): those update the holds per
//! (condition, route) instead, and the condition URL of every requester granted on the route is
//! notified when the first transfer is held there and when the last is released, whether or not
//! that requester has a transfer on the route. `CND-ReserveShort` is notified from the core's
//! `condition-raised` and `condition-cleared` entries, which name no transfer. Other entries with
//! no transfer (refusals answered without a transfer, operator and reserve signals) are not
//! notified: the log keeps them.
//!
//! **Delivery.** Claims due deliveries with a lease (`FOR UPDATE SKIP LOCKED`, so dispatchers do
//! not take the same row), builds each body from the log entry it points at (PR #55 review M2: a
//! stored body is never signed), checks the row against that entry and the recipient's standing
//! (its request reached the transfer, or it is granted on the route), posts it signed with its
//! requester's latest secret to the URL current then, and records the answer: a 2xx ends it
//! delivered; anything else, or no answer, schedules a retry with back-off, until
//! [`super::Webhooks::max_attempts`] ends it dead. A row that does not match its entry ends dead at
//! once, unsent. A crash between the post and the record sends it again after the lease:
//! at-least-once, de-duplicable by its key.

use super::{
    DEDUPE_HEADER, GATE_CONDITIONS, SIGNATURE_HEADER, TIMESTAMP_HEADER, Webhooks, class,
    secret_aad, signature, url_aad,
};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// The body of a notification about a transfer: only where to look and how to recognise it.
#[derive(Serialize)]
struct TransferNotice<'a> {
    r#type: &'static str,
    seq: i64,
    dedupe_key: &'a str,
    bridge_reference: &'a str,
    request_reference: &'a str,
}

/// A route as a condition notification names it.
#[derive(Serialize)]
struct RouteRef<'a> {
    asset: &'a str,
    network: &'a str,
    kind: &'a str,
}

/// The body of a notification about a condition (`REQ-IC-5`).
#[derive(Serialize)]
struct ConditionNotice<'a> {
    r#type: &'static str,
    seq: i64,
    dedupe_key: &'a str,
    condition: &'a str,
    route: RouteRef<'a>,
}

/// What one step did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Log positions fanned out.
    pub fanned_out: i64,
    /// Deliveries appended to the outbox.
    pub enqueued: u64,
    /// Deliveries acknowledged.
    pub delivered: u64,
    /// Deliveries that failed and wait for a retry.
    pub retrying: u64,
    /// Deliveries dead-lettered.
    pub dead: u64,
}

/// The route a transfer travels: `(asset or token, network, kind)`.
#[derive(sqlx::FromRow)]
struct Route {
    asset: String,
    network: String,
    kind: String,
}

impl Route {
    fn key(&self) -> String {
        format!("{}/{}/{}", self.asset, self.network, self.kind)
    }
}

/// A delivery claimed for an attempt: what it points at, never what is sent.
#[derive(sqlx::FromRow)]
struct Due {
    id: i64,
    requester: String,
    seq: i64,
    dedupe_key: String,
    target: String,
    transfer_id: Option<i64>,
    condition: Option<String>,
    change: Option<String>,
    attempts: i32,
}

/// What a delivery is sent as, built at send time from the log entry it points at.
struct Prepared {
    url_nonce: Vec<u8>,
    url_sealed: Vec<u8>,
    body: String,
}

/// The result of one attempt.
enum Attempt {
    Acknowledged(u16),
    /// Not acknowledged: the HTTP status if one answered, the coarse class the delivery records
    /// ([`class`]; PR #55 review M1), and a detail for the operator's log only (never an address,
    /// never the URL).
    Failed(Option<u16>, &'static str, String),
    /// Not sent, and never will be: the row does not match the log entry it points at, or its
    /// requester no longer stands to receive it. It ends dead at once.
    Refused(&'static str),
}

/// The de-duplication key of a transfer notification.
fn transfer_key(seq: i64) -> String {
    format!("log-{seq}")
}

/// The de-duplication key of a condition notification.
fn condition_key(seq: i64, change: &str, condition: &str) -> String {
    format!("log-{seq}-{change}-{condition}")
}

/// The route of `transfer`.
async fn route_of<'e>(conn: impl PgExecutor<'e>, transfer: i64) -> sqlx::Result<Route> {
    sqlx::query_as(
        "SELECT COALESCE(asset, token) AS asset, \
                COALESCE(CASE WHEN kind = 'deposit' THEN source_network END, \
                         destination_network) AS network, \
                kind::text AS kind \
         FROM transfers WHERE id = $1",
    )
    .bind(transfer)
    .fetch_one(conn)
    .await
}

/// The webhook dispatcher.
#[derive(Clone)]
pub struct Dispatcher {
    pool: PgPool,
    settings: Arc<Webhooks>,
}

impl Dispatcher {
    /// A dispatcher on `pool`, whose login must be a member of [`bezent_store::WEBHOOKS_ROLE`]
    /// (and of nothing else).
    pub fn new(pool: PgPool, settings: Webhooks) -> Result<Self, String> {
        // A client is built per attempt, pinned to the addresses vetted for it (PR #55 review
        // M1); this one only checks, at start, that one can be built.
        settings
            .client(None)
            .map_err(|e| format!("cannot build the webhook client: {e}"))?;
        Ok(Self {
            pool,
            settings: Arc::new(settings),
        })
    }

    /// Runs a step every tick until `stop` turns true.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(self.settings.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.changed() => return,
            }
            if *stop.borrow() {
                return;
            }
            match self.step().await {
                Ok(s) if s != Step::default() => tracing::debug!(?s, "webhook step"),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "webhook step failed; the next starts from the store")
                }
            }
        }
    }

    /// Fans the log out until it is caught up (a bounded number of pages), then attempts one batch
    /// of due deliveries.
    pub async fn step(&self) -> sqlx::Result<Step> {
        let mut step = Step::default();
        for _ in 0..16 {
            let (positions, enqueued) = self.fan_out().await?;
            step.fanned_out += positions;
            step.enqueued += enqueued;
            if positions < self.settings.batch {
                break;
            }
        }
        self.deliver(&mut step).await?;
        Ok(step)
    }

    /// One page of fan-out; returns the positions covered and the deliveries appended.
    async fn fan_out(&self) -> sqlx::Result<(i64, u64)> {
        let mut tx = self.pool.begin().await?;
        let Some(after): Option<i64> =
            sqlx::query_scalar("SELECT after FROM webhook_cursor FOR UPDATE SKIP LOCKED")
                .fetch_optional(&mut *tx)
                .await?
        else {
            // Another dispatcher holds the cursor.
            return Ok((0, 0));
        };
        let head: i64 = sqlx::query_scalar("SELECT seq FROM event_log_head")
            .fetch_one(&mut *tx)
            .await?;
        let until = after.saturating_add(self.settings.batch).min(head);
        if until <= after {
            return Ok((0, 0));
        }
        let entries: Vec<(i64, String, Option<i64>, Value)> = sqlx::query_as(
            "SELECT seq, kind, transfer_id, payload FROM event_log \
             WHERE seq > $1 AND seq <= $2 \
               AND (transfer_id IS NOT NULL OR kind IN ('condition-raised', 'condition-cleared')) \
             ORDER BY seq",
        )
        .bind(after)
        .bind(until)
        .fetch_all(&mut *tx)
        .await?;
        let mut enqueued = 0;
        for (seq, kind, transfer, payload) in entries {
            enqueued += self
                .fan_out_entry(&mut tx, seq, &kind, transfer, &payload)
                .await?;
        }
        sqlx::query("UPDATE webhook_cursor SET after = $1")
            .bind(until)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((until - after, enqueued))
    }

    async fn fan_out_entry(
        &self,
        conn: &mut PgConnection,
        seq: i64,
        kind: &str,
        transfer: Option<i64>,
        payload: &Value,
    ) -> sqlx::Result<u64> {
        let Some(transfer) = transfer else {
            return self.fan_out_core_condition(conn, seq, kind, payload).await;
        };
        let route = route_of(&mut *conn, transfer).await?;
        let mut enqueued = 0;
        let in_place_gate = if kind == "transition" {
            self.track_holds(conn, seq, transfer, payload, &route, &mut enqueued)
                .await?
        } else {
            false
        };
        if in_place_gate {
            return Ok(enqueued);
        }
        // The requesters whose request made or reached the transfer (REQ-ID-4) and that set a
        // callback URL for it before this entry: only entries after the first registration are
        // notified.
        let parties: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT i.requester FROM intake_answers a \
             JOIN intake i ON i.id = a.intake_id \
             WHERE a.transfer_id = $1 \
               AND (SELECT min(m.after_seq) FROM webhook_transfer_urls m \
                    WHERE m.requester = i.requester AND m.transfer_id = $1) < $2 \
             ORDER BY 1",
        )
        .bind(transfer)
        .bind(seq)
        .fetch_all(&mut *conn)
        .await?;
        for requester in parties {
            enqueued += enqueue(conn, &requester, seq, Some(transfer), None).await?;
        }
        Ok(enqueued)
    }

    /// The core's `condition-raised` / `condition-cleared` of `CND-ReserveShort` name no
    /// transfer, only an asset and a network (withdrawals): they start and clear the condition on
    /// that route (`REQ-OT-5`, `REQ-IC-5`).
    async fn fan_out_core_condition(
        &self,
        conn: &mut PgConnection,
        seq: i64,
        kind: &str,
        payload: &Value,
    ) -> sqlx::Result<u64> {
        let change = match kind {
            "condition-raised" => "held",
            "condition-cleared" => "released",
            _ => return Ok(0),
        };
        let field = |name| payload.get(name).and_then(Value::as_str);
        let (Some(condition), Some(_), Some(_)) =
            (field("condition"), field("asset"), field("network"))
        else {
            return Ok(0);
        };
        if !GATE_CONDITIONS.contains(&condition) {
            return Ok(0);
        }
        self.notify_route(conn, seq, "withdrawal", condition, change)
            .await
    }

    /// Keeps the holds of shared gate conditions in step with a transition, and notifies the
    /// route's requesters when a (condition, route) is first held or last released
    /// (`REQ-IC-5`). `CND-ReserveShort` is notified from the core's own entries instead. Returns
    /// whether the transition only held or released such a condition in place, which is then not
    /// notified per transfer.
    async fn track_holds(
        &self,
        conn: &mut PgConnection,
        seq: i64,
        transfer: i64,
        payload: &Value,
        route: &Route,
        enqueued: &mut u64,
    ) -> sqlx::Result<bool> {
        let condition = payload.get("condition").and_then(Value::as_str);
        let gate = condition.filter(|c| GATE_CONDITIONS.contains(c));
        let in_place = payload.get("from_state").is_some()
            && payload.get("from_state") == payload.get("to_state");
        let previous: Option<(String, String)> = sqlx::query_as(
            "SELECT condition::text, route FROM webhook_condition_holds WHERE transfer_id = $1",
        )
        .bind(transfer)
        .fetch_optional(&mut *conn)
        .await?;

        if previous.as_ref().map(|(c, _)| c.as_str()) != gate {
            if let Some((held, held_route)) = &previous {
                sqlx::query("DELETE FROM webhook_condition_holds WHERE transfer_id = $1")
                    .bind(transfer)
                    .execute(&mut *conn)
                    .await?;
                if held != "CND-ReserveShort" && !still_held(conn, held, held_route).await? {
                    let kind = held_route.rsplit('/').next().unwrap_or_default();
                    *enqueued += self.notify_route(conn, seq, kind, held, "released").await?;
                }
            }
            if let Some(gate) = gate {
                let key = route.key();
                let others = still_held(conn, gate, &key).await?;
                sqlx::query(
                    "INSERT INTO webhook_condition_holds (transfer_id, condition, route, since_seq) \
                     VALUES ($1, $2::text::transfer_condition, $3, $4)",
                )
                .bind(transfer)
                .bind(gate)
                .bind(&key)
                .bind(seq)
                .execute(&mut *conn)
                .await?;
                if gate != "CND-ReserveShort" && !others {
                    *enqueued += self
                        .notify_route(conn, seq, &route.kind, gate, "held")
                        .await?;
                }
            }
        }
        // Only a gate held or released in place: neither another state nor another condition.
        Ok(in_place && (previous.is_some() || gate.is_some()) && condition == gate)
    }

    /// Appends a condition delivery for every requester granted on a route of `kind` (a grant of
    /// the route's kind) that set a condition URL, whether or not it has a transfer there
    /// (`REQ-IC-5`).
    async fn notify_route(
        &self,
        conn: &mut PgConnection,
        seq: i64,
        kind: &str,
        condition: &str,
        change: &str,
    ) -> sqlx::Result<u64> {
        let granted: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT g.requester FROM requester_grants g \
             WHERE g.grant_kind = $1 \
               AND EXISTS (SELECT 1 FROM webhook_condition_urls w WHERE w.requester = g.requester) \
             ORDER BY 1",
        )
        .bind(kind)
        .fetch_all(&mut *conn)
        .await?;
        let mut enqueued = 0;
        for requester in granted {
            enqueued += enqueue(conn, &requester, seq, None, Some((condition, change))).await?;
        }
        Ok(enqueued)
    }

    /// Builds what `d` is sent as from the log entry it points at, or `None` when the row does
    /// not match that entry or its requester no longer stands to receive it (PR #55 review M2).
    async fn prepare(&self, d: &Due) -> sqlx::Result<Option<Prepared>> {
        let entry: Option<(String, Option<i64>, Value)> =
            sqlx::query_as("SELECT kind, transfer_id, payload FROM event_log WHERE seq = $1")
                .bind(d.seq)
                .fetch_optional(&self.pool)
                .await?;
        let Some((kind, entry_transfer, payload)) = entry else {
            return Ok(None);
        };
        match (d.target.as_str(), d.transfer_id) {
            ("transfer", Some(transfer)) => {
                if entry_transfer != Some(transfer) || d.dedupe_key != transfer_key(d.seq) {
                    return Ok(None);
                }
                // Its own request made or reached the transfer (REQ-ID-4).
                let references: Option<(String, String)> = sqlx::query_as(
                    "SELECT t.bridge_reference, i.request_reference FROM intake_answers a \
                     JOIN intake i ON i.id = a.intake_id JOIN transfers t ON t.id = a.transfer_id \
                     WHERE a.transfer_id = $1 AND i.requester = $2 ORDER BY i.id LIMIT 1",
                )
                .bind(transfer)
                .bind(&d.requester)
                .fetch_optional(&self.pool)
                .await?;
                let Some((bridge_reference, request_reference)) = references else {
                    return Ok(None);
                };
                // The URL current now: the requester may have replaced the one it was fanned out
                // to (its own callback token changes on each retry).
                let url: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
                    "SELECT url_nonce, url_sealed FROM webhook_transfer_urls \
                     WHERE requester = $1 AND transfer_id = $2 ORDER BY id DESC LIMIT 1",
                )
                .bind(&d.requester)
                .bind(transfer)
                .fetch_optional(&self.pool)
                .await?;
                let Some((url_nonce, url_sealed)) = url else {
                    return Ok(None);
                };
                let body = serde_json::to_string(&TransferNotice {
                    r#type: "transfer",
                    seq: d.seq,
                    dedupe_key: &d.dedupe_key,
                    bridge_reference: &bridge_reference,
                    request_reference: &request_reference,
                })
                .expect("a notice serialises");
                Ok(Some(Prepared {
                    url_nonce,
                    url_sealed,
                    body,
                }))
            }
            ("condition", None) => {
                let (Some(condition), Some(change)) = (d.condition.as_deref(), d.change.as_deref())
                else {
                    return Ok(None);
                };
                if !GATE_CONDITIONS.contains(&condition)
                    || d.dedupe_key != condition_key(d.seq, change, condition)
                {
                    return Ok(None);
                }
                let said = payload.get("condition").and_then(Value::as_str);
                let route = match entry_transfer {
                    // The core's own entry: it names the condition, the asset and the network.
                    None => {
                        let expected = match change {
                            "held" => "condition-raised",
                            "released" => "condition-cleared",
                            _ => return Ok(None),
                        };
                        let field = |name| payload.get(name).and_then(Value::as_str);
                        let (Some(asset), Some(network)) = (field("asset"), field("network"))
                        else {
                            return Ok(None);
                        };
                        if kind != expected || said != Some(condition) {
                            return Ok(None);
                        }
                        Route {
                            asset: asset.into(),
                            network: network.into(),
                            kind: "withdrawal".into(),
                        }
                    }
                    // A transition of a transfer on the route: it took the condition on (held),
                    // or left it (released). CND-ReserveShort comes from the core's entries.
                    Some(transfer) => {
                        let fits = kind == "transition"
                            && condition != "CND-ReserveShort"
                            && match change {
                                "held" => said == Some(condition),
                                "released" => said != Some(condition),
                                _ => false,
                            };
                        if !fits {
                            return Ok(None);
                        }
                        route_of(&self.pool, transfer).await?
                    }
                };
                // Granted on the route (REQ-IC-5).
                let granted: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM requester_grants \
                       WHERE requester = $1 AND grant_kind = $2)",
                )
                .bind(&d.requester)
                .bind(&route.kind)
                .fetch_one(&self.pool)
                .await?;
                let url: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
                    "SELECT url_nonce, url_sealed FROM webhook_condition_urls \
                     WHERE requester = $1 ORDER BY id DESC LIMIT 1",
                )
                .bind(&d.requester)
                .fetch_optional(&self.pool)
                .await?;
                let (true, Some((url_nonce, url_sealed))) = (granted, url) else {
                    return Ok(None);
                };
                let body = serde_json::to_string(&ConditionNotice {
                    r#type: "condition",
                    seq: d.seq,
                    dedupe_key: &d.dedupe_key,
                    condition,
                    route: RouteRef {
                        asset: &route.asset,
                        network: &route.network,
                        kind: &route.kind,
                    },
                })
                .expect("a notice serialises");
                Ok(Some(Prepared {
                    url_nonce,
                    url_sealed,
                    body,
                }))
            }
            _ => Ok(None),
        }
    }

    /// Claims and attempts one batch of due deliveries.
    async fn deliver(&self, step: &mut Step) -> sqlx::Result<()> {
        let lease_ms = i64::try_from(self.settings.lease.as_millis()).unwrap_or(i64::MAX);
        let due: Vec<Due> = sqlx::query_as(
            "UPDATE webhook_deliveries \
             SET leased_until = now() + $2::bigint * interval '1 millisecond' \
             WHERE id IN ( \
               SELECT id FROM webhook_deliveries \
               WHERE state = 'pending' AND next_attempt_at <= now() \
                 AND (leased_until IS NULL OR leased_until <= now()) \
               ORDER BY next_attempt_at, id LIMIT $1 FOR UPDATE SKIP LOCKED) \
             RETURNING id, requester, seq, dedupe_key, target, transfer_id, condition, change, \
                       attempts",
        )
        .bind(self.settings.batch)
        .bind(lease_ms)
        .fetch_all(&self.pool)
        .await?;
        if due.is_empty() {
            return Ok(());
        }

        let mut prepared = Vec::with_capacity(due.len());
        for d in due {
            let p = self.prepare(&d).await?;
            prepared.push((d, p));
        }

        // The latest secret of each requester, opened once per batch.
        let mut secrets: HashMap<String, Option<Vec<u8>>> = HashMap::new();
        for (d, _) in &prepared {
            if secrets.contains_key(&d.requester) {
                continue;
            }
            let row: Option<(Vec<u8>, Vec<u8>)> = sqlx::query_as(
                "SELECT nonce, sealed FROM webhook_secrets WHERE requester = $1 \
                 ORDER BY id DESC LIMIT 1",
            )
            .bind(&d.requester)
            .fetch_optional(&self.pool)
            .await?;
            let opened = row.and_then(|(nonce, sealed)| {
                self.settings
                    .key
                    .open(&secret_aad(&d.requester), &nonce, &sealed)
            });
            secrets.insert(d.requester.clone(), opened);
        }

        let mut attempts = JoinSet::new();
        for (d, p) in prepared {
            let secret = secrets.get(&d.requester).cloned().flatten();
            let settings = self.settings.clone();
            attempts.spawn(async move {
                let Some(p) = p else {
                    return (d, Attempt::Refused(class::NOTICE_REFUSED));
                };
                let url = settings
                    .key
                    .open(&url_aad(&d.requester), &p.url_nonce, &p.url_sealed)
                    .and_then(|u| String::from_utf8(u).ok())
                    .and_then(|u| url::Url::parse(&u).ok());
                let result = match (secret, url) {
                    (None, _) => Attempt::Failed(
                        None,
                        class::NO_SECRET,
                        "no signing secret (POST /v0/webhooks/secret); nothing is sent unsigned"
                            .into(),
                    ),
                    (_, None) => Attempt::Failed(
                        None,
                        class::URL_UNOPENABLE,
                        "the callback URL does not open under this ingress's webhook key".into(),
                    ),
                    (Some(secret), Some(url)) => {
                        post(&settings, &url, &secret, &d.dedupe_key, &p.body).await
                    }
                };
                (d, result)
            });
        }
        while let Some(joined) = attempts.join_next().await {
            let Ok((d, result)) = joined else { continue };
            self.record(&d, result, step).await?;
        }
        Ok(())
    }

    async fn record(&self, d: &Due, result: Attempt, step: &mut Step) -> sqlx::Result<()> {
        match result {
            Attempt::Acknowledged(status) => {
                sqlx::query(
                    "UPDATE webhook_deliveries SET state = 'delivered', attempts = attempts + 1, \
                       last_status = $2, last_error = NULL, leased_until = NULL, \
                       finished_at = now() \
                     WHERE id = $1 AND state = 'pending'",
                )
                .bind(d.id)
                .bind(i32::from(status))
                .execute(&self.pool)
                .await?;
                step.delivered += 1;
                tracing::info!(
                    delivery = d.id,
                    requester = %d.requester,
                    dedupe_key = %d.dedupe_key,
                    "webhook delivered"
                );
            }
            Attempt::Refused(class) => {
                sqlx::query(
                    "UPDATE webhook_deliveries SET state = 'dead', last_status = NULL, \
                       last_error = $2, leased_until = NULL, finished_at = now() \
                     WHERE id = $1 AND state = 'pending'",
                )
                .bind(d.id)
                .bind(class)
                .execute(&self.pool)
                .await?;
                step.dead += 1;
                tracing::error!(
                    delivery = d.id,
                    requester = %d.requester,
                    dedupe_key = %d.dedupe_key,
                    error = class,
                    "webhook refused: the delivery does not match the log entry it points at; \
                     nothing was sent"
                );
            }
            Attempt::Failed(status, class, detail) => {
                let attempts = d.attempts.saturating_add(1);
                let dead = attempts >= self.settings.max_attempts;
                let delay_ms =
                    i64::try_from(self.settings.backoff(attempts).as_millis()).unwrap_or(i64::MAX);
                sqlx::query(
                    "UPDATE webhook_deliveries SET attempts = attempts + 1, last_status = $2, \
                       last_error = $3, leased_until = NULL, \
                       state = CASE WHEN $4 THEN 'dead' ELSE 'pending' END, \
                       finished_at = CASE WHEN $4 THEN now() END, \
                       next_attempt_at = now() + $5::bigint * interval '1 millisecond' \
                     WHERE id = $1 AND state = 'pending'",
                )
                .bind(d.id)
                .bind(status.map(i32::from))
                .bind(class)
                .bind(dead)
                .bind(delay_ms)
                .execute(&self.pool)
                .await?;
                if dead {
                    step.dead += 1;
                    tracing::error!(
                        delivery = d.id,
                        requester = %d.requester,
                        dedupe_key = %d.dedupe_key,
                        attempts,
                        error = class,
                        detail = %detail,
                        "webhook dead-lettered; the requester reads the log (REQ-ST-4)"
                    );
                } else {
                    step.retrying += 1;
                    tracing::warn!(
                        delivery = d.id,
                        requester = %d.requester,
                        dedupe_key = %d.dedupe_key,
                        attempts,
                        retry_in_ms = delay_ms,
                        error = class,
                        detail = %detail,
                        "webhook not acknowledged; retrying"
                    );
                }
            }
        }
        Ok(())
    }
}

/// Whether any transfer is still held by `condition` on `route`.
async fn still_held(conn: &mut PgConnection, condition: &str, route: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM webhook_condition_holds \
           WHERE condition = $1::text::transfer_condition AND route = $2)",
    )
    .bind(condition)
    .bind(route)
    .fetch_one(conn)
    .await
}

/// Appends a delivery unless the requester already has one with its key; returns 1 if appended.
/// A transfer delivery names its transfer; a condition delivery its condition and change.
async fn enqueue(
    conn: &mut PgConnection,
    requester: &str,
    seq: i64,
    transfer: Option<i64>,
    condition: Option<(&str, &str)>,
) -> sqlx::Result<u64> {
    let (target, key) = match condition {
        Some((condition, change)) => ("condition", condition_key(seq, change, condition)),
        None => ("transfer", transfer_key(seq)),
    };
    sqlx::query(
        "INSERT INTO webhook_deliveries \
           (requester, seq, dedupe_key, target, transfer_id, condition, change) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (requester, dedupe_key) DO NOTHING",
    )
    .bind(requester)
    .bind(seq)
    .bind(&key)
    .bind(target)
    .bind(transfer)
    .bind(condition.map(|(c, _)| c))
    .bind(condition.map(|(_, change)| change))
    .execute(conn)
    .await
    .map(|r| r.rows_affected())
}

/// One signed POST, after the send-time guard (PR #55 review M1): the host is resolved once and
/// every address vetted, and the connection goes only to those addresses. The response body is
/// never read. Never logs the URL, which may carry the requester's token, nor an address.
async fn post(
    settings: &Webhooks,
    url: &url::Url,
    secret: &[u8],
    dedupe_key: &str,
    body: &str,
) -> Attempt {
    let pinned = match settings.vet(url).await {
        Ok(pinned) => pinned,
        Err(class) => return Attempt::Failed(None, class, "refused before sending".into()),
    };
    let client = match settings.client(
        pinned
            .as_ref()
            .map(|(domain, addrs)| (domain.as_str(), addrs.as_slice())),
    ) {
        Ok(client) => client,
        Err(_) => {
            return Attempt::Failed(None, class::REQUEST_FAILED, "no client".into());
        }
    };
    let timestamp = chrono::Utc::now().timestamp();
    let sent = client
        .post(url.as_str())
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(TIMESTAMP_HEADER, timestamp.to_string())
        .header(SIGNATURE_HEADER, signature(secret, timestamp, body))
        .header(DEDUPE_HEADER, dedupe_key)
        .body(body.to_owned())
        .send()
        .await;
    match sent {
        // Dropped unread: no response body is taken in.
        Ok(response) if response.status().is_success() => {
            Attempt::Acknowledged(response.status().as_u16())
        }
        Ok(response) => Attempt::Failed(
            Some(response.status().as_u16()),
            class::HTTP_STATUS,
            format!("answered {}", response.status()),
        ),
        Err(e) => {
            let class = if e.is_timeout() {
                class::TIMEOUT
            } else if e.is_connect() {
                class::CONNECT_FAILED
            } else {
                class::REQUEST_FAILED
            };
            let mut detail = e.without_url().to_string();
            detail.truncate(300);
            Attempt::Failed(None, class, detail)
        }
    }
}

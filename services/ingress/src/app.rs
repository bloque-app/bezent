// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTP service: state, router, extractors, start-up and graceful shutdown (T-002-01).

use crate::config::Config;
use crate::error::ApiError;
use crate::webhooks::{self, Dispatcher, Webhooks};
use crate::{intake, query};
use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequest, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use serde::de::DeserializeOwned;
use sqlx::PgPool;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

/// The largest request body taken, in bytes.
const BODY_LIMIT: usize = 64 * 1024;

/// What bounds the work the ingress takes on (`REQ-LM-4`).
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Requests taken in at once; one more is refused with `ERR-Overloaded`.
    pub max_in_flight: usize,
    /// Log positions one read covers.
    pub log_page: i64,
}

/// The service's shared state.
#[derive(Clone)]
pub struct App {
    pool: PgPool,
    permits: Arc<Semaphore>,
    log_page: i64,
    webhooks: Option<Arc<Webhooks>>,
    /// When the store last answered (seconds since the epoch): a `/health` query, or a request that
    /// succeeded. `/health` may call a saturated pool alive only while this is recent (issue #61).
    store_answered: Arc<AtomicU64>,
}

/// How long ago the store may last have answered for a saturated pool to still count as alive.
pub const BUSY_WINDOW: Duration = Duration::from_secs(60);

/// The longest one statement may run on the ingress's connections.
const STATEMENT_TIMEOUT: &str = "30s";

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl App {
    /// The service on a store pool. The pool's login must be a member of `bezent_service`.
    pub fn new(pool: PgPool, limits: Limits) -> Self {
        Self {
            pool,
            permits: Arc::new(Semaphore::new(limits.max_in_flight)),
            log_page: limits.log_page.max(1),
            webhooks: None,
            store_answered: Arc::new(AtomicU64::new(now_secs())),
        }
    }

    /// Notes that the store answered just now.
    pub(crate) fn store_answered(&self) {
        self.store_answered.store(now_secs(), Ordering::Relaxed);
    }

    /// Whether the store answered within [`BUSY_WINDOW`].
    pub(crate) fn store_answered_recently(&self) -> bool {
        let at = self.store_answered.load(Ordering::Relaxed);
        now_secs().saturating_sub(at) <= BUSY_WINDOW.as_secs()
    }

    /// For tests: dates the store's last answer `ago` back.
    #[doc(hidden)]
    pub fn set_store_answered_ago(&self, ago: Duration) {
        self.store_answered
            .store(now_secs().saturating_sub(ago.as_secs()), Ordering::Relaxed);
    }

    /// The service with webhooks (`REQ-ST-5`): it takes registrations and hands out signing
    /// secrets. The deliveries are the [`Dispatcher`]'s, which runs beside it.
    pub fn with_webhooks(mut self, webhooks: Webhooks) -> Self {
        self.webhooks = Some(Arc::new(webhooks));
        self
    }

    /// The webhook settings, or `webhooks-disabled`.
    pub(crate) fn webhooks(&self) -> Result<&Webhooks, ApiError> {
        self.webhooks.as_deref().ok_or(ApiError::WebhooksDisabled)
    }

    /// The store pool.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The permits that bound the requests taken in at once (`REQ-LM-4`).
    pub fn intake_permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.permits)
    }

    /// A permit to take a request in, or `ERR-Overloaded` (`REQ-LM-4`).
    pub(crate) fn permit(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| ApiError::Overloaded)
    }

    pub(crate) fn log_page(&self) -> i64 {
        self.log_page
    }
}

/// A JSON body whose refusal is an [`ApiError::Malformed`]: an unknown field, such as a claimed
/// outcome, is named in it (`REQ-RQ-7`).
pub(crate) struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::Malformed(rejection.body_text())),
        }
    }
}

/// Query parameters whose refusal is an [`ApiError::Malformed`].
pub(crate) struct Params<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequestParts<S> for Params<T> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match axum::extract::Query::<T>::from_request_parts(parts, state).await {
            Ok(axum::extract::Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::Malformed(rejection.body_text())),
        }
    }
}

/// The request and query surfaces.
pub fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(query::health))
        .route("/v0/deposits", post(intake::deposit))
        .route("/v0/withdrawals/extras", post(intake::withdrawal_extras))
        .route("/v0/refunds", post(intake::refund))
        .route(
            "/v0/transfers/by-request/{reference}",
            get(query::by_request),
        )
        .route("/v0/transfers/by-bridge/{reference}", get(query::by_bridge))
        .route("/v0/transfers/by-coordinate", get(query::by_coordinate))
        .route("/v0/routes", get(query::routes))
        .route("/v0/reserves", get(query::reserves))
        .route("/v0/reserve-equation", get(query::reserve_equation))
        .route("/v0/log", get(query::log))
        .route("/v0/webhooks/secret", post(webhooks::api::rotate_secret))
        .route(
            "/v0/webhooks/conditions",
            put(webhooks::api::register_conditions),
        )
        .route(
            "/v0/webhooks/transfers/by-request/{reference}",
            put(webhooks::api::register_by_request),
        )
        .route(
            "/v0/webhooks/transfers/by-bridge/{reference}",
            put(webhooks::api::register_by_bridge),
        )
        .route("/v0/webhooks/deliveries", get(webhooks::api::deliveries))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(app.clone(), note_answers))
        .with_state(app)
}

/// A request that succeeded had the store answer it: noted for `/health` (issue #61).
async fn note_answers(State(app): State<App>, request: Request, next: Next) -> Response {
    let health = request.uri().path() == "/health";
    let response = next.run(request).await;
    if !health && response.status().is_success() {
        app.store_answered();
    }
    response
}

/// Connects to the store, checks it answers, and checks the login is a member of `bezent_ingress`
/// and of nothing else, holding exactly the ingress's grants (#46, `REQ-AR-3`). Fails closed: the
/// ingress does not start without its store, or on any other login.
pub async fn connect(config: &Config) -> Result<PgPool, String> {
    connect_with(config, &config.resolver()?).await
}

/// [`connect`], reading the secrets through `resolver` (one login to Vault for the start).
pub async fn connect_with(
    config: &Config,
    resolver: &bezent_vault::Resolver,
) -> Result<PgPool, String> {
    let url = config.store_url(resolver).await?;
    let pool = bezent_store::tls::connect(
        bezent_store::login::pool_options()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_millis(config.acquire_timeout_ms))
            // A statement that hangs ends, and frees its connection (issue #61).
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query(&format!("SET statement_timeout = '{STATEMENT_TIMEOUT}'"))
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            }),
        &url,
    )
    .await
    .map_err(|e| format!("cannot connect to the store: {e}"))?;
    sqlx::query("SELECT 1 FROM event_log_head")
        .execute(&pool)
        .await
        .map_err(|e| format!("the store is not migrated or not readable: {e}"))?;
    bezent_store::login::check_login(&pool, bezent_store::INGRESS_ROLE).await?;
    Ok(pool)
}

/// Serves `app` on `listener` until `shutdown` resolves, then finishes the requests in flight.
pub async fn run(
    listener: TcpListener,
    app: App,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(app))
        .with_graceful_shutdown(shutdown)
        .await
}

/// Connects to the store, binds the configured address, and serves until `shutdown` resolves. With
/// `[webhooks]` configured, the dispatcher runs beside it, on a pool of its own so it never takes
/// the intake's connections, as its own login (a member of `bezent_webhooks` only, PR #55 review
/// M2), and stops with it.
pub async fn serve(
    config: Config,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), String> {
    let resolver = config.resolver()?;
    let pool = connect_with(&config, &resolver).await?;
    let hooks = match &config.webhooks {
        Some(w) => {
            let production = bezent_store::environment::production_networks(&pool)
                .await
                .map_err(|e| format!("store: {e}"))?;
            w.confirm_environment(&production)?;
            Some(w.settings(w.sealing_key(&resolver).await?)?)
        }
        None => None,
    };
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!(listen = %config.listen, webhooks = hooks.is_some(), "bezent-ingress serving");
    let mut app = App::new(
        pool.clone(),
        Limits {
            max_in_flight: config.max_in_flight,
            log_page: config.log_page,
        },
    );
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut dispatcher = None;
    if let (Some(hooks), Some(w)) = (hooks, &config.webhooks) {
        app = app.with_webhooks(hooks.clone());
        let dispatch_pool = bezent_store::tls::connect(
            bezent_store::login::pool_options()
                .max_connections(2)
                .acquire_timeout(Duration::from_secs(5)),
            &w.store_url(&resolver).await?,
        )
        .await
        .map_err(|e| format!("cannot connect the webhook dispatcher to the store: {e}"))?;
        // Its own login: a member of bezent_webhooks and of nothing else (#46; PR #55 review M2).
        bezent_store::login::check_login(&dispatch_pool, bezent_store::WEBHOOKS_ROLE)
            .await
            .map_err(|e| format!("[webhooks] database_url: {e}"))?;
        let d = Dispatcher::new(dispatch_pool.clone(), hooks)?;
        dispatcher = Some((tokio::spawn(d.run(stop_rx)), dispatch_pool));
    }
    let stopping = async move {
        shutdown.await;
        let _ = stop_tx.send(true);
    };
    let served = run(listener, app, stopping)
        .await
        .map_err(|e| format!("serving failed: {e}"));
    if let Some((task, dispatch_pool)) = dispatcher {
        let _ = task.await;
        dispatch_pool.close().await;
    }
    pool.close().await;
    tracing::info!("bezent-ingress stopped");
    served
}

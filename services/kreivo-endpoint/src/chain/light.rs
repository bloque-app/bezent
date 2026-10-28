// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Kreivo through subxt over the embedded smoldot light client (feature `light-client`; PLAN
//! `DEC-5`).
//!
//! **The light client** is smoldot-light 2.x behind subxt's [`RpcClientT`], as aledgent's settle
//! adapter runs it (`services/settle/src/chain/light.rs`, from which [`LightRpc`] is taken): one
//! smoldot client, the relay chain added first and Kreivo against it, and a task that routes each
//! JSON-RPC response to its request and each notification to its subscription. The workspace
//! builds smoldot from aledgent's fork, pinned by commit (`[patch.crates-io]`): released smoldot
//! recalculates the root of every child trie a runtime call wrote, which the node's call proof does
//! not cover, so a dry run of a call that writes contract storage failed (aledgent#121). Every
//! mint writes the contract's storage.
//!
//! **Nothing is trusted from a node**: smoldot verifies the relay-chain-finalised head itself
//! (GRANDPA), and every storage read and runtime call it answers is checked against a proof. The
//! endpoint reads at named blocks only, and reaches blocks by parent hash from the finalized head:
//! the light client answers no lookup by number.
//!
//! **What is read**: `System::Events` at a block (decoded with the runtime's metadata: the
//! contract's `ContractEmitted` with their topics, and every extrinsic's result),
//! `Timestamp::Now`, `System::Account` (the nonce), the block body, and `ContractsApi_call` for dry
//! runs. A runtime upgrade between the metadata subxt holds and an old block's events can make
//! that block's events fail to decode: the block is then a visible gap, never a guess.
//!
//! **History** (issue #49): reads at blocks more than `light_client_depth` behind the finalized
//! head (the events, a nonce, a body) go to the archive sources, verified ([`super::archive`]);
//! a header known to be that old too. Each falls back to the other side when its first one
//! fails. Everything at the finalized head, runtime calls included, stays on the light client.

use std::collections::HashMap;
use std::num::NonZero;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use parity_scale_codec::{Decode, Encode};
use scale_value::{Composite, Primitive, Value, ValueDef};
use serde::Deserialize;
use smoldot_light::platform::DefaultPlatform;
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::reconnecting_rpc_client::{ExponentialBackoff, RpcClient as Reconnecting};
use subxt::backend::rpc::{RawRpcFuture, RawRpcSubscription, RawValue, RpcClient, RpcClientT};
use subxt::config::Header as _;
use subxt::ext::subxt_rpcs::{Error as RpcError, UserError};
use subxt::tx::SubmittableTransaction;
use subxt::utils::H256;
use subxt::{OnlineClient, PolkadotConfig};
use tokio::sync::{mpsc, oneshot};

use super::archive::{Archive, Fail, Source, StartCheck};
use super::{
    BlockEvents, BlockRef, Chain, ChainError, ChainResult, Emitted, ExtrinsicResult, Header,
    RuntimeVersion,
};
use crate::kreivo::{AccountId32, blake2_256};

// ------------------------------------------------------------------------------ the light client

/// Tasks that stop when this is dropped, however the owner is left (an error, a cancelled future).
struct AbortOnDrop(Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// What the light client's bootstrap has shown so far, to tell progress from standing still.
#[derive(Default)]
struct WarmUp {
    at: Option<u64>,
    ready: bool,
    started: bool,
}

impl WarmUp {
    /// Whether `state` is a step forward: the first state with peers, a warp sync that reached a
    /// higher block, or the sync that finished with peers. A stalled chain (no peers, a warp sync
    /// that does not advance) and a state that only changes the peer count are not.
    fn advanced(&mut self, state: &smoldot_light::lifecycle_service::LifecycleState) -> bool {
        use smoldot_light::lifecycle_service::{Health, Phase};
        if state.health != Health::Ok || state.num_peers == 0 {
            return false;
        }
        match state.phase {
            Phase::Connecting => !std::mem::replace(&mut self.started, true),
            Phase::Syncing { at, .. } => {
                self.started = true;
                let forward = self.at.is_none_or(|last| at > last);
                self.at = Some(at.max(self.at.unwrap_or(0)));
                forward
            }
            Phase::Ready => !std::mem::replace(&mut self.ready, true),
        }
    }
}

type Client = smoldot_light::Client<Arc<DefaultPlatform>, ()>;
type Notifications = mpsc::UnboundedSender<Result<Box<RawValue>, RpcError>>;

struct Pending {
    reply: oneshot::Sender<Result<Box<RawValue>, RpcError>>,
    notifications: Option<Notifications>,
}

struct Inner {
    client: Mutex<Client>,
    chain: smoldot_light::ChainId,
    /// The relay chain, whose warp sync is the long part of a first start: a parachain's own sync
    /// service reports ready at once.
    relay: Option<smoldot_light::ChainId>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<String, Pending>>,
    subscriptions: Mutex<HashMap<String, Notifications>>,
}

/// The light client's JSON-RPC, for subxt's [`RpcClient`].
#[derive(Clone)]
pub struct LightRpc {
    inner: Arc<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn client_error(e: impl std::fmt::Display) -> RpcError {
    RpcError::Client(e.to_string().into())
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    error: Option<Box<RawValue>>,
    #[serde(default)]
    params: Option<NotificationParams>,
}

#[derive(Deserialize)]
struct NotificationParams {
    subscription: serde_json::Value,
    #[serde(default, deserialize_with = "present")]
    result: Option<Box<RawValue>>,
    #[serde(default, deserialize_with = "present")]
    error: Option<Box<RawValue>>,
}

/// A field that is there, whatever its value, `null` included.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Box<RawValue>>, D::Error> {
    Box::<RawValue>::deserialize(d).map(Some)
}

fn id_key(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn error_of(raw: &RawValue) -> RpcError {
    match serde_json::from_str::<UserError>(raw.get()) {
        Ok(user) => RpcError::User(user),
        Err(_) => client_error(raw.get()),
    }
}

impl LightRpc {
    /// The light client's bootstrap state of the relay chain (when there is one) and of Kreivo: the
    /// phase, the peers and the stall verdict, current first and then at each change (the pinned
    /// smoldot fork's lifecycle service).
    pub fn lifecycle(&self) -> Vec<smoldot_light::lifecycle_service::Subscription> {
        let client = lock(&self.inner.client);
        self.inner
            .relay
            .into_iter()
            .chain([self.inner.chain])
            .map(|id| client.lifecycle_state(id))
            .collect()
    }

    /// Starts a light client for `chain_spec`, a parachain of `relay_chain_spec` when one is
    /// given. Must be called inside a Tokio runtime.
    pub fn new(chain_spec: &str, relay_chain_spec: Option<&str>) -> Result<LightRpc, String> {
        let platform = DefaultPlatform::new(
            "bezent-kreivo-endpoint".into(),
            env!("CARGO_PKG_VERSION").into(),
        );
        let mut client = Client::new(platform);
        let relay = match relay_chain_spec {
            Some(spec) => {
                let added = client
                    .add_chain(smoldot_light::AddChainConfig {
                        user_data: (),
                        specification: spec,
                        database_content: "",
                        potential_relay_chains: std::iter::empty(),
                        json_rpc: smoldot_light::AddChainConfigJsonRpc::Disabled,
                        statement_protocol_config: None,
                    })
                    .map_err(|e| format!("the relay chain: {e}"))?;
                Some(added.chain_id)
            }
            None => None,
        };
        let added = client
            .add_chain(smoldot_light::AddChainConfig {
                user_data: (),
                specification: chain_spec,
                database_content: "",
                potential_relay_chains: relay.into_iter(),
                json_rpc: smoldot_light::AddChainConfigJsonRpc::Enabled {
                    max_pending_requests: NonZero::<u32>::MAX,
                    max_subscriptions: u32::MAX,
                },
                statement_protocol_config: None,
            })
            .map_err(|e| format!("the chain: {e}"))?;
        let mut responses = added
            .json_rpc_responses
            .ok_or("the chain was added without JSON-RPC")?;
        let inner = Arc::new(Inner {
            client: Mutex::new(client),
            chain: added.chain_id,
            relay,
            next_id: AtomicU64::new(1),
            pending: Mutex::default(),
            subscriptions: Mutex::default(),
        });
        let weak = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while let Some(text) = responses.next().await {
                let Some(inner) = weak.upgrade() else { break };
                inner.route(&text);
            }
        });
        Ok(LightRpc { inner })
    }
}

impl Inner {
    fn route(&self, text: &str) {
        let Ok(m) = serde_json::from_str::<Message>(text) else {
            return;
        };
        if let Some(id) = &m.id {
            let Some(p) = lock(&self.pending).remove(&id_key(id)) else {
                return;
            };
            let reply = match (m.result, m.error) {
                (Some(result), _) => {
                    if let Some(tx) = p.notifications {
                        match serde_json::from_str::<serde_json::Value>(result.get()) {
                            Ok(sub) => {
                                lock(&self.subscriptions).insert(id_key(&sub), tx);
                            }
                            Err(e) => {
                                let _ = p.reply.send(Err(RpcError::Deserialization(e)));
                                return;
                            }
                        }
                    }
                    Ok(result)
                }
                (None, Some(error)) => Err(error_of(&error)),
                (None, None) => Err(client_error("a response with neither result nor error")),
            };
            let _ = p.reply.send(reply);
            return;
        }
        let Some(params) = m.params else { return };
        let key = id_key(&params.subscription);
        let item = match (params.result, params.error) {
            (Some(r), _) => Ok(r),
            (None, Some(e)) => Err(error_of(&e)),
            (None, None) => return,
        };
        let mut subs = lock(&self.subscriptions);
        if let Some(tx) = subs.get(&key)
            && tx.send(item).is_err()
        {
            subs.remove(&key);
        }
    }

    fn send(
        &self,
        method: &str,
        params: Option<Box<RawValue>>,
        notifications: Option<Notifications>,
    ) -> oneshot::Receiver<Result<Box<RawValue>, RpcError>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let (reply, rx) = oneshot::channel();
        lock(&self.pending).insert(
            id.clone(),
            Pending {
                reply,
                notifications,
            },
        );
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":"{id}","method":{},"params":{}}}"#,
            serde_json::Value::String(method.to_owned()),
            params.as_ref().map_or("[]", |p| p.get())
        );
        if let Err(e) = lock(&self.client).json_rpc_request(request, self.chain)
            && let Some(p) = lock(&self.pending).remove(&id)
        {
            let _ = p.reply.send(Err(client_error(format!(
                "the light client refused {method}: {e:?}"
            ))));
        }
        rx
    }
}

struct Subscription {
    rx: mpsc::UnboundedReceiver<Result<Box<RawValue>, RpcError>>,
    inner: Arc<Inner>,
    id: String,
    unsubscribe: String,
}

impl subxt::ext::futures::Stream for Subscription {
    type Item = Result<Box<RawValue>, RpcError>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        lock(&self.inner.subscriptions).remove(&self.id);
        let params = serde_json::value::to_raw_value(&[&self.id]).ok();
        drop(self.inner.send(&self.unsubscribe, params, None));
    }
}

impl RpcClientT for LightRpc {
    fn request_raw<'a>(
        &'a self,
        method: &'a str,
        params: Option<Box<RawValue>>,
    ) -> RawRpcFuture<'a, Box<RawValue>> {
        let rx = self.inner.send(method, params, None);
        Box::pin(async move {
            rx.await
                .map_err(|_| client_error("the light client stopped"))?
        })
    }

    fn subscribe_raw<'a>(
        &'a self,
        sub: &'a str,
        params: Option<Box<RawValue>>,
        unsub: &'a str,
    ) -> RawRpcFuture<'a, RawRpcSubscription> {
        let (tx, notifications) = mpsc::unbounded_channel();
        let rx = self.inner.send(sub, params, Some(tx));
        let inner = self.inner.clone();
        Box::pin(async move {
            let result = rx
                .await
                .map_err(|_| client_error("the light client stopped"))??;
            let id = id_key(
                &serde_json::from_str::<serde_json::Value>(result.get())
                    .map_err(RpcError::Deserialization)?,
            );
            Ok(RawRpcSubscription {
                id: Some(id.clone()),
                stream: Box::pin(Subscription {
                    rx: notifications,
                    inner,
                    id,
                    unsubscribe: unsub.to_owned(),
                }),
            })
        })
    }
}

// ------------------------------------------------------------------------------ history sources

/// One archive node's JSON-RPC (issue #49), over one long-lived WebSocket connection that
/// re-opens itself with backoff: a node's connections are shared with all its clients, so a
/// replay never opens one per request. Each request waits at most `timeout`.
pub struct WsSource {
    url: String,
    name: String,
    timeout: Duration,
    max_response: u32,
    client: tokio::sync::Mutex<Option<Arc<Reconnecting>>>,
}

/// The most characters of what a server or a transport said that an error keeps.
const ERROR_TEXT: usize = 160;

fn h256(v: &serde_json::Value) -> Option<[u8; 32]> {
    crate::config::hex_bytes(v.as_str()?, "a hash").ok()
}

fn hex_vec(v: &serde_json::Value) -> Option<Vec<u8>> {
    hex::decode(v.as_str()?.strip_prefix("0x")?).ok()
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

/// The SCALE encoding of a header as JSON-RPC renders it (each digest log is already SCALE).
pub fn scale_header(v: &serde_json::Value) -> Option<Vec<u8>> {
    let mut out = h256(v.get("parentHash")?)?.to_vec();
    let number = v.get("number")?.as_str()?.strip_prefix("0x")?;
    parity_scale_codec::Compact(u32::from_str_radix(number, 16).ok()?).encode_to(&mut out);
    out.extend(h256(v.get("stateRoot")?)?);
    out.extend(h256(v.get("extrinsicsRoot")?)?);
    let logs = v.pointer("/digest/logs")?.as_array()?;
    parity_scale_codec::Compact(u32::try_from(logs.len()).ok()?).encode_to(&mut out);
    for log in logs {
        out.extend(hex_vec(log)?);
    }
    Some(out)
}

impl WsSource {
    /// A source at `url`; it connects on its first request. An answer longer than `max_response`
    /// bytes is a failed request.
    pub fn new(url: &str, timeout: Duration, max_response: u32) -> WsSource {
        WsSource {
            url: url.to_owned(),
            name: crate::config::source_name(url),
            timeout,
            max_response,
            client: tokio::sync::Mutex::default(),
        }
    }

    /// `text` as an error message may be logged and recorded: no part of the URL (its path may
    /// hold a key), and at most [`ERROR_TEXT`] characters of what a server or a transport said.
    fn scrub(&self, text: &str) -> String {
        let rest = self.url.split_once("://").map(|(_, rest)| rest);
        let tail = rest
            .and_then(|r| r.split_once(['/', '?', '#']))
            .map(|(_, tail)| tail);
        let userinfo = rest
            .and_then(|r| r.split(['/', '?', '#']).next())
            .and_then(|authority| authority.rsplit_once('@'))
            .map(|(userinfo, _)| userinfo);
        // The URL whole and in its parts, then every token of the parts that could be a key.
        let parts = [Some(self.url.as_str()), rest, tail, userinfo];
        let tokens = [tail, userinfo]
            .into_iter()
            .flatten()
            .flat_map(|p| p.split(['/', '?', '#', '&', '=', ':', '@']))
            .filter(|t| t.chars().count() >= 4);
        let mut out = text.to_owned();
        for secret in parts.into_iter().flatten().chain(tokens) {
            out = out.replace(secret, "<source>");
        }
        out.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(ERROR_TEXT)
            .collect()
    }

    async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, Fail> {
        let client = {
            let mut held = self.client.lock().await;
            match &*held {
                Some(c) => c.clone(),
                None => {
                    let build = Reconnecting::builder()
                        .retry_policy(
                            ExponentialBackoff::from_millis(10).max_delay(Duration::from_secs(30)),
                        )
                        .request_timeout(self.timeout)
                        .max_response_size(self.max_response)
                        .build(&self.url);
                    let c = match tokio::time::timeout(self.timeout, build).await {
                        Ok(Ok(c)) => Arc::new(c),
                        // The transport's error can carry the URL: none of it is kept.
                        Ok(Err(_)) => return Err(Fail::Unavailable("cannot connect".into())),
                        Err(_) => return Err(Fail::Unavailable("cannot connect in time".into())),
                    };
                    *held = Some(c.clone());
                    c
                }
            }
        };
        let params = serde_json::value::to_raw_value(&params)
            .map_err(|_| Fail::Unavailable("the request cannot be encoded".into()))?;
        match tokio::time::timeout(self.timeout, client.request_raw(method, Some(params))).await {
            Err(_) => Err(Fail::Unavailable(format!(
                "{method}: no answer in {:?}",
                self.timeout
            ))),
            Ok(Err(RpcError::User(e))) => Err(Fail::Missing(format!(
                "{method}: {}",
                self.scrub(&e.message)
            ))),
            Ok(Err(e)) => Err(Fail::Unavailable(format!(
                "{method}: {}",
                self.scrub(&e.to_string())
            ))),
            Ok(Ok(raw)) => serde_json::from_str(raw.get())
                .map_err(|_| Fail::Lied(format!("{method}: not JSON"))),
        }
    }
}

impl Source for WsSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn block_hash(&self, number: u64) -> Result<Option<[u8; 32]>, Fail> {
        let v = self
            .call("chain_getBlockHash", serde_json::json!([number]))
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        h256(&v)
            .map(Some)
            .ok_or_else(|| Fail::Lied("chain_getBlockHash: not a hash".into()))
    }

    async fn header(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Fail> {
        let v = self
            .call("chain_getHeader", serde_json::json!([hex0x(hash)]))
            .await?;
        if v.is_null() {
            return Err(Fail::Missing(format!("no header for {}", hex0x(hash))));
        }
        scale_header(&v).ok_or_else(|| Fail::Lied("chain_getHeader: not a header".into()))
    }

    async fn read_proof(&self, at: &[u8; 32], keys: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, Fail> {
        let keys: Vec<String> = keys.iter().map(|k| hex0x(k)).collect();
        let v = self
            .call("state_getReadProof", serde_json::json!([keys, hex0x(at)]))
            .await?;
        v.get("proof")
            .and_then(|p| p.as_array())
            .and_then(|nodes| nodes.iter().map(hex_vec).collect())
            .ok_or_else(|| Fail::Lied("state_getReadProof: not a proof".into()))
    }

    async fn body(&self, hash: &[u8; 32]) -> Result<Vec<Vec<u8>>, Fail> {
        let v = self
            .call("chain_getBlock", serde_json::json!([hex0x(hash)]))
            .await?;
        if v.is_null() {
            return Err(Fail::Missing(format!("no body for {}", hex0x(hash))));
        }
        v.pointer("/block/extrinsics")
            .and_then(|x| x.as_array())
            .and_then(|xs| xs.iter().map(hex_vec).collect())
            .ok_or_else(|| Fail::Lied("chain_getBlock: not a block".into()))
    }
}

// ------------------------------------------------------------------------------ the chain

fn unavailable(e: impl std::fmt::Display) -> ChainError {
    ChainError::Unavailable(e.to_string())
}

fn invalid(e: impl std::fmt::Display) -> ChainError {
    ChainError::Invalid(e.to_string())
}

fn field<'a>(v: &'a Value<u32>, name: &str) -> Option<&'a Value<u32>> {
    match &v.value {
        ValueDef::Composite(Composite::Named(fields)) => {
            fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
        }
        _ => None,
    }
}

fn number(v: &Value<u32>) -> Option<u128> {
    match &v.value {
        ValueDef::Primitive(Primitive::U128(n)) => Some(*n),
        ValueDef::Composite(c) => {
            let vals: Vec<&Value<u32>> = c.values().collect();
            if vals.len() == 1 {
                number(vals[0])
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The bytes a value holds, however its newtypes nest: a hash decodes as a composite of numbers,
/// or of a composite of numbers. Exactly 32 of them, each a byte, or nothing.
fn bytes32(v: &Value<u32>) -> Option<[u8; 32]> {
    fn leaves(v: &Value<u32>, out: &mut Vec<u8>) -> Option<()> {
        match &v.value {
            ValueDef::Primitive(Primitive::U128(n)) => out.push(u8::try_from(*n).ok()?),
            ValueDef::Composite(c) => {
                for x in c.values() {
                    leaves(x, out)?;
                }
            }
            _ => return None,
        }
        Some(())
    }
    let mut out = Vec::new();
    leaves(v, &mut out)?;
    out.try_into().ok()
}

/// Kreivo over the light client. See the module documentation.
pub struct LightChain {
    client: OnlineClient<PolkadotConfig>,
    rpc: LegacyRpcMethods<PolkadotConfig>,
    /// Every transaction submitted and still watched, by hash: smoldot-light panics when one
    /// transaction has two status watchers and one closes (aledgent F-027 NOTES, decision 40), so
    /// the same bytes are never watched twice at once.
    watching: Mutex<HashMap<[u8; 32], tokio::task::JoinHandle<()>>>,
    /// History beyond the light client's reach (issue #49).
    archive: Archive<WsSource>,
    /// Blocks further than this behind the finalized head are read from `archive` first.
    depth: u64,
    /// The finalized head's number, as last read.
    finalized: AtomicU64,
    /// Block numbers by hash, of each header read and its parent: how old a header asked for is.
    numbers: Mutex<HashMap<[u8; 32], u64>>,
}

/// Block numbers remembered by hash, at most.
const NUMBERS: usize = 262_144;

/// Whether block `number` is beyond the light client's reach: more than `depth` blocks behind
/// `finalized`.
fn is_old(finalized: u64, number: u64, depth: u64) -> bool {
    finalized.saturating_sub(number) > depth
}

/// A read at an old block asks the archive first, any other the light client first; each falls
/// back to the other side.
async fn routed<T>(
    old: bool,
    archive: impl std::future::Future<Output = ChainResult<T>>,
    light: impl std::future::Future<Output = ChainResult<T>>,
) -> ChainResult<T> {
    if old {
        either(archive, light).await
    } else {
        either(light, archive).await
    }
}

/// Tries `first`, then `second` when it fails. When both fail: `Invalid` if either is (a source
/// lied, or an answer cannot be used), else unknown, with both reasons.
async fn either<T>(
    first: impl std::future::Future<Output = ChainResult<T>>,
    second: impl std::future::Future<Output = ChainResult<T>>,
) -> ChainResult<T> {
    match first.await {
        Ok(v) => Ok(v),
        Err(e) => second.await.map_err(|other| {
            let lied =
                matches!(e, ChainError::Invalid(_)) || matches!(other, ChainError::Invalid(_));
            let (ChainError::Invalid(a) | ChainError::Unavailable(a)) = e;
            let (ChainError::Invalid(b) | ChainError::Unavailable(b)) = other;
            let why = format!("{a}; then {b}");
            if lied {
                ChainError::Invalid(why)
            } else {
                ChainError::Unavailable(why)
            }
        }),
    }
}

impl LightChain {
    /// Starts the light client on `chain_spec` (Kreivo), a parachain of `relay_chain_spec`, with
    /// `archive_rpc` for history older than `depth` blocks (issue #49).
    ///
    /// `progress` is called while the light client warms up, each time its bootstrap moves
    /// forward (a warp sync that reached a higher block, the sync that finished), and never while
    /// it is stalled or still: the container's health check counts those calls (issue #61).
    pub async fn start(
        chain_spec: &str,
        relay_chain_spec: Option<&str>,
        archive_rpc: &[String],
        depth: u64,
        timeout: Duration,
        max_response: u32,
        progress: impl Fn() + Send + Sync + 'static,
    ) -> ChainResult<LightChain> {
        let rpc = LightRpc::new(chain_spec, relay_chain_spec).map_err(unavailable)?;
        // Either chain moving forward is progress: the relay chain's warp sync is the long part.
        let progress = Arc::new(progress);
        let watchers = AbortOnDrop(
            rpc.lifecycle()
                .into_iter()
                .map(|mut states| {
                    let progress = progress.clone();
                    tokio::spawn(async move {
                        let mut seen = WarmUp::default();
                        while let Some(state) = states.next().await {
                            if seen.advanced(&state) {
                                progress();
                            }
                        }
                    })
                })
                .collect(),
        );
        let rpc_client = RpcClient::new(rpc);
        let client = OnlineClient::<PolkadotConfig>::from_rpc_client(rpc_client.clone()).await;
        // The warm-up is over, or failed: nothing is counted after it (`watchers` stop when
        // dropped, here or if this future is).
        drop(watchers);
        let client = client.map_err(unavailable)?;
        Ok(LightChain {
            client,
            rpc: LegacyRpcMethods::<PolkadotConfig>::new(rpc_client),
            watching: Mutex::default(),
            archive: Archive::new(
                archive_rpc
                    .iter()
                    .map(|u| WsSource::new(u, timeout, max_response))
                    .collect(),
            ),
            depth,
            finalized: AtomicU64::new(0),
            numbers: Mutex::default(),
        })
    }

    /// How often each history source lied or failed since start-up.
    pub fn history_counts(&self) -> Vec<super::archive::SourceCounts> {
        self.archive.counts()
    }

    /// Whether block `number` is beyond the light client's reach.
    fn old(&self, number: u64) -> bool {
        is_old(self.finalized.load(Ordering::Relaxed), number, self.depth)
    }

    fn learn(&self, h: &Header) {
        let mut numbers = lock(&self.numbers);
        if numbers.len() >= NUMBERS {
            numbers.clear();
        }
        numbers.insert(h.block.hash, h.block.number);
        if let Some(n) = h.block.number.checked_sub(1) {
            numbers.insert(h.parent, n);
        }
    }

    /// The storage key of `pallet::entry(keys)`.
    fn key(&self, pallet: &str, entry: &str, keys: Vec<Value<()>>) -> ChainResult<Vec<u8>> {
        let address = subxt::dynamic::storage(pallet, entry, keys);
        subxt::ext::subxt_core::storage::get_address_bytes(&address, &self.client.metadata())
            .map_err(invalid)
    }

    /// A value of `pallet::entry`, from its SCALE bytes.
    fn decode(&self, pallet: &str, entry: &str, bytes: &[u8]) -> ChainResult<Value<u32>> {
        let address = subxt::dynamic::storage(pallet, entry, Vec::<Value<()>>::new());
        subxt::ext::subxt_core::storage::decode_value(
            &mut &bytes[..],
            &address,
            &self.client.metadata(),
        )
        .map_err(invalid)?
        .to_value()
        .map_err(invalid)
    }

    /// `keys` at `at` from the archive, each proven.
    async fn archived(&self, at: &BlockRef, keys: &[Vec<u8>]) -> ChainResult<Vec<Option<Vec<u8>>>> {
        let values = self.archive.storage(&at.hash, keys).await?;
        if values.len() != keys.len() {
            return Err(invalid("a storage proof answered another number of keys"));
        }
        Ok(values)
    }

    async fn fetch(
        &self,
        at: &BlockRef,
        pallet: &str,
        entry: &str,
        keys: Vec<Value<()>>,
    ) -> ChainResult<Option<Value<u32>>> {
        let light = async {
            let thunk = self
                .client
                .storage()
                .at(H256(at.hash))
                .fetch(&subxt::dynamic::storage(pallet, entry, keys.clone()))
                .await
                .map_err(unavailable)?;
            thunk.map(|t| t.to_value().map_err(invalid)).transpose()
        };
        let archive = async {
            let key = self.key(pallet, entry, keys.clone())?;
            let value = self.archived(at, &[key]).await?.remove(0);
            value.map(|b| self.decode(pallet, entry, &b)).transpose()
        };
        routed(self.old(at.number), archive, light).await
    }

    async fn light_header(&self, hash: &[u8; 32]) -> ChainResult<Header> {
        let header = self
            .rpc
            .chain_get_header(Some(H256(*hash)))
            .await
            .map_err(unavailable)?
            .ok_or_else(|| unavailable(format!("no header for 0x{}", hex::encode(hash))))?;
        Ok(Header {
            block: BlockRef {
                number: header.number().into(),
                hash: *hash,
            },
            parent: header.parent_hash.0,
        })
    }

    async fn light_events(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> ChainResult<BlockEvents> {
        let time_ms = self
            .fetch(at, "Timestamp", "Now", Vec::new())
            .await?
            .as_ref()
            .and_then(number)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| invalid(format!("block {} has no Timestamp::Now", at.number)))?;
        let events = self
            .client
            .events()
            .at(H256(at.hash))
            .await
            .map_err(unavailable)?;
        block_events(time_ms, &events, contract)
    }

    /// `Timestamp::Now` and `System::Events` in one proof.
    async fn archive_events(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> ChainResult<BlockEvents> {
        let keys = [
            self.key("Timestamp", "Now", Vec::new())?,
            self.key("System", "Events", Vec::new())?,
        ];
        let mut values = self.archived(at, &keys).await?;
        let time_ms = values[0]
            .as_deref()
            .and_then(|b| self.decode("Timestamp", "Now", b).ok())
            .as_ref()
            .and_then(number)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| invalid(format!("block {} has no Timestamp::Now", at.number)))?;
        // No `System::Events` at all is no events: an empty list.
        let bytes = values[1].take().unwrap_or_else(|| vec![0]);
        let events = subxt::events::Events::decode_from(bytes, self.client.metadata());
        block_events(time_ms, &events, contract)
    }

    /// The start-up check of the history sources (issue #49; see [`Archive::check`]), at the
    /// light client's finalized head.
    pub async fn check_history(
        &self,
        genesis: [u8; 32],
        start_block: u64,
        start_hash: [u8; 32],
        contract: &AccountId32,
        code_hash: [u8; 32],
    ) -> Result<(), String> {
        let finalized = self.finalized().await.map_err(|e| e.to_string())?;
        let contract_key = self
            .key(
                "Contracts",
                "ContractInfoOf",
                vec![Value::from_bytes(contract)],
            )
            .map_err(|e| e.to_string())?;
        let decode = |b: &[u8]| {
            let v = self.decode("Contracts", "ContractInfoOf", b).ok()?;
            field(&v, "code_hash").and_then(bytes32)
        };
        self.archive
            .check(&StartCheck {
                genesis,
                finalized,
                start_block,
                start_hash,
                contract_key,
                code_hash,
                decode_code_hash: &decode,
                lag_retries: 10,
                lag_wait: Duration::from_secs(6),
            })
            .await
    }
}

/// What the endpoint reads of a block's events. See [`Chain::events`].
fn block_events(
    time_ms: u64,
    events: &subxt::events::Events<PolkadotConfig>,
    contract: &AccountId32,
) -> ChainResult<BlockEvents> {
    let mut emitted = Vec::new();
    let mut results = Vec::new();
    for ev in events.iter() {
        let ev = ev.map_err(invalid)?;
        match (ev.pallet_name(), ev.variant_name()) {
            ("Contracts", "ContractEmitted") => {
                let mut b = ev.field_bytes();
                let (who, data) = <(AccountId32, Vec<u8>)>::decode(&mut b).map_err(invalid)?;
                if who == *contract {
                    emitted.push(Emitted {
                        index: ev.index(),
                        topics: ev.topics().iter().map(|t| t.0).collect(),
                        data,
                    });
                }
            }
            ("System", name @ ("ExtrinsicSuccess" | "ExtrinsicFailed")) => {
                let subxt::events::Phase::ApplyExtrinsic(index) = ev.phase() else {
                    continue;
                };
                let success = name == "ExtrinsicSuccess";
                let error = (!success).then(|| {
                    ev.field_values().map_or_else(
                        |e| e.to_string(),
                        |c| {
                            Value::without_context(ValueDef::Composite(c.map_context(|_| ())))
                                .to_string()
                        },
                    )
                });
                results.push(ExtrinsicResult {
                    index,
                    success,
                    error,
                });
            }
            _ => {}
        }
    }
    Ok(BlockEvents {
        time_ms,
        emitted,
        results,
    })
}

impl Chain for LightChain {
    async fn genesis(&self) -> ChainResult<[u8; 32]> {
        Ok(self.rpc.genesis_hash().await.map_err(unavailable)?.0)
    }

    async fn runtime_version(&self, at: &BlockRef) -> ChainResult<RuntimeVersion> {
        let v = self
            .rpc
            .state_get_runtime_version(Some(H256(at.hash)))
            .await
            .map_err(unavailable)?;
        Ok(RuntimeVersion {
            spec_version: v.spec_version,
            transaction_version: v.transaction_version,
        })
    }

    async fn finalized(&self) -> ChainResult<BlockRef> {
        let hash = self
            .rpc
            .chain_get_finalized_head()
            .await
            .map_err(unavailable)?;
        let head = self.light_header(&hash.0).await?;
        self.learn(&head);
        self.finalized
            .fetch_max(head.block.number, Ordering::Relaxed);
        Ok(head.block)
    }

    async fn header(&self, hash: &[u8; 32]) -> ChainResult<Header> {
        let archive = async { Ok(self.archive.header(hash).await?.header) };
        let old = lock(&self.numbers).get(hash).is_some_and(|n| self.old(*n));
        let header = routed(old, archive, self.light_header(hash)).await?;
        self.learn(&header);
        Ok(header)
    }

    async fn events(&self, at: &BlockRef, contract: &AccountId32) -> ChainResult<BlockEvents> {
        routed(
            self.old(at.number),
            self.archive_events(at, contract),
            self.light_events(at, contract),
        )
        .await
    }

    async fn extrinsics(&self, at: &BlockRef) -> ChainResult<Vec<Vec<u8>>> {
        let light = async {
            let block = self
                .rpc
                .chain_get_block(Some(H256(at.hash)))
                .await
                .map_err(unavailable)?
                .ok_or_else(|| unavailable(format!("no body for block {}", at.number)))?;
            Ok(block.block.extrinsics.into_iter().map(|b| b.0).collect())
        };
        let archive = self.archive.extrinsics(&at.hash);
        routed(self.old(at.number), archive, light).await
    }

    async fn account_nonce(&self, at: &BlockRef, account: &AccountId32) -> ChainResult<u64> {
        let Some(v) = self
            .fetch(at, "System", "Account", vec![Value::from_bytes(account)])
            .await?
        else {
            return Ok(0);
        };
        field(&v, "nonce")
            .and_then(number)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| invalid("an account without a nonce"))
    }
    async fn asset_issuance(&self, at: &BlockRef, asset: u32) -> ChainResult<Option<u128>> {
        // Kreivo's `pallet_assets::Config::AssetId` is `FungibleAssetLocation`; a bridged asset is
        // `Here(index)` (kreivo `common/src/multilocation_asset_id.rs`).
        let key = Value::unnamed_variant("Here", [Value::u128(u128::from(asset))]);
        let Some(v) = self.fetch(at, "Assets", "Asset", vec![key]).await? else {
            return Ok(None);
        };
        field(&v, "supply")
            .and_then(number)
            .map(Some)
            .ok_or_else(|| invalid("an asset without a supply"))
    }

    async fn contract_code_hash(
        &self,
        at: &BlockRef,
        contract: &AccountId32,
    ) -> ChainResult<Option<[u8; 32]>> {
        // `ContractInfo { trie_id, code_hash, .. }`: read by field name, so a layout that moves
        // other fields does not matter, and one without a code hash is refused, not guessed at.
        let Some(v) = self
            .fetch(
                at,
                "Contracts",
                "ContractInfoOf",
                vec![Value::from_bytes(contract)],
            )
            .await?
        else {
            return Ok(None);
        };
        field(&v, "code_hash")
            .and_then(bytes32)
            .map(Some)
            .ok_or_else(|| invalid("a contract without a 32-byte code hash"))
    }

    async fn contracts_api_call(&self, at: &BlockRef, args: &[u8]) -> ChainResult<Vec<u8>> {
        self.rpc
            .state_call("ContractsApi_call", Some(args), Some(H256(at.hash)))
            .await
            .map_err(unavailable)
    }

    async fn submit(&self, extrinsic: &[u8]) -> ChainResult<()> {
        let hash = blake2_256(extrinsic);
        {
            let mut watching = lock(&self.watching);
            watching.retain(|_, w| !w.is_finished());
            if watching.contains_key(&hash) {
                return Ok(());
            }
        }
        let tx = SubmittableTransaction::from_bytes(self.client.clone(), extrinsic.to_vec());
        match tx.submit_and_watch().await {
            Ok(mut progress) => {
                let watch = tokio::spawn(async move {
                    while let Some(status) = progress.next().await {
                        match status {
                            Ok(s) => {
                                tracing::debug!(hash = %hex::encode(hash), status = ?std::mem::discriminant(&s), "a submitted transaction's status")
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, hash = %hex::encode(hash), "a submitted transaction's watch ended");
                                break;
                            }
                        }
                    }
                });
                lock(&self.watching).insert(hash, watch);
                Ok(())
            }
            // The bytes are persisted and sent again while their era lasts: a node's refusal is
            // not an outcome (REQ-NX-3).
            Err(e) => Err(unavailable(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The history side of the light chain (issue #49): the JSON-to-SCALE header, the source over a
    //! real WebSocket (reconnects, errors, the response cap), `either` and the routing. No test
    //! reaches a real network; the light client itself is not started.

    use std::sync::atomic::AtomicUsize;

    use serde_json::{Value, json};
    use soketto::handshake::{Server, server::Response};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::compat::TokioAsyncReadCompatExt;

    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../../fixtures/archive-39867043.json")).unwrap()
    }

    // -------------------------------------------------------------------------- scale_header

    #[test]
    fn issue_49_the_json_header_is_the_scale_header_that_hashes_to_its_hash() {
        let f = fixture();
        let bytes = scale_header(&f["header_json"]).expect("a header");
        // As the archive served it: the very bytes whose BLAKE2b-256 is the block's hash.
        assert_eq!(hex0x(&bytes), f["header"].as_str().unwrap());
        assert_eq!(hex0x(&blake2_256(&bytes)), f["hash"].as_str().unwrap());
        let verified = super::super::archive::verify_header(&h256(&f["hash"]).unwrap(), &bytes)
            .expect("verifies");
        assert_eq!(verified.header.block.number, 39_867_043);
    }

    #[test]
    fn issue_49_a_json_header_that_is_not_one_is_refused_not_guessed() {
        let good = fixture()["header_json"].clone();
        let broken = |f: &dyn Fn(&mut Value)| {
            let mut v = good.clone();
            f(&mut v);
            scale_header(&v)
        };
        assert!(broken(&|v| _ = v.as_object_mut().unwrap().remove("stateRoot")).is_none());
        assert!(broken(&|v| v["parentHash"] = json!("0x1234")).is_none());
        assert!(broken(&|v| v["number"] = json!("39867043")).is_none());
        assert!(broken(&|v| v["number"] = json!("0x1ffffffff")).is_none());
        assert!(broken(&|v| v["digest"]["logs"][0] = json!("not hex")).is_none());
        // Another number is another header, which then does not hash to the hash asked for.
        let other = broken(&|v| v["number"] = json!("0x26052a4")).unwrap();
        assert_ne!(other, hex_vec(&fixture()["header"]).unwrap());
        assert_ne!(blake2_256(&other), h256(&fixture()["hash"]).unwrap());
    }

    // -------------------------------------------------------------------------- either, routing

    fn unavailable_err(s: &str) -> ChainError {
        ChainError::Unavailable(s.into())
    }

    fn invalid_err(s: &str) -> ChainError {
        ChainError::Invalid(s.into())
    }

    #[tokio::test]
    async fn issue_49_either_falls_back_and_says_why_both_failed() {
        let ok = |n: u8| async move { Ok::<u8, ChainError>(n) };
        let no = |e: ChainError| async move { Err::<u8, ChainError>(e) };
        assert_eq!(either(ok(1), no(invalid_err("never asked"))).await, Ok(1));
        assert_eq!(either(no(unavailable_err("down")), ok(2)).await, Ok(2));
        // A lie on the first side is not the end: the other side may answer.
        assert_eq!(either(no(invalid_err("lied")), ok(3)).await, Ok(3));
        assert_eq!(
            either(no(unavailable_err("a")), no(unavailable_err("b"))).await,
            Err(unavailable_err("a; then b"))
        );
        // Both unknown is unknown; a lie on either side makes the failure `Invalid`.
        assert_eq!(
            either(no(invalid_err("a")), no(unavailable_err("b"))).await,
            Err(invalid_err("a; then b"))
        );
        assert_eq!(
            either(no(unavailable_err("a")), no(invalid_err("b"))).await,
            Err(invalid_err("a; then b"))
        );
    }

    #[test]
    fn issue_49_a_block_is_old_when_more_than_depth_behind_the_finalized_head() {
        assert!(!is_old(1_000, 1_000, 128));
        assert!(!is_old(1_000, 872, 128), "exactly depth behind is not old");
        assert!(is_old(1_000, 871, 128), "depth + 1 is");
        assert!(is_old(1_000, 0, 128));
        // Above the finalized head (the head moved on while a block was being read) is not old.
        assert!(!is_old(1_000, 1_001, 128));
        assert!(!is_old(0, 5, 128));
    }

    #[tokio::test]
    async fn issue_49_old_reads_ask_the_archive_first_and_recent_ones_the_light_client() {
        let asked = Mutex::new(Vec::<&'static str>::new());
        let probe = |name: &'static str, result: ChainResult<u8>| {
            let asked = &asked;
            async move {
                asked.lock().unwrap().push(name);
                result
            }
        };
        let take = || std::mem::take(&mut *asked.lock().unwrap());

        // The first side answers: the other is never asked.
        assert_eq!(
            routed(true, probe("archive", Ok(1)), probe("light", Ok(2))).await,
            Ok(1)
        );
        assert_eq!(take(), ["archive"]);
        assert_eq!(
            routed(false, probe("archive", Ok(1)), probe("light", Ok(2))).await,
            Ok(2)
        );
        assert_eq!(take(), ["light"]);

        // The first side fails: the other answers.
        let down = || Err(unavailable_err("down"));
        assert_eq!(
            routed(true, probe("archive", down()), probe("light", Ok(2))).await,
            Ok(2)
        );
        assert_eq!(take(), ["archive", "light"]);
        assert_eq!(
            routed(false, probe("archive", Ok(1)), probe("light", down())).await,
            Ok(1)
        );
        assert_eq!(take(), ["light", "archive"]);

        // Neither answers: unknown, with both reasons, the first side's first.
        let got = routed(
            true,
            probe("archive", Err(unavailable_err("archive down"))),
            probe("light", Err(unavailable_err("light down"))),
        )
        .await;
        assert_eq!(got, Err(unavailable_err("archive down; then light down")));
    }

    // -------------------------------------------------------------------------- WsSource

    /// What the mock archive answers to one request.
    enum Reply {
        Result(Value),
        Error(String),
        /// Text that is not JSON.
        Garbage,
        /// Close the connection without answering.
        Hang,
    }

    type Script = Arc<dyn Fn(&str, &Value) -> Reply + Send + Sync>;

    struct Mock {
        port: u16,
        connections: Arc<AtomicUsize>,
        tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    }

    impl Mock {
        async fn start(script: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> Mock {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            Mock::serve(listener, port, Arc::new(script))
        }

        /// On `port` again, after a mock there was stopped.
        async fn restart(
            port: u16,
            script: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static,
        ) -> Mock {
            let listener = TcpListener::bind(("127.0.0.1", port)).await.unwrap();
            Mock::serve(listener, port, Arc::new(script))
        }

        fn serve(listener: TcpListener, port: u16, script: Script) -> Mock {
            let connections = Arc::new(AtomicUsize::new(0));
            let counted = connections.clone();
            let tasks = Arc::new(Mutex::new(Vec::new()));
            let spawned = tasks.clone();
            let accepting = tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        break;
                    };
                    counted.fetch_add(1, Ordering::SeqCst);
                    let serving = tokio::spawn(Mock::connection(socket, script.clone()));
                    spawned.lock().unwrap().push(serving);
                }
            });
            tasks.lock().unwrap().push(accepting);
            Mock {
                port,
                connections,
                tasks,
            }
        }

        async fn connection(socket: TcpStream, script: Script) {
            let mut server = Server::new(socket.compat());
            let Ok(request) = server.receive_request().await else {
                return;
            };
            let key = request.key();
            if server
                .send_response(&Response::Accept {
                    key,
                    protocol: None,
                })
                .await
                .is_err()
            {
                return;
            }
            let (mut sender, mut receiver) = server.into_builder().finish();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                if receiver.receive_data(&mut buf).await.is_err() {
                    return;
                }
                let Ok(request) = serde_json::from_slice::<Value>(&buf) else {
                    return;
                };
                let method = request["method"].as_str().unwrap_or_default().to_owned();
                let text = match script(&method, &request["params"]) {
                    Reply::Result(v) => {
                        json!({"jsonrpc": "2.0", "id": request["id"], "result": v}).to_string()
                    }
                    Reply::Error(message) => json!({"jsonrpc": "2.0", "id": request["id"],
                        "error": {"code": -32000, "message": message}})
                    .to_string(),
                    Reply::Garbage => "this is not json".to_owned(),
                    Reply::Hang => return,
                };
                if sender.send_text(text).await.is_err() || sender.flush().await.is_err() {
                    return;
                }
            }
        }

        fn url(&self, path: &str) -> String {
            format!("ws://127.0.0.1:{}{path}", self.port)
        }

        /// Stops listening and drops every connection it has not already.
        fn stop(self) {
            for task in self.tasks.lock().unwrap().iter() {
                task.abort();
            }
        }
    }

    fn source(url: &str) -> WsSource {
        WsSource::new(url, Duration::from_secs(5), 16 * 1024 * 1024)
    }

    #[tokio::test]
    async fn issue_49_a_source_reads_headers_proofs_and_bodies_over_one_connection() {
        let f = fixture();
        let hash = h256(&f["hash"]).unwrap();
        let header_json = f["header_json"].clone();
        let mock = Mock::start(move |method, params| match method {
            "chain_getBlockHash" if params[0] == json!(39_867_043) => {
                Reply::Result(json!(hash0x(&hash)))
            }
            "chain_getBlockHash" => Reply::Result(Value::Null),
            "chain_getHeader" => Reply::Result(header_json.clone()),
            "state_getReadProof" => {
                Reply::Result(json!({"at": "0x00", "proof": ["0x0102", "0x03"]}))
            }
            "chain_getBlock" => Reply::Result(json!({"block": {"extrinsics": ["0xaa", "0xbbcc"]}})),
            _ => Reply::Error("method not found".into()),
        })
        .await;
        let s = source(&mock.url(""));
        assert_eq!(s.name(), format!("ws://127.0.0.1:{}", mock.port));
        assert_eq!(s.block_hash(39_867_043).await, Ok(Some(hash)));
        assert_eq!(s.block_hash(1).await, Ok(None));
        let header = s.header(&hash).await.unwrap();
        assert_eq!(hex0x(&header), f["header"].as_str().unwrap());
        assert_eq!(
            s.read_proof(&hash, &[vec![1]]).await,
            Ok(vec![vec![1, 2], vec![3]])
        );
        assert_eq!(s.body(&hash).await, Ok(vec![vec![0xaa], vec![0xbb, 0xcc]]));
        assert_eq!(
            mock.connections.load(Ordering::SeqCst),
            1,
            "one connection for all of it"
        );
        mock.stop();
    }

    fn hash0x(h: &[u8; 32]) -> String {
        hex0x(h)
    }

    #[tokio::test]
    async fn issue_49_a_source_that_answers_wrongly_lies_and_one_that_refuses_is_missing() {
        let mock = Mock::start(|method, _| match method {
            "chain_getBlockHash" => Reply::Garbage,
            "chain_getHeader" => Reply::Result(json!({"number": "0x1"})),
            "state_getReadProof" => Reply::Result(json!({"proof": "no"})),
            "chain_getBlock" => Reply::Error("State already discarded".into()),
            _ => Reply::Result(json!("not a hash")),
        })
        .await;
        let s = source(&mock.url(""));
        // Not even a JSON-RPC message: the connection is not trusted to have answered.
        assert!(matches!(s.block_hash(1).await, Err(Fail::Unavailable(_))));
        assert!(matches!(s.header(&[1; 32]).await, Err(Fail::Lied(_))));
        assert!(matches!(
            s.read_proof(&[1; 32], &[]).await,
            Err(Fail::Lied(_))
        ));
        // A refusal from the node is "it does not have it", not a lie.
        match s.body(&[1; 32]).await {
            Err(Fail::Missing(m)) => assert!(m.contains("State already discarded"), "{m}"),
            other => panic!("{other:?}"),
        }
        mock.stop();
    }

    #[tokio::test]
    async fn issue_49_a_source_connects_again_after_the_node_drops_or_restarts() {
        let answer = |method: &str, _: &Value| match method {
            "chain_getBlockHash" => Reply::Result(json!(hex0x(&[5; 32]))),
            _ => Reply::Error("no".into()),
        };
        // A node that drops the connection on its first request, then behaves.
        let first = Arc::new(AtomicUsize::new(0));
        let seen = first.clone();
        let mock = Mock::start(move |m, p| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::Hang
            } else {
                answer(m, p)
            }
        })
        .await;
        let s = WsSource::new(&mock.url(""), Duration::from_millis(500), 1 << 20);
        let mut got = None;
        for _ in 0..40 {
            if let Ok(v) = s.block_hash(1).await {
                got = Some(v);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(got, Some(Some([5; 32])), "the source recovered");
        assert!(
            mock.connections.load(Ordering::SeqCst) >= 2,
            "it connected again"
        );

        // The node goes away altogether: the source is unreachable, with a reason that says
        // nothing of the address, and it works once the node is back on the same port.
        let port = mock.port;
        mock.stop();
        let mut down = None;
        for _ in 0..40 {
            match s.block_hash(1).await {
                Ok(_) => tokio::time::sleep(Duration::from_millis(25)).await,
                Err(e) => {
                    down = Some(e);
                    break;
                }
            }
        }
        assert!(matches!(down, Some(Fail::Unavailable(_))), "{down:?}");
        let back = Mock::restart(port, answer).await;
        let mut got = None;
        for _ in 0..80 {
            if let Ok(v) = s.block_hash(1).await {
                got = Some(v);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            got,
            Some(Some([5; 32])),
            "the source reconnected with backoff"
        );
        back.stop();
    }

    #[tokio::test]
    async fn issue_49_a_source_never_reached_is_unavailable_without_its_address() {
        // A port nothing listens on.
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        let s = WsSource::new(
            &format!("ws://user:SECRETKEY@127.0.0.1:{port}/v2/SECRETKEY?k=SECRETKEY"),
            Duration::from_millis(300),
            1 << 20,
        );
        let started = std::time::Instant::now();
        let err = s.block_hash(1).await.unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "bounded by the timeout"
        );
        let Fail::Unavailable(text) = err else {
            panic!("{err:?}");
        };
        assert!(
            !text.contains("SECRETKEY") && !text.contains("127.0.0.1"),
            "{text}"
        );
        assert_eq!(s.name(), format!("ws://127.0.0.1:{port}"));
    }

    #[tokio::test]
    async fn issue_49_what_a_server_says_is_neither_unbounded_nor_carries_the_key() {
        let mock = Mock::start(|_, _| {
            // An error that echoes the request URL, then goes on for a long time.
            Reply::Error(format!(
                "bad request to ws://127.0.0.1:1/v2/SECRETKEY?apikey=SECRETKEY {}",
                "x".repeat(10_000)
            ))
        })
        .await;
        let s = source(&mock.url("/v2/SECRETKEY"));
        // The mock cannot know its own address in the message above, so the source is told to
        // scrub the text it will be asked about: the URL's path is a secret too.
        let Err(Fail::Missing(text)) = s.block_hash(1).await else {
            panic!("a refusal is `Missing`");
        };
        assert!(text.len() < 260, "{} bytes", text.len());
        assert!(!text.contains("SECRETKEY"), "{text}");
        // The scrubbing, directly: the whole URL, its path, its query and its userinfo.
        let s = source("wss://user:PASS@node.example:9944/v2/PATHKEY?apikey=QUERYKEY");
        for secret in [
            "wss://user:PASS@node.example:9944/v2/PATHKEY?apikey=QUERYKEY",
            "user:PASS@node.example:9944/v2/PATHKEY?apikey=QUERYKEY",
            "v2/PATHKEY?apikey=QUERYKEY",
            "user:PASS",
        ] {
            let out = s.scrub(&format!("connect to {secret} failed\nbadly"));
            assert!(
                !out.contains("PASS") && !out.contains("PATHKEY") && !out.contains("QUERYKEY"),
                "{out}"
            );
            assert!(!out.contains('\n'));
        }
        mock.stop();
    }

    #[tokio::test]
    async fn issue_49_an_answer_over_the_cap_is_a_failed_request_not_a_big_allocation() {
        let mock =
            Mock::start(|_, _| Reply::Result(json!("0x".to_owned() + &"ab".repeat(20_000)))).await;
        // Cap: 4 KiB; the answer is 40 KB.
        let capped = WsSource::new(&mock.url(""), Duration::from_secs(2), 4 * 1024);
        let got = capped.header(&[1; 32]).await;
        assert!(
            matches!(got, Err(Fail::Unavailable(_) | Fail::Missing(_))),
            "{got:?}"
        );
        // The same answer is read under a larger cap (it then fails as a header, not as a size).
        let roomy = WsSource::new(&mock.url(""), Duration::from_secs(2), 1 << 20);
        assert!(matches!(roomy.header(&[1; 32]).await, Err(Fail::Lied(_))));
        mock.stop();
    }
}

#[cfg(test)]
mod warm_up_tests {
    use super::WarmUp;
    use smoldot_light::lifecycle_service::{Health, LifecycleState, Phase, StallReason};

    fn state(phase: Phase, health: Health) -> LifecycleState {
        LifecycleState {
            phase,
            num_peers: 3,
            health,
        }
    }

    #[test]
    fn a_warp_sync_that_advances_is_progress_and_one_that_stands_still_is_not() {
        let mut w = WarmUp::default();
        let syncing = |at| state(Phase::Syncing { at, target: 1000 }, Health::Ok);
        assert!(w.advanced(&syncing(10)));
        assert!(w.advanced(&syncing(20)));
        assert!(!w.advanced(&syncing(20)));
        assert!(!w.advanced(&syncing(15)));
        assert!(w.advanced(&syncing(21)));
    }

    #[test]
    fn a_stalled_chain_is_never_progress() {
        let mut w = WarmUp::default();
        let stalled = Health::Stalled {
            reason: StallReason::NoProgress,
        };
        assert!(!w.advanced(&state(Phase::Syncing { at: 5, target: 9 }, stalled)));
        assert!(!w.advanced(&state(Phase::Connecting, stalled)));
        assert!(!w.advanced(&state(Phase::Ready, stalled)));
    }

    #[test]
    fn nothing_counts_without_peers() {
        let mut w = WarmUp::default();
        let mut s = state(Phase::Ready, Health::Ok);
        s.num_peers = 0;
        assert!(!w.advanced(&s));
        s.num_peers = 2;
        assert!(w.advanced(&s));
    }

    #[test]
    fn connecting_and_ready_count_once() {
        let mut w = WarmUp::default();
        assert!(w.advanced(&state(Phase::Connecting, Health::Ok)));
        assert!(!w.advanced(&state(Phase::Connecting, Health::Ok)));
        assert!(w.advanced(&state(Phase::Ready, Health::Ok)));
        assert!(!w.advanced(&state(Phase::Ready, Health::Ok)));
    }
}

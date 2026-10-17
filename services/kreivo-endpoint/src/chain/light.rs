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

use std::collections::HashMap;
use std::num::NonZero;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use parity_scale_codec::Decode;
use scale_value::{Composite, Primitive, Value, ValueDef};
use serde::Deserialize;
use smoldot_light::platform::DefaultPlatform;
use subxt::backend::legacy::LegacyRpcMethods;
use subxt::backend::rpc::{RawRpcFuture, RawRpcSubscription, RawValue, RpcClient, RpcClientT};
use subxt::config::Header as _;
use subxt::ext::subxt_rpcs::{Error as RpcError, UserError};
use subxt::tx::SubmittableTransaction;
use subxt::utils::H256;
use subxt::{OnlineClient, PolkadotConfig};
use tokio::sync::{mpsc, oneshot};

use super::{
    BlockEvents, BlockRef, Chain, ChainError, ChainResult, Emitted, ExtrinsicResult, Header,
    RuntimeVersion,
};
use crate::kreivo::{AccountId32, blake2_256};

// ------------------------------------------------------------------------------ the light client

type Client = smoldot_light::Client<Arc<DefaultPlatform>, ()>;
type Notifications = mpsc::UnboundedSender<Result<Box<RawValue>, RpcError>>;

struct Pending {
    reply: oneshot::Sender<Result<Box<RawValue>, RpcError>>,
    notifications: Option<Notifications>,
}

struct Inner {
    client: Mutex<Client>,
    chain: smoldot_light::ChainId,
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

/// Kreivo over the light client. See the module documentation.
pub struct LightChain {
    client: OnlineClient<PolkadotConfig>,
    rpc: LegacyRpcMethods<PolkadotConfig>,
    /// Every transaction submitted and still watched, by hash: smoldot-light panics when one
    /// transaction has two status watchers and one closes (aledgent F-027 NOTES, decision 40), so
    /// the same bytes are never watched twice at once.
    watching: Mutex<HashMap<[u8; 32], tokio::task::JoinHandle<()>>>,
}

impl LightChain {
    /// Starts the light client on `chain_spec` (Kreivo), a parachain of `relay_chain_spec`.
    pub async fn start(
        chain_spec: &str,
        relay_chain_spec: Option<&str>,
    ) -> ChainResult<LightChain> {
        let rpc = LightRpc::new(chain_spec, relay_chain_spec).map_err(unavailable)?;
        let rpc_client = RpcClient::new(rpc);
        let client = OnlineClient::<PolkadotConfig>::from_rpc_client(rpc_client.clone())
            .await
            .map_err(unavailable)?;
        Ok(LightChain {
            client,
            rpc: LegacyRpcMethods::<PolkadotConfig>::new(rpc_client),
            watching: Mutex::default(),
        })
    }

    async fn fetch(
        &self,
        at: &BlockRef,
        pallet: &str,
        entry: &str,
        keys: Vec<Value<()>>,
    ) -> ChainResult<Option<Value<u32>>> {
        let thunk = self
            .client
            .storage()
            .at(H256(at.hash))
            .fetch(&subxt::dynamic::storage(pallet, entry, keys))
            .await
            .map_err(unavailable)?;
        thunk.map(|t| t.to_value().map_err(invalid)).transpose()
    }
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
        Ok(self.header(&hash.0).await?.block)
    }

    async fn header(&self, hash: &[u8; 32]) -> ChainResult<Header> {
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

    async fn events(&self, at: &BlockRef, contract: &AccountId32) -> ChainResult<BlockEvents> {
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

    async fn extrinsics(&self, at: &BlockRef) -> ChainResult<Vec<Vec<u8>>> {
        let block = self
            .rpc
            .chain_get_block(Some(H256(at.hash)))
            .await
            .map_err(unavailable)?
            .ok_or_else(|| unavailable(format!("no body for block {}", at.number)))?;
        Ok(block.block.extrinsics.into_iter().map(|b| b.0).collect())
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

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! One provider of an EVM network: what the endpoint reads and sends (PLAN `DEC-8`, `DEC-12`).
//!
//! [`Rpc`] is the provider interface; [`Http`] is a JSON-RPC provider over HTTP through alloy's
//! client; [`mock::MockRpc`] is an in-memory one for tests. Every read names the block it is made
//! at. An error is the provider's own text (`REQ-ST-3`) and is never a disposition: a failed read
//! is unknown (`REQ-NX-3`, `AC-C1.2`). Two providers are read together through
//! [`crate::pair::Pair`].

pub mod mock;

use std::fmt;

use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::rpc::client::{ClientBuilder, RpcClient};
use alloy::transports::{RpcError as TransportRpcError, TransportErrorKind};
use serde_json::{Value, json};

/// A block a read is made at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum At {
    /// The network's `finalized` tag: its own finality signal (`REQ-OB-1`, `REQ-RN-1`).
    Finalized,
    /// The `latest` tag (only for preparing a transaction: nonce, fees, the dry run).
    Latest,
    /// A block number.
    Number(u64),
}

impl At {
    fn param(self) -> Value {
        match self {
            Self::Finalized => json!("finalized"),
            Self::Latest => json!("latest"),
            Self::Number(n) => json!(format!("0x{n:x}")),
        }
    }
}

/// A block header, as far as the endpoint reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Its number.
    pub number: u64,
    /// Its hash.
    pub hash: B256,
    /// Its parent's hash.
    pub parent_hash: B256,
    /// Its time, in seconds since the Unix epoch.
    pub timestamp: u64,
    /// Its base fee, in wei.
    pub base_fee: Option<u128>,
}

/// An `eth_getLogs` query over blocks `from` to `to`, inclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogQuery {
    /// First block.
    pub from: u64,
    /// Last block.
    pub to: u64,
    /// The emitting contracts; empty for any.
    pub address: Vec<Address>,
    /// Topics by position; `None` for any.
    pub topics: Vec<Option<B256>>,
}

impl LogQuery {
    /// Whether `log` answers this query.
    pub fn matches(&self, log: &Log) -> bool {
        (self.from..=self.to).contains(&log.block_number)
            && (self.address.is_empty() || self.address.contains(&log.address))
            && self
                .topics
                .iter()
                .enumerate()
                .all(|(i, t)| t.is_none_or(|t| log.topics.get(i) == Some(&t)))
    }
}

/// A log, with its coordinate.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Log {
    /// The block number.
    pub block_number: u64,
    /// Its index in the block.
    pub log_index: u64,
    /// The block hash the provider reports it in.
    pub block_hash: B256,
    /// The transaction hash.
    pub tx_hash: B256,
    /// The transaction's index in the block.
    pub tx_index: u64,
    /// The emitting contract.
    pub address: Address,
    /// The topics.
    pub topics: Vec<B256>,
    /// The data.
    pub data: Bytes,
    /// Whether the provider marks it removed by a reorganisation.
    pub removed: bool,
}

/// A transaction in a block, as far as the endpoint reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tx {
    /// Its hash.
    pub hash: B256,
    /// Its index in the block.
    pub index: u64,
    /// Its EIP-2718 type.
    pub tx_type: u8,
    /// The sender.
    pub from: Address,
    /// The sender's nonce.
    pub nonce: u64,
    /// The recipient; `None` for a creation.
    pub to: Option<Address>,
    /// The value.
    pub value: U256,
    /// The input.
    pub input: Bytes,
    /// The chain id, if the transaction carries one.
    pub chain_id: Option<u64>,
    /// The gas limit.
    pub gas: u64,
    /// `maxFeePerGas` (type 2).
    pub max_fee_per_gas: Option<u128>,
    /// `maxPriorityFeePerGas` (type 2).
    pub max_priority_fee_per_gas: Option<u128>,
    /// Whether the access list is empty.
    pub access_list_empty: bool,
    /// The signature's `r ‖ s ‖ v`, `v` the parity (0 or 1).
    pub signature: Vec<u8>,
}

/// A transaction receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    /// The transaction hash.
    pub tx_hash: B256,
    /// The block number.
    pub block_number: u64,
    /// The block hash.
    pub block_hash: B256,
    /// The transaction's index in the block.
    pub tx_index: u64,
    /// Whether it executed (`REQ-OB-6`): status 1.
    pub status: bool,
}

/// A provider's failure: its own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError(pub String);

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RpcError {}

/// The outcome of `eth_call` or `eth_estimateGas`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// The call reverted: the provider's message and the revert data.
    Reverted {
        /// The message.
        message: String,
        /// The revert data.
        data: Option<Bytes>,
    },
    /// The provider failed.
    Rpc(RpcError),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reverted { message, data } => {
                f.write_str(&crate::evm::revert_reason(message, data.as_ref()))
            }
            Self::Rpc(e) => write!(f, "{e}"),
        }
    }
}

/// A call or gas estimate: from, to, data, no value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The sender.
    pub from: Address,
    /// The target.
    pub to: Address,
    /// The calldata.
    pub data: Bytes,
}

/// One provider of one network.
#[allow(async_fn_in_trait)]
pub trait Rpc {
    /// Its name, for logs and records (never its URL, which may carry a key).
    fn name(&self) -> &str;
    /// `eth_chainId`.
    async fn chain_id(&self) -> Result<u64, RpcError>;
    /// `eth_getBlockByNumber(at, false)`; `None` if the provider has no such block.
    async fn header(&self, at: At) -> Result<Option<Header>, RpcError>;
    /// `eth_getLogs`.
    async fn logs(&self, query: &LogQuery) -> Result<Vec<Log>, RpcError>;
    /// `eth_getTransactionCount(address, at)`.
    async fn nonce(&self, address: Address, at: At) -> Result<u64, RpcError>;
    /// `eth_getBalance(address, at)`.
    async fn balance(&self, address: Address, at: At) -> Result<U256, RpcError>;
    /// `eth_getCode(address, at)`.
    async fn code(&self, address: Address, at: At) -> Result<Bytes, RpcError>;
    /// `eth_getBlockByNumber(n, true)`: the header and every transaction.
    async fn block_transactions(&self, number: u64) -> Result<Option<(Header, Vec<Tx>)>, RpcError>;
    /// `eth_getTransactionReceipt`.
    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, RpcError>;
    /// `eth_call` at `at`.
    async fn call(&self, call: &Call, at: At) -> Result<Bytes, CallError>;
    /// `eth_estimateGas` at the latest block.
    async fn estimate_gas(&self, call: &Call) -> Result<u64, CallError>;
    /// `eth_maxPriorityFeePerGas`.
    async fn priority_fee(&self) -> Result<u128, RpcError>;
    /// `eth_sendRawTransaction`.
    async fn send_raw(&self, bytes: &[u8]) -> Result<B256, RpcError>;
}

/// A provider over HTTP, through alloy's JSON-RPC client.
pub struct Http {
    name: String,
    client: RpcClient,
}

impl fmt::Debug for Http {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The URL may carry a key: never printed.
        f.debug_struct("Http").field("name", &self.name).finish()
    }
}

impl Http {
    /// A provider named `name` at `url`.
    pub fn new(name: &str, url: &str) -> Result<Self, String> {
        let url = url::Url::parse(url).map_err(|e| format!("provider {name}: a bad URL: {e}"))?;
        Ok(Self {
            name: name.to_owned(),
            client: ClientBuilder::default().http(url),
        })
    }

    async fn request(&self, method: &'static str, params: Value) -> Result<Value, CallError> {
        self.client
            .request::<Value, Value>(method, params)
            .await
            .map_err(|e| match e {
                TransportRpcError::ErrorResp(p) => {
                    let data = p
                        .data
                        .as_ref()
                        .and_then(|d| serde_json::from_str::<Value>(d.get()).ok())
                        .and_then(|d| d.as_str().and_then(parse_bytes));
                    // A revert is code 3 with data, or a message that says so.
                    if p.code == 3 || p.message.contains("revert") {
                        CallError::Reverted {
                            message: p.message.to_string(),
                            data,
                        }
                    } else {
                        CallError::Rpc(RpcError(format!(
                            "{}: {method}: error {}: {}",
                            self.name, p.code, p.message
                        )))
                    }
                }
                other => CallError::Rpc(RpcError(format!(
                    "{}: {method}: {}",
                    self.name,
                    transport_text(&other)
                ))),
            })
    }

    async fn read(&self, method: &'static str, params: Value) -> Result<Value, RpcError> {
        self.request(method, params).await.map_err(|e| match e {
            CallError::Rpc(e) => e,
            CallError::Reverted { message, .. } => {
                RpcError(format!("{}: {method}: {message}", self.name))
            }
        })
    }

    fn bad(&self, method: &str, what: &str) -> RpcError {
        RpcError(format!("{}: {method}: the answer has no {what}", self.name))
    }
}

fn transport_text(e: &TransportRpcError<TransportErrorKind>) -> String {
    // The transport's text names the URL for some errors; keep only the kind.
    match e {
        TransportRpcError::Transport(kind) => match kind {
            TransportErrorKind::HttpError(h) => format!("HTTP {}", h.status),
            _ => "transport error".into(),
        },
        TransportRpcError::NullResp => "a null answer".into(),
        TransportRpcError::DeserError { err, .. } => {
            format!("an answer that does not decode: {err}")
        }
        other => other.to_string(),
    }
}

/// Parses a `0x` quantity.
pub fn parse_quantity(v: &Value) -> Option<u128> {
    u128::from_str_radix(v.as_str()?.strip_prefix("0x")?, 16).ok()
}

fn parse_u64(v: &Value) -> Option<u64> {
    parse_quantity(v).and_then(|n| u64::try_from(n).ok())
}

fn parse_b256(v: &Value) -> Option<B256> {
    v.as_str()?.parse().ok()
}

fn parse_address(v: &Value) -> Option<Address> {
    v.as_str()?.parse().ok()
}

fn parse_bytes(s: &str) -> Option<Bytes> {
    hex::decode(s.strip_prefix("0x")?).ok().map(Bytes::from)
}

fn parse_u256(v: &Value) -> Option<U256> {
    let digits = v.as_str()?.strip_prefix("0x")?;
    U256::from_str_radix(digits, 16).ok()
}

/// A header from a block object.
pub fn parse_header(v: &Value) -> Option<Header> {
    Some(Header {
        number: parse_u64(&v["number"])?,
        hash: parse_b256(&v["hash"])?,
        parent_hash: parse_b256(&v["parentHash"])?,
        timestamp: parse_u64(&v["timestamp"])?,
        base_fee: parse_quantity(&v["baseFeePerGas"]),
    })
}

/// A log from a log object.
pub fn parse_log(v: &Value) -> Option<Log> {
    Some(Log {
        block_number: parse_u64(&v["blockNumber"])?,
        log_index: parse_u64(&v["logIndex"])?,
        block_hash: parse_b256(&v["blockHash"])?,
        tx_hash: parse_b256(&v["transactionHash"])?,
        tx_index: parse_u64(&v["transactionIndex"])?,
        address: parse_address(&v["address"])?,
        topics: v["topics"]
            .as_array()?
            .iter()
            .map(parse_b256)
            .collect::<Option<_>>()?,
        data: parse_bytes(v["data"].as_str()?)?,
        removed: v["removed"].as_bool().unwrap_or(false),
    })
}

/// A transaction from a transaction object.
pub fn parse_tx(v: &Value) -> Option<Tx> {
    let tx_type = parse_u64(&v["type"]).unwrap_or(0);
    let parity = match (parse_quantity(&v["yParity"]), parse_quantity(&v["v"])) {
        (Some(p), _) => p,
        (None, Some(v)) if v == 27 || v == 28 => v - 27,
        (None, Some(v)) if v >= 35 => (v - 35) % 2,
        (None, Some(v)) => v,
        _ => return None,
    };
    let mut signature = Vec::with_capacity(65);
    signature.extend_from_slice(&parse_u256(&v["r"])?.to_be_bytes::<32>());
    signature.extend_from_slice(&parse_u256(&v["s"])?.to_be_bytes::<32>());
    signature.push(u8::try_from(parity).ok()?);
    Some(Tx {
        hash: parse_b256(&v["hash"])?,
        index: parse_u64(&v["transactionIndex"])?,
        tx_type: u8::try_from(tx_type).ok()?,
        from: parse_address(&v["from"])?,
        nonce: parse_u64(&v["nonce"])?,
        to: parse_address(&v["to"]),
        value: parse_u256(&v["value"])?,
        input: parse_bytes(v["input"].as_str()?)?,
        chain_id: parse_u64(&v["chainId"]),
        gas: parse_u64(&v["gas"])?,
        max_fee_per_gas: parse_quantity(&v["maxFeePerGas"]),
        max_priority_fee_per_gas: parse_quantity(&v["maxPriorityFeePerGas"]),
        access_list_empty: v["accessList"].as_array().is_none_or(Vec::is_empty),
        signature,
    })
}

/// A receipt from a receipt object.
pub fn parse_receipt(v: &Value) -> Option<Receipt> {
    Some(Receipt {
        tx_hash: parse_b256(&v["transactionHash"])?,
        block_number: parse_u64(&v["blockNumber"])?,
        block_hash: parse_b256(&v["blockHash"])?,
        tx_index: parse_u64(&v["transactionIndex"])?,
        status: parse_u64(&v["status"])? == 1,
    })
}

fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

fn call_param(c: &Call) -> Value {
    json!({"from": c.from, "to": c.to, "data": hex0x(&c.data)})
}

impl Rpc for Http {
    fn name(&self) -> &str {
        &self.name
    }

    async fn chain_id(&self) -> Result<u64, RpcError> {
        let v = self.read("eth_chainId", json!([])).await?;
        parse_u64(&v).ok_or_else(|| self.bad("eth_chainId", "chain id"))
    }

    async fn header(&self, at: At) -> Result<Option<Header>, RpcError> {
        let v = self
            .read("eth_getBlockByNumber", json!([at.param(), false]))
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        parse_header(&v)
            .map(Some)
            .ok_or_else(|| self.bad("eth_getBlockByNumber", "header"))
    }

    async fn logs(&self, q: &LogQuery) -> Result<Vec<Log>, RpcError> {
        let topics: Vec<Value> = q
            .topics
            .iter()
            .map(|t| t.map_or(Value::Null, |t| json!(t)))
            .collect();
        let mut filter = json!({
            "fromBlock": At::Number(q.from).param(),
            "toBlock": At::Number(q.to).param(),
            "topics": topics,
        });
        if !q.address.is_empty() {
            filter["address"] = json!(q.address);
        }
        let v = self.read("eth_getLogs", json!([filter])).await?;
        v.as_array()
            .ok_or_else(|| self.bad("eth_getLogs", "list"))?
            .iter()
            .map(|l| parse_log(l).ok_or_else(|| self.bad("eth_getLogs", "well-formed log")))
            .collect()
    }

    async fn nonce(&self, address: Address, at: At) -> Result<u64, RpcError> {
        let v = self
            .read("eth_getTransactionCount", json!([address, at.param()]))
            .await?;
        parse_u64(&v).ok_or_else(|| self.bad("eth_getTransactionCount", "count"))
    }

    async fn balance(&self, address: Address, at: At) -> Result<U256, RpcError> {
        let v = self
            .read("eth_getBalance", json!([address, at.param()]))
            .await?;
        parse_u256(&v).ok_or_else(|| self.bad("eth_getBalance", "balance"))
    }

    async fn code(&self, address: Address, at: At) -> Result<Bytes, RpcError> {
        let v = self
            .read("eth_getCode", json!([address, at.param()]))
            .await?;
        v.as_str()
            .and_then(parse_bytes)
            .ok_or_else(|| self.bad("eth_getCode", "bytes"))
    }

    async fn block_transactions(&self, number: u64) -> Result<Option<(Header, Vec<Tx>)>, RpcError> {
        let v = self
            .read(
                "eth_getBlockByNumber",
                json!([At::Number(number).param(), true]),
            )
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        let header = parse_header(&v).ok_or_else(|| self.bad("eth_getBlockByNumber", "header"))?;
        let txs = v["transactions"]
            .as_array()
            .ok_or_else(|| self.bad("eth_getBlockByNumber", "transactions"))?
            .iter()
            .map(|t| {
                parse_tx(t)
                    .ok_or_else(|| self.bad("eth_getBlockByNumber", "well-formed transaction"))
            })
            .collect::<Result<_, _>>()?;
        Ok(Some((header, txs)))
    }

    async fn receipt(&self, hash: B256) -> Result<Option<Receipt>, RpcError> {
        let v = self
            .read("eth_getTransactionReceipt", json!([hash]))
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        parse_receipt(&v)
            .map(Some)
            .ok_or_else(|| self.bad("eth_getTransactionReceipt", "receipt"))
    }

    async fn call(&self, call: &Call, at: At) -> Result<Bytes, CallError> {
        let v = self
            .request("eth_call", json!([call_param(call), at.param()]))
            .await?;
        v.as_str()
            .and_then(parse_bytes)
            .ok_or_else(|| CallError::Rpc(self.bad("eth_call", "bytes")))
    }

    async fn estimate_gas(&self, call: &Call) -> Result<u64, CallError> {
        let v = self
            .request("eth_estimateGas", json!([call_param(call), "latest"]))
            .await?;
        parse_u64(&v).ok_or_else(|| CallError::Rpc(self.bad("eth_estimateGas", "gas")))
    }

    async fn priority_fee(&self) -> Result<u128, RpcError> {
        let v = self.read("eth_maxPriorityFeePerGas", json!([])).await?;
        parse_quantity(&v).ok_or_else(|| self.bad("eth_maxPriorityFeePerGas", "fee"))
    }

    async fn send_raw(&self, bytes: &[u8]) -> Result<B256, RpcError> {
        let v = self
            .read("eth_sendRawTransaction", json!([hex0x(bytes)]))
            .await?;
        parse_b256(&v).ok_or_else(|| self.bad("eth_sendRawTransaction", "hash"))
    }
}

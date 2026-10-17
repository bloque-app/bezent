// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The network's identity, checked at start-up (`T-007-01`; `AC-I4.1`, `REQ-IC-1`; PLAN `DEC-5`).
//!
//! A network is identified by its genesis hash. The endpoint starts only when three agree: the
//! genesis the configuration declares, the genesis of the pinned chain spec the light client is
//! given, and the genesis of the chain the client actually follows. Any disagreement, or a chain
//! that cannot say, stops the start: the endpoint fails closed and reads nothing, builds nothing
//! and submits nothing. For a parachain, the relay chain is pinned the same way (review S5): its
//! spec's genesis must be the declared relay genesis, and its `id` the chain spec's `relay_chain`.
//! The relay spec's checkpoint, which smoldot warp-syncs from and whose finality it trusts, is
//! pinned too (review S-c), and so is the parachain id the chain spec names (review S-d).
//!
//! The pinned chain spec is a light chain spec (`genesis.stateRootHash`), as aledgent pins
//! Kreivo's: its genesis block is the header with no parent, number 0, that state root, the empty
//! trie's root as extrinsics root, and no digest. A raw spec (`genesis.raw`) is refused rather than
//! having its trie root computed here.

use std::fmt;

use crate::chain::Chain;
use crate::kreivo::blake2_256;

/// Why the endpoint refuses to start on a network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// The chain spec cannot be read, or is not a light spec.
    ChainSpec(String),
    /// The pinned chain spec describes another network than the declared one.
    SpecMismatch {
        /// The declared genesis.
        declared: [u8; 32],
        /// The chain spec's.
        spec: [u8; 32],
    },
    /// The relay chain spec describes another relay chain than the declared one.
    RelayMismatch {
        /// The declared relay genesis.
        declared: [u8; 32],
        /// The relay chain spec's.
        spec: [u8; 32],
    },
    /// The relay chain spec's checkpoint is not the declared one, or one of them is missing.
    CheckpointMismatch {
        /// The declared checkpoint hash.
        declared: Option<[u8; 32]>,
        /// The relay chain spec's.
        spec: Option<[u8; 32]>,
    },
    /// The chain spec names another parachain id than the declared one, or none.
    ParaMismatch {
        /// The declared id.
        declared: Option<u32>,
        /// The chain spec's.
        spec: Option<u64>,
    },
    /// The chain the client follows is another network than the declared one.
    ChainMismatch {
        /// The declared genesis.
        declared: [u8; 32],
        /// The chain's.
        chain: [u8; 32],
    },
    /// The chain did not say what it is: unknown, so the start stops (`REQ-NX-3`).
    Unknown(String),
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityError::ChainSpec(e) => write!(f, "AC-I4.1: the chain spec is unusable: {e}"),
            IdentityError::SpecMismatch { declared, spec } => write!(
                f,
                "AC-I4.1: the chain spec's genesis 0x{} is not the declared network's 0x{}; refusing to start",
                hex::encode(spec),
                hex::encode(declared)
            ),
            IdentityError::RelayMismatch { declared, spec } => write!(
                f,
                "AC-I4.1: the relay chain spec's genesis 0x{} is not the declared relay chain's 0x{}; refusing to start",
                hex::encode(spec),
                hex::encode(declared)
            ),
            IdentityError::CheckpointMismatch { declared, spec } => {
                let h = |x: &Option<[u8; 32]>| {
                    x.map_or_else(|| "none".to_owned(), |b| format!("0x{}", hex::encode(b)))
                };
                write!(
                    f,
                    "AC-I4.1: the relay chain spec's checkpoint {} is not the declared relay_checkpoint {}; refusing to start",
                    h(spec),
                    h(declared)
                )
            }
            IdentityError::ParaMismatch { declared, spec } => write!(
                f,
                "AC-I4.1: the chain spec's para_id {spec:?} is not the declared para_id {declared:?}; refusing to start"
            ),
            IdentityError::ChainMismatch { declared, chain } => write!(
                f,
                "AC-I4.1: the chain's genesis 0x{} is not the declared network's 0x{}; refusing to start",
                hex::encode(chain),
                hex::encode(declared)
            ),
            IdentityError::Unknown(e) => write!(
                f,
                "AC-I4.1: the chain's identity is unknown ({e}); refusing to start"
            ),
        }
    }
}

impl std::error::Error for IdentityError {}

/// The genesis hash of a light chain spec's JSON text.
pub fn chain_spec_genesis(spec: &str) -> Result<[u8; 32], IdentityError> {
    let value: serde_json::Value =
        serde_json::from_str(spec).map_err(|e| IdentityError::ChainSpec(e.to_string()))?;
    let root = value
        .pointer("/genesis/stateRootHash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            IdentityError::ChainSpec("not a light chain spec: no genesis.stateRootHash".into())
        })?;
    let root: [u8; 32] = crate::config::hex_bytes(root, "genesis.stateRootHash")
        .map_err(IdentityError::ChainSpec)?;
    let mut header = vec![0u8; 32]; // parent hash
    header.push(0); // compact(0): the number
    header.extend_from_slice(&root);
    header.extend_from_slice(&blake2_256(&[0])); // the empty trie's root
    header.push(0); // no digest items
    Ok(blake2_256(&header))
}

/// The offline half: the pinned chain spec describes the declared network.
pub fn check_spec(declared: [u8; 32], spec: &str) -> Result<(), IdentityError> {
    let genesis = chain_spec_genesis(spec)?;
    if genesis != declared {
        return Err(IdentityError::SpecMismatch {
            declared,
            spec: genesis,
        });
    }
    Ok(())
}

/// What the configuration pins of a parachain's relay chain (`AC-I4.1`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RelayPin {
    /// The relay chain's genesis hash.
    pub genesis: Option<[u8; 32]>,
    /// The BLAKE2b-256 of the relay chain spec's checkpoint ([`relay_checkpoint`]), if it has one.
    pub checkpoint: Option<[u8; 32]>,
    /// The parachain id the chain spec must name.
    pub para_id: Option<u32>,
}

/// JSON with object keys sorted and no whitespace: the same text for the same value, whatever
/// order a file writes its keys in.
fn canonical(v: &serde_json::Value, out: &mut String) {
    match v {
        serde_json::Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String(k.clone()).to_string());
                out.push(':');
                canonical(&m[k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(x, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The relay chain spec's checkpoint, as the configuration pins it (review S-c): the BLAKE2b-256
/// of the canonical JSON of an object holding the spec's `lightSyncState` and `checkpoint`
/// fields, whichever it has. `None` when it has neither: smoldot then syncs from the genesis,
/// which is pinned on its own.
pub fn relay_checkpoint(relay_spec: &str) -> Result<Option<[u8; 32]>, IdentityError> {
    let value: serde_json::Value =
        serde_json::from_str(relay_spec).map_err(|e| IdentityError::ChainSpec(e.to_string()))?;
    let mut fields = serde_json::Map::new();
    for key in ["checkpoint", "lightSyncState"] {
        if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
            fields.insert(key.to_owned(), v.clone());
        }
    }
    if fields.is_empty() {
        return Ok(None);
    }
    let mut text = String::new();
    canonical(&serde_json::Value::Object(fields), &mut text);
    Ok(Some(blake2_256(text.as_bytes())))
}

/// The relay chain, for a parachain (review S5, S-c, S-d): the relay chain's identity is pinned
/// with the network's. A chain spec that names a relay chain needs a relay chain spec, whose
/// genesis must be the declared relay genesis, whose `id` must be the one the chain spec names, and
/// whose checkpoint must be the declared one; and the chain spec's `para_id` must be the declared
/// one. smoldot verifies the parachain's finality through the relay chain it is given, from the
/// relay spec's checkpoint, and follows the heads of the chain spec's `para_id`: a relay spec of
/// another network, a forged checkpoint or another `para_id` would make it follow another
/// network's finality.
pub fn check_relay(
    pin: &RelayPin,
    spec: &str,
    relay_spec: Option<&str>,
) -> Result<(), IdentityError> {
    let value: serde_json::Value =
        serde_json::from_str(spec).map_err(|e| IdentityError::ChainSpec(e.to_string()))?;
    let relay_chain = value.get("relay_chain").and_then(|v| v.as_str());
    let (declared, relay_spec) = match (relay_chain, pin.genesis, relay_spec) {
        (None, None, None) if pin.para_id.is_none() && pin.checkpoint.is_none() => return Ok(()),
        (_, Some(d), Some(r)) => (d, r),
        _ => {
            return Err(IdentityError::ChainSpec(format!(
                "the chain spec names relay chain {relay_chain:?}; a relay chain spec and its declared genesis are both required with it"
            )));
        }
    };
    let genesis = chain_spec_genesis(relay_spec)?;
    if genesis != declared {
        return Err(IdentityError::RelayMismatch {
            declared,
            spec: genesis,
        });
    }
    let relay: serde_json::Value =
        serde_json::from_str(relay_spec).map_err(|e| IdentityError::ChainSpec(e.to_string()))?;
    let id = relay.get("id").and_then(|v| v.as_str());
    if relay_chain.is_none() || id != relay_chain {
        return Err(IdentityError::ChainSpec(format!(
            "the relay chain spec is {id:?}, the chain spec's relay chain is {relay_chain:?}"
        )));
    }
    let para_id = value
        .get("para_id")
        .or_else(|| value.get("paraId"))
        .and_then(|v| v.as_u64());
    if pin.para_id.is_none() || para_id != pin.para_id.map(u64::from) {
        return Err(IdentityError::ParaMismatch {
            declared: pin.para_id,
            spec: para_id,
        });
    }
    let checkpoint = relay_checkpoint(relay_spec)?;
    if checkpoint != pin.checkpoint {
        return Err(IdentityError::CheckpointMismatch {
            declared: pin.checkpoint,
            spec: checkpoint,
        });
    }
    Ok(())
}

/// The online half: the chain the client follows is the declared network.
pub async fn check_chain<C: Chain>(chain: &C, declared: [u8; 32]) -> Result<(), IdentityError> {
    let genesis = chain
        .genesis()
        .await
        .map_err(|e| IdentityError::Unknown(e.to_string()))?;
    if genesis != declared {
        return Err(IdentityError::ChainMismatch {
            declared,
            chain: genesis,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Kreivo's genesis (the light spec aledgent pins, `chain-specs/kreivo.json` here).
    const KREIVO: &str = "0xc710a5f16adc17bcd212cff0aedcbf1c1212a043cdc0fb2dcba861efe5305b01";

    #[test]
    fn ac_i4_1_the_pinned_chain_spec_is_kreivo() {
        let spec = include_str!("../chain-specs/kreivo.json");
        let declared = crate::config::hex_bytes(KREIVO, "genesis").unwrap();
        assert_eq!(chain_spec_genesis(spec).unwrap(), declared);
        assert_eq!(check_spec(declared, spec), Ok(()));
    }

    #[test]
    fn ac_i4_1_a_spec_of_another_network_is_refused() {
        let spec = include_str!("../chain-specs/kreivo.json");
        let err = check_spec([7; 32], spec).unwrap_err();
        assert!(matches!(err, IdentityError::SpecMismatch { .. }), "{err}");
        assert!(err.to_string().contains("refusing to start"));
        let raw = r#"{"genesis": {"raw": {"top": {}}}}"#;
        assert!(matches!(
            check_spec([7; 32], raw),
            Err(IdentityError::ChainSpec(_))
        ));
    }
}

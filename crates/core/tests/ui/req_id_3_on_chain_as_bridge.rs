// SPDX-License-Identifier: Apache-2.0 OR MIT
// REQ-ID-3: an on-chain reference cannot be passed where a bridge reference is expected.
use bezent_core::{BridgeReference, OnChainReference};

fn wants_bridge_reference(_: BridgeReference) {}

fn main() {
    let on_chain = OnChainReference::new(vec![1u8]).unwrap();
    wants_bridge_reference(on_chain);
}

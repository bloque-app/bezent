// SPDX-License-Identifier: Apache-2.0 OR MIT
// REQ-ID-3: a request reference cannot be passed where a bridge reference is expected.
use bezent_core::{BridgeReference, RequestReference};

fn wants_bridge_reference(_: &BridgeReference) {}

fn main() {
    let request = RequestReference::new("req-1").unwrap();
    wants_bridge_reference(&request);
}

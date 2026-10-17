// SPDX-License-Identifier: Apache-2.0 OR MIT
// REQ-ID-3: no conversion turns a request reference into a bridge reference.
use bezent_core::{BridgeReference, RequestReference};

fn main() {
    let request = RequestReference::new("req-1").unwrap();
    let _bridge: BridgeReference = request.into();
}

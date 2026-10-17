// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Compile-fail proofs that the reference kinds never stand in for each other (`REQ-ID-3`) and
//! that an amount has no floating-point conversion (`INV-11`).

#[test]
fn req_id_3_reference_kinds_never_convert() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/req_id_3_*.rs");
}

#[test]
fn inv_11_amount_has_no_float_conversion() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/inv_11_*.rs");
}

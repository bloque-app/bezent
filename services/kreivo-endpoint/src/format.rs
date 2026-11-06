// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether a Kreivo runtime takes the transaction format the endpoint writes (issue #104): the
//! fingerprint, checked against the runtime's own metadata before anything is built or signed.
//!
//! Submissions are not pinned to a `spec_version`, since Kreivo upgrades and most upgrades leave the
//! transaction format alone. They are pinned to a `transaction_version` in
//! [`TRANSACTION_VERSIONS`](crate::kreivo::TRANSACTION_VERSIONS). That number is a convention a
//! runtime author keeps, not something the chain enforces, so the runtime's metadata (V15, from
//! the light client's proven runtime call at the block) must also match, item by item, what
//! [`kreivo`](crate::kreivo) writes and reads by position:
//!
//! | Item | What must hold |
//! |---|---|
//! | `extrinsic.version` | 4 |
//! | `extrinsic.address` | variant 0 `Id`, a `[u8; 32]` |
//! | `extrinsic.signature` | variants 0 `Ed25519` `[u8; 64]`, 1 `Sr25519` `[u8; 64]`, 2 `Ecdsa` `[u8; 65]` |
//! | `extrinsic.extensions` | exactly [`EXTENSIONS`], in that order |
//! | `extension.<name>` | each one's explicit and implicit data as [`kreivo::extra`](crate::kreivo::extra) and [`kreivo::implicit`](crate::kreivo::implicit) write them |
//! | `call.Contracts.call` | pallet `Contracts` at 80, call 6 `call`, its five fields as [`kreivo::contracts_call`](crate::kreivo::contracts_call) writes them |
//! | `api.ContractsApi.call` | its six inputs, and its result up to the `Ok` arm's `flags` and `data` |
//!
//! Only what the endpoint's bytes depend on is compared. The `Some` arms of `PassAuthenticate` and
//! of the fee asset, the address and signature variants the endpoint never writes, `DispatchError`
//! and the dry run's `events` are not: the endpoint never writes them, and a change there leaves
//! its bytes the same. Anything that does not match is named, and nothing is built for that runtime.

use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed, v15::RuntimeMetadataV15};
use parity_scale_codec::Decode;
use scale_info::{PortableRegistry, TypeDef, TypeDefPrimitive, form::PortableForm};

use crate::kreivo::{CONTRACTS_CALL, CONTRACTS_PALLET};

/// The metadata version the endpoint asks the runtime for (`Metadata_metadata_at_version`).
pub const METADATA_VERSION: u32 = 15;

/// The transaction extensions, in the order the endpoint writes their data.
pub const EXTENSIONS: &[&str] = &[
    "PassAuthenticate",
    "CheckNonZeroSender",
    "CheckSpecVersion",
    "CheckTxVersion",
    "CheckGenesis",
    "CheckMortality",
    "CheckNonce",
    "CheckWeight",
    "ChargeAssetTxPayment",
    "DynamicMaxBlockWeight",
];

/// What the shape of a weight is (`sp_weights::Weight`, both parts compact).
const WEIGHT: &str = "{ref_time: Compact<u64>, proof_size: Compact<u64>}";

/// Checks the SCALE bytes of a runtime's metadata (`RuntimeMetadataPrefixed`, V15) against the
/// format the endpoint writes. `Err` names every item that does not match, or why the metadata
/// could not be read; never a partial pass.
pub fn check(metadata: &[u8]) -> Result<(), Vec<String>> {
    let prefixed = RuntimeMetadataPrefixed::decode(&mut &metadata[..])
        .map_err(|e| vec![format!("metadata: does not decode: {e}")])?;
    match prefixed.1 {
        RuntimeMetadata::V15(m) => check_v15(&m),
        _ => Err(vec![format!(
            "metadata: not V{METADATA_VERSION}, the version asked for"
        )]),
    }
}

/// [`check`], on metadata already decoded.
pub fn check_v15(m: &RuntimeMetadataV15) -> Result<(), Vec<String>> {
    let reg = &m.types;
    let mut bad = Vec::new();
    let mut expect = |item: &str, want: &str, found: String| {
        if found != want {
            bad.push(format!("{item}: expected {want}, found {found}"));
        }
    };

    let ext = &m.extrinsic;
    expect("extrinsic.version", "4", ext.version.to_string());
    expect(
        "extrinsic.address",
        "[u8; 32]",
        arm(reg, ext.address_ty.id, 0, "Id"),
    );
    expect(
        "extrinsic.signature",
        "[u8; 64] [u8; 64] [u8; 65]",
        [(0, "Ed25519"), (1, "Sr25519"), (2, "Ecdsa")]
            .map(|(i, n)| arm(reg, ext.signature_ty.id, i, n))
            .join(" "),
    );
    let names: Vec<&str> = ext
        .signed_extensions
        .iter()
        .map(|e| e.identifier.as_str())
        .collect();
    expect(
        "extrinsic.extensions",
        &EXTENSIONS.join(", "),
        names.join(", "),
    );
    if names == EXTENSIONS {
        for e in &ext.signed_extensions {
            let (explicit, implicit) = (e.ty.id, e.additional_signed.id);
            let (want_explicit, found_explicit, want_implicit) = match e.identifier.as_str() {
                // None: the account signs for itself.
                "PassAuthenticate" => ("()", arm(reg, explicit, 0, "None"), "()"),
                "CheckSpecVersion" | "CheckTxVersion" => ("()", shape(reg, explicit), "u32"),
                "CheckGenesis" => ("()", shape(reg, explicit), "[u8; 32]"),
                // Era::Mortal is two bytes; its type is the 256-variant enum Substrate declares.
                "CheckMortality" => ("era", era(reg, explicit), "[u8; 32]"),
                "CheckNonce" => ("Compact<u32>", shape(reg, explicit), "()"),
                // `{ #[codec(compact)] tip, asset_id: None }`.
                "ChargeAssetTxPayment" => ("Compact<u128> ()", fee(reg, explicit), "()"),
                _ => ("()", shape(reg, explicit), "()"),
            };
            let item = format!("extension.{}", e.identifier);
            expect(&format!("{item} (explicit)"), want_explicit, found_explicit);
            expect(
                &format!("{item} (implicit)"),
                want_implicit,
                shape(reg, implicit),
            );
        }
    }

    expect(
        "call.Contracts.call",
        &format!(
            "pallet {CONTRACTS_PALLET} call {CONTRACTS_CALL}: dest: [u8; 32], value: Compact<u128>, gas_limit: {WEIGHT}, storage_deposit_limit: () | Compact<u128>, data: Vec<u8>"
        ),
        contracts_call(m),
    );
    expect(
        "api.ContractsApi.call",
        &format!(
            "origin: [u8; 32], dest: [u8; 32], value: u128, gas_limit: () | {WEIGHT}, storage_deposit_limit: () | u128, input_data: Vec<u8> -> gas_consumed: {WEIGHT}, gas_required: {WEIGHT}, storage_deposit: u128 | u128, debug_message: Vec<u8>, result: {{flags: {{bits: u32}}, data: Vec<u8>}}"
        ),
        contracts_api_call(m),
    );
    if bad.is_empty() { Ok(()) } else { Err(bad) }
}

/// `Contracts::call` as the runtime declares it.
fn contracts_call(m: &RuntimeMetadataV15) -> String {
    let reg = &m.types;
    let Some(pallet) = m.pallets.iter().find(|p| p.name == "Contracts") else {
        return "no pallet Contracts".into();
    };
    let Some(calls) = &pallet.calls else {
        return format!("pallet {} without calls", pallet.index);
    };
    let Some(TypeDef::Variant(v)) = reg.resolve(calls.ty.id).map(|t| &t.type_def) else {
        return format!("pallet {}: calls are not an enum", pallet.index);
    };
    let Some(call) = v.variants.iter().find(|v| v.name == "call") else {
        return format!("pallet {}: no call `call`", pallet.index);
    };
    let fields: Vec<String> = call
        .fields
        .iter()
        .map(|f| {
            let name = f.name.as_deref().unwrap_or("_");
            let ty = match name {
                "dest" => arm(reg, f.ty.id, 0, "Id"),
                "storage_deposit_limit" => option(reg, f.ty.id),
                _ => shape(reg, f.ty.id),
            };
            format!("{name}: {ty}")
        })
        .collect();
    format!(
        "pallet {} call {}: {}",
        pallet.index,
        call.index,
        fields.join(", ")
    )
}

/// `ContractsApi_call` as the runtime declares it: its inputs, and the part of its result the
/// endpoint reads ([`kreivo::decode_dry_run`](crate::kreivo::decode_dry_run)).
fn contracts_api_call(m: &RuntimeMetadataV15) -> String {
    let reg = &m.types;
    let Some(method) = m
        .apis
        .iter()
        .find(|a| a.name == "ContractsApi")
        .and_then(|a| a.methods.iter().find(|m| m.name == "call"))
    else {
        return "no ContractsApi::call".into();
    };
    let inputs: Vec<String> = method
        .inputs
        .iter()
        .map(|p| {
            let ty = match p.name.as_str() {
                "gas_limit" | "storage_deposit_limit" => option(reg, p.ty.id),
                _ => shape(reg, p.ty.id),
            };
            format!("{}: {ty}", p.name)
        })
        .collect();
    let Some(TypeDef::Composite(result)) = reg.resolve(method.output.id).map(|t| &t.type_def)
    else {
        return format!("{} -> not a struct", inputs.join(", "));
    };
    let read: Vec<String> = result
        .fields
        .iter()
        .take(5)
        .map(|f| {
            let name = f.name.as_deref().unwrap_or("_");
            let ty = match name {
                // StorageDeposit::{Refund = 0, Charge = 1}(Balance).
                "storage_deposit" => format!(
                    "{} | {}",
                    arm(reg, f.ty.id, 0, "Refund"),
                    arm(reg, f.ty.id, 1, "Charge")
                ),
                // Result<ExecReturnValue, DispatchError>: only the Ok arm is read.
                "result" => arm(reg, f.ty.id, 0, "Ok"),
                _ => shape(reg, f.ty.id),
            };
            format!("{name}: {ty}")
        })
        .collect();
    format!("{} -> {}", inputs.join(", "), read.join(", "))
}

/// A type with a single unnamed field encodes as that field: followed through.
fn transparent(reg: &PortableRegistry, mut id: u32) -> u32 {
    for _ in 0..16 {
        match reg.resolve(id).map(|t| &t.type_def) {
            Some(TypeDef::Composite(c)) if c.fields.len() == 1 && c.fields[0].name.is_none() => {
                id = c.fields[0].ty.id;
            }
            _ => break,
        }
    }
    id
}

/// The shape of the fields of variant `index`, which must be named `name`, of the enum `id`.
fn arm(reg: &PortableRegistry, id: u32, index: u8, name: &str) -> String {
    let id = transparent(reg, id);
    let Some(TypeDef::Variant(v)) = reg.resolve(id).map(|t| &t.type_def) else {
        return format!("not an enum: {}", shape(reg, id));
    };
    match v.variants.iter().find(|v| v.index == index) {
        Some(v) if v.name == name => fields(reg, &v.fields),
        Some(v) => format!("variant {index} is {}", v.name),
        None => format!("no variant {index}"),
    }
}

/// An `Option`'s two arms: `None` (no fields) | `Some`'s.
fn option(reg: &PortableRegistry, id: u32) -> String {
    format!("{} | {}", arm(reg, id, 0, "None"), arm(reg, id, 1, "Some"))
}

/// `Era`: variant 0 `Immortal`, then `Mortal1`…`Mortal255`, each one byte.
fn era(reg: &PortableRegistry, id: u32) -> String {
    let id = transparent(reg, id);
    let Some(TypeDef::Variant(v)) = reg.resolve(id).map(|t| &t.type_def) else {
        return format!("not an enum: {}", shape(reg, id));
    };
    let ok = v.variants.len() == 256
        && v.variants.iter().all(|v| {
            let want = if v.index == 0 {
                ("Immortal".to_owned(), "()")
            } else {
                (format!("Mortal{}", v.index), "u8")
            };
            v.name == want.0 && fields(reg, &v.fields) == want.1
        });
    if ok { "era".into() } else { shape(reg, id) }
}

/// `ChargeAssetTxPayment`'s explicit data: `tip` (compact), then `asset_id`'s `None` arm.
fn fee(reg: &PortableRegistry, id: u32) -> String {
    let id = transparent(reg, id);
    let Some(TypeDef::Composite(c)) = reg.resolve(id).map(|t| &t.type_def) else {
        return shape(reg, id);
    };
    match c.fields.as_slice() {
        [tip, asset]
            if tip.name.as_deref() == Some("tip") && asset.name.as_deref() == Some("asset_id") =>
        {
            format!(
                "{} {}",
                shape(reg, tip.ty.id),
                arm(reg, asset.ty.id, 0, "None")
            )
        }
        _ => shape(reg, id),
    }
}

/// Fields as one shape: none is `()`, one unnamed is its own shape, named ones `{name: shape}`.
fn fields(reg: &PortableRegistry, fields: &[scale_info::Field<PortableForm>]) -> String {
    match fields {
        [] => "()".into(),
        [f] if f.name.is_none() => shape(reg, f.ty.id),
        _ if fields.iter().all(|f| f.name.is_some()) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|f| format!(
                    "{}: {}",
                    f.name.as_deref().unwrap_or("_"),
                    shape(reg, f.ty.id)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => format!(
            "({})",
            fields
                .iter()
                .map(|f| shape(reg, f.ty.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The encoding shape of type `id`: names of fields and variants, indices, primitives; no paths.
pub fn shape(reg: &PortableRegistry, id: u32) -> String {
    shape_at(reg, id, 0)
}

fn shape_at(reg: &PortableRegistry, id: u32, depth: u8) -> String {
    if depth > 8 {
        return "…".into();
    }
    let Some(t) = reg.resolve(id) else {
        return format!("unknown type {id}");
    };
    let at = |id: u32| shape_at(reg, id, depth + 1);
    match &t.type_def {
        TypeDef::Primitive(p) => match p {
            TypeDefPrimitive::Bool => "bool",
            TypeDefPrimitive::Char => "char",
            TypeDefPrimitive::Str => "str",
            TypeDefPrimitive::U8 => "u8",
            TypeDefPrimitive::U16 => "u16",
            TypeDefPrimitive::U32 => "u32",
            TypeDefPrimitive::U64 => "u64",
            TypeDefPrimitive::U128 => "u128",
            TypeDefPrimitive::U256 => "u256",
            TypeDefPrimitive::I8 => "i8",
            TypeDefPrimitive::I16 => "i16",
            TypeDefPrimitive::I32 => "i32",
            TypeDefPrimitive::I64 => "i64",
            TypeDefPrimitive::I128 => "i128",
            TypeDefPrimitive::I256 => "i256",
        }
        .into(),
        TypeDef::Compact(c) => format!("Compact<{}>", at(c.type_param.id)),
        TypeDef::Sequence(s) => format!("Vec<{}>", at(s.type_param.id)),
        TypeDef::Array(a) => format!("[{}; {}]", at(a.type_param.id), a.len),
        TypeDef::Tuple(t) => format!(
            "({})",
            t.fields
                .iter()
                .map(|f| at(f.id))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDef::Composite(c) => match c.fields.as_slice() {
            [] => "()".into(),
            [f] if f.name.is_none() => at(f.ty.id),
            fs => {
                let named = fs.iter().all(|f| f.name.is_some());
                let parts: Vec<String> = fs
                    .iter()
                    .map(|f| match &f.name {
                        Some(n) if named => format!("{n}: {}", at(f.ty.id)),
                        _ => at(f.ty.id),
                    })
                    .collect();
                if named {
                    format!("{{{}}}", parts.join(", "))
                } else {
                    format!("({})", parts.join(", "))
                }
            }
        },
        TypeDef::Variant(v) => format!(
            "enum{{{}}}",
            v.variants
                .iter()
                .map(|v| format!("{} {}{}", v.index, v.name, {
                    let f = v
                        .fields
                        .iter()
                        .map(|f| at(f.ty.id))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("({f})")
                }))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        TypeDef::BitSequence(_) => "bits".into(),
    }
}

#[cfg(test)]
mod tests {
    //! Against the metadata of Kreivo's spec 134 and spec 136, as their code returns it
    //! (`fixtures/kreivo-spec-*.metadata`, read from Kreivo mainnet), and against each item changed.

    use frame_metadata::v15::SignedExtensionMetadata;
    use scale_info::{Path, Type, TypeDefCompact, TypeDefPrimitive, interner::UntrackedSymbol};

    use super::*;

    fn fixture(spec: u32) -> Vec<u8> {
        std::fs::read(format!(
            "{}/fixtures/kreivo-spec-{spec}.metadata",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn v15(spec: u32) -> RuntimeMetadataV15 {
        match RuntimeMetadataPrefixed::decode(&mut &fixture(spec)[..])
            .unwrap()
            .1
        {
            RuntimeMetadata::V15(m) => m,
            _ => panic!("not V15"),
        }
    }

    /// A new type in `m`'s registry; its id.
    fn add(m: &mut RuntimeMetadataV15, def: TypeDef<PortableForm>) -> u32 {
        let id = u32::try_from(m.types.types.len()).unwrap();
        m.types.types.push(scale_info::PortableType {
            id,
            ty: Type {
                path: Path::default(),
                type_params: Vec::new(),
                type_def: def,
                docs: Vec::new(),
            },
        });
        id
    }

    fn failures(m: &RuntimeMetadataV15) -> Vec<String> {
        check_v15(m).err().unwrap_or_default()
    }

    #[test]
    fn issue_104_spec_134_and_spec_136_take_the_format_the_endpoint_writes() {
        // Spec 134 is transaction version 13, spec 136 transaction version 14: their formats are
        // the same, as far as the endpoint writes and reads them.
        assert_eq!(check(&fixture(134)), Ok(()));
        assert_eq!(check(&fixture(136)), Ok(()));
    }

    #[test]
    fn issue_104_metadata_that_does_not_decode_or_is_not_v15_is_refused() {
        assert!(check(b"meta").is_err());
        assert!(check(&fixture(136)[..1000]).is_err());
        let mut v14 = fixture(136);
        v14[4] = 14;
        assert!(check(&v14).is_err());
    }

    #[test]
    fn issue_104_another_contracts_index_or_call_index_is_named() {
        let mut m = v15(136);
        let p = m
            .pallets
            .iter_mut()
            .find(|p| p.name == "Contracts")
            .unwrap();
        p.index = 81;
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("call.Contracts.call"),
            "{f:?}"
        );

        let mut m = v15(136);
        let ty = m
            .pallets
            .iter()
            .find(|p| p.name == "Contracts")
            .unwrap()
            .calls
            .as_ref()
            .unwrap()
            .ty
            .id;
        let t = m.types.types.iter_mut().find(|t| t.id == ty).unwrap();
        let TypeDef::Variant(v) = &mut t.ty.type_def else {
            panic!()
        };
        v.variants
            .iter_mut()
            .find(|v| v.name == "call")
            .unwrap()
            .index = 7;
        let f = failures(&m);
        assert!(f.len() == 1 && f[0].contains("call 7"), "{f:?}");
    }

    #[test]
    fn issue_104_another_argument_type_of_the_call_is_named() {
        // `value` no longer compact.
        let mut m = v15(136);
        let u128_id = add(&mut m, TypeDef::Primitive(TypeDefPrimitive::U128));
        let ty = m
            .pallets
            .iter()
            .find(|p| p.name == "Contracts")
            .unwrap()
            .calls
            .as_ref()
            .unwrap()
            .ty
            .id;
        let t = m.types.types.iter_mut().find(|t| t.id == ty).unwrap();
        let TypeDef::Variant(v) = &mut t.ty.type_def else {
            panic!()
        };
        let call = v.variants.iter_mut().find(|v| v.name == "call").unwrap();
        let value = call
            .fields
            .iter_mut()
            .find(|f| f.name.as_deref() == Some("value"))
            .unwrap();
        value.ty = UntrackedSymbol::from(u128_id);
        let f = failures(&m);
        assert!(f.len() == 1 && f[0].contains("value: u128,"), "{f:?}");
    }

    #[test]
    fn issue_104_an_extension_added_removed_or_moved_is_named() {
        let mut m = v15(136);
        m.extrinsic.signed_extensions.swap(5, 6);
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("extrinsic.extensions"),
            "{f:?}"
        );

        let mut m = v15(136);
        m.extrinsic.signed_extensions.remove(0);
        assert!(failures(&m)[0].starts_with("extrinsic.extensions"));

        let mut m = v15(136);
        let unit = m.extrinsic.signed_extensions[1].ty;
        m.extrinsic.signed_extensions.insert(
            8,
            SignedExtensionMetadata {
                identifier: "CheckMetadataHash".into(),
                ty: unit,
                additional_signed: unit,
            },
        );
        assert!(failures(&m)[0].starts_with("extrinsic.extensions"));
    }

    #[test]
    fn issue_104_an_extension_whose_data_changes_is_named() {
        // CheckNonce's nonce widened to Compact<u64>.
        let mut m = v15(136);
        let u64_id = add(&mut m, TypeDef::Primitive(TypeDefPrimitive::U64));
        let c = add(
            &mut m,
            TypeDef::Compact(TypeDefCompact::new(UntrackedSymbol::from(u64_id))),
        );
        m.extrinsic.signed_extensions[6].ty = UntrackedSymbol::from(c);
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("extension.CheckNonce (explicit)"),
            "{f:?}"
        );

        // CheckGenesis's implicit no longer a hash.
        let mut m = v15(136);
        m.extrinsic.signed_extensions[4].additional_signed =
            UntrackedSymbol::from(u64_id_of(&mut m));
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("extension.CheckGenesis (implicit)"),
            "{f:?}"
        );

        // CheckWeight starts carrying data.
        let mut m = v15(136);
        m.extrinsic.signed_extensions[7].ty = UntrackedSymbol::from(u64_id_of(&mut m));
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("extension.CheckWeight (explicit)"),
            "{f:?}"
        );
    }

    fn u64_id_of(m: &mut RuntimeMetadataV15) -> u32 {
        add(m, TypeDef::Primitive(TypeDefPrimitive::U64))
    }

    #[test]
    fn issue_104_another_extrinsic_version_address_or_signature_is_named() {
        let mut m = v15(136);
        m.extrinsic.version = 5;
        assert!(failures(&m)[0].starts_with("extrinsic.version"));

        let mut m = v15(136);
        m.extrinsic.address_ty = m.extrinsic.signature_ty;
        assert!(failures(&m)[0].starts_with("extrinsic.address"));

        let mut m = v15(136);
        m.extrinsic.signature_ty = m.extrinsic.address_ty;
        assert!(failures(&m)[0].starts_with("extrinsic.signature"));
    }

    #[test]
    fn issue_104_a_changed_dry_run_api_is_named() {
        let mut m = v15(136);
        let api = m
            .apis
            .iter_mut()
            .find(|a| a.name == "ContractsApi")
            .unwrap();
        let call = api.methods.iter_mut().find(|m| m.name == "call").unwrap();
        call.inputs.swap(0, 1);
        let f = failures(&m);
        assert!(
            f.len() == 1 && f[0].starts_with("api.ContractsApi.call"),
            "{f:?}"
        );

        let mut m = v15(136);
        m.apis.retain(|a| a.name != "ContractsApi");
        assert!(failures(&m)[0].contains("no ContractsApi::call"));
    }
}

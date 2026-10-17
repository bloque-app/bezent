// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! The three reference kinds (`REQ-ID-3`).
//!
//! A request reference is the requester's; a bridge reference is bezent's; an on-chain reference is
//! what a movement's transactions carry on chain (`REQ-ID-2`). They are distinct types with no
//! conversion between them: no `From`, no `Into`, no shared trait that would let one stand in for
//! another. `tests/ui/` holds the compile-fail proofs.

use core::fmt;

/// A reference was empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyReference;

impl fmt::Display for EmptyReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a reference must not be empty")
    }
}

impl std::error::Error for EmptyReference {}

/// The requester's reference for a request (`REQ-RQ-1`, `REQ-ID-1`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestReference(String);

/// bezent's reference for a transfer. Distinct from the request reference and never a replacement
/// for it (`REQ-ID-3`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BridgeReference(String);

/// The reference a movement's transactions write on chain, atomically with the value (`REQ-ID-2`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OnChainReference(Vec<u8>);

impl RequestReference {
    /// A non-empty request reference.
    pub fn new(reference: impl Into<String>) -> Result<Self, EmptyReference> {
        let reference = reference.into();
        if reference.is_empty() {
            Err(EmptyReference)
        } else {
            Ok(Self(reference))
        }
    }

    /// The reference as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl BridgeReference {
    /// A non-empty bridge reference.
    pub fn new(reference: impl Into<String>) -> Result<Self, EmptyReference> {
        let reference = reference.into();
        if reference.is_empty() {
            Err(EmptyReference)
        } else {
            Ok(Self(reference))
        }
    }

    /// The reference as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl OnChainReference {
    /// A non-empty on-chain reference.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, EmptyReference> {
        let bytes = bytes.into();
        if bytes.is_empty() {
            Err(EmptyReference)
        } else {
            Ok(Self(bytes))
        }
    }

    /// The reference's bytes, as written on chain.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Display for RequestReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for BridgeReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Display for OnChainReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("0x")?;
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_id_3_references_are_never_empty() {
        assert_eq!(RequestReference::new(""), Err(EmptyReference));
        assert_eq!(BridgeReference::new(""), Err(EmptyReference));
        assert_eq!(OnChainReference::new(Vec::new()), Err(EmptyReference));
    }
}

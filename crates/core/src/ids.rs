// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Asset and network ids.

use core::fmt;

/// An id was empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmptyId;

impl fmt::Display for EmptyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an id must not be empty")
    }
}

impl std::error::Error for EmptyId {}

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// A non-empty id.
            pub fn new(id: impl Into<String>) -> Result<Self, EmptyId> {
                let id = id.into();
                if id.is_empty() { Err(EmptyId) } else { Ok(Self(id)) }
            }

            /// The id as text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// A bridged asset, as bezent names it (`§5.1`). One per issuer's token family (`REQ-AS-3`).
    AssetId
);

id_newtype!(
    /// A network bezent connects: the issuing network or an origin network.
    NetworkId
);

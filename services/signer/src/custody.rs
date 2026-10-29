// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The custody the signer runs on, chosen by configuration (`DEC-11`): test keys from a file, or
//! delivery keys in a key-management service, AWS KMS or Vault Transit, with the other keys
//! (statement, pause) from Vault, behind one [`KeyBackend`].

use std::fmt;
use std::future::Future;

use crate::keys::{Environment, FileBackend, KeyBackend, KeyError, Scheme};
use crate::kms::{AwsKms, KmsApi, KmsBackend};
use crate::transit::{TransitApi, TransitBackend, VaultTransit};

/// A key-management service that holds delivery keys and signs for them, never releasing them.
pub trait Delivery: KeyBackend + fmt::Debug {
    /// What the configuration calls it, for a refusal.
    const NAME: &'static str;

    /// The ids of the keys held.
    fn key_ids(&self) -> Vec<String>;
}

impl<A: KmsApi> Delivery for KmsBackend<A> {
    const NAME: &'static str = "KMS";

    fn key_ids(&self) -> Vec<String> {
        self.ids().map(str::to_owned).collect()
    }
}

impl<A: TransitApi> Delivery for TransitBackend<A> {
    const NAME: &'static str = "Vault Transit";

    fn key_ids(&self) -> Vec<String> {
        self.ids().map(str::to_owned).collect()
    }
}

/// Delivery keys in a key-management service ([`KmsBackend`] or [`TransitBackend`]) and,
/// optionally, the other keys from Vault, of one environment.
pub struct Mixed<D: Delivery> {
    kms: D,
    vault: Option<FileBackend>,
}

impl<D: Delivery> Mixed<D> {
    /// Joins the two custodies. They must be of one environment and share no key id.
    pub fn new(kms: D, vault: Option<FileBackend>) -> Result<Self, KeyError> {
        if let Some(vault) = &vault {
            if vault.environment() != kms.environment() {
                return Err(KeyError::Refused(format!(
                    "the {} keys and the Vault keys are of different environments (REQ-KY-6)",
                    D::NAME
                )));
            }
            let held = kms.key_ids();
            if let Some(id) = vault.ids().find(|id| held.iter().any(|k| k == id)) {
                return Err(KeyError::Source(format!(
                    "key {id} is in both {} and Vault",
                    D::NAME
                )));
            }
        }
        Ok(Self { kms, vault })
    }

    fn vault_for(&self, id: &str) -> Option<&FileBackend> {
        self.vault.as_ref().filter(|v| v.ids().any(|k| k == id))
    }
}

impl<D: Delivery> KeyBackend for Mixed<D> {
    fn environment(&self) -> Environment {
        self.kms.environment()
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.scheme(id),
            None => self.kms.scheme(id),
        }
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.public_key(id),
            None => self.kms.public_key(id),
        }
    }

    /// A key of the key-management service is never released; one read from Vault is in the
    /// signer's memory.
    fn non_exportable(&self, id: &str) -> bool {
        self.vault_for(id).is_none()
    }

    fn sign(
        &self,
        id: &str,
        message: &[u8],
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let vault = self.vault_for(id);
        async move {
            match vault {
                Some(v) => v.sign(id, message).await,
                None => self.kms.sign(id, message).await,
            }
        }
    }
}

impl<D: Delivery> fmt::Debug for Mixed<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.kms)?;
        if let Some(v) = &self.vault {
            write!(f, " + {v:?}")?;
        }
        Ok(())
    }
}

/// The custody the configuration names.
pub enum Custody {
    /// Test keys from a file (`M-0`).
    File(FileBackend),
    /// KMS (and Vault).
    Kms(Mixed<KmsBackend<AwsKms>>),
    /// Vault Transit (and Vault KV for the other keys).
    Transit(Mixed<TransitBackend<VaultTransit>>),
}

impl KeyBackend for Custody {
    fn environment(&self) -> Environment {
        match self {
            Self::File(b) => b.environment(),
            Self::Kms(b) => b.environment(),
            Self::Transit(b) => b.environment(),
        }
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        match self {
            Self::File(b) => b.scheme(id),
            Self::Kms(b) => b.scheme(id),
            Self::Transit(b) => b.scheme(id),
        }
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self {
            Self::File(b) => b.public_key(id),
            Self::Kms(b) => b.public_key(id),
            Self::Transit(b) => b.public_key(id),
        }
    }

    fn non_exportable(&self, id: &str) -> bool {
        match self {
            Self::File(b) => b.non_exportable(id),
            Self::Kms(b) => b.non_exportable(id),
            Self::Transit(b) => b.non_exportable(id),
        }
    }

    async fn sign(&self, id: &str, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        match self {
            Self::File(b) => b.sign(id, message).await,
            Self::Kms(b) => b.sign(id, message).await,
            Self::Transit(b) => b.sign(id, message).await,
        }
    }
}

impl fmt::Debug for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(b) => b.fmt(f),
            Self::Kms(b) => b.fmt(f),
            Self::Transit(b) => b.fmt(f),
        }
    }
}

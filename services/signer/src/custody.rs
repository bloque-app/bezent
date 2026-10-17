// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The custody the signer runs on, chosen by configuration (`DEC-11`): test keys from a file, or
//! delivery keys in AWS KMS with the other keys (statement, pause) from Vault, behind one
//! [`KeyBackend`].

use std::fmt;
use std::future::Future;

use crate::keys::{Environment, FileBackend, KeyBackend, KeyError, Scheme};
use crate::kms::KmsBackend;
use crate::kms::{AwsKms, KmsApi};

/// Delivery keys in KMS and, optionally, the other keys from Vault, of one environment.
pub struct Mixed<A: KmsApi> {
    kms: KmsBackend<A>,
    vault: Option<FileBackend>,
}

impl<A: KmsApi> Mixed<A> {
    /// Joins the two custodies. They must be of one environment and share no key id.
    pub fn new(kms: KmsBackend<A>, vault: Option<FileBackend>) -> Result<Self, KeyError> {
        if let Some(vault) = &vault {
            if vault.environment() != kms.environment() {
                return Err(KeyError::Refused(
                    "the KMS keys and the Vault keys are of different environments (REQ-KY-6)"
                        .to_owned(),
                ));
            }
            if let Some(id) = vault.ids().find(|id| kms.ids().any(|k| k == *id)) {
                return Err(KeyError::Source(format!(
                    "key {id} is in both KMS and Vault"
                )));
            }
        }
        Ok(Self { kms, vault })
    }

    fn vault_for(&self, id: &str) -> Option<&FileBackend> {
        self.vault.as_ref().filter(|v| v.ids().any(|k| k == id))
    }
}

impl<A: KmsApi> KeyBackend for Mixed<A> {
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

impl<A: KmsApi> fmt::Debug for Mixed<A> {
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
    Kms(Mixed<AwsKms>),
}

impl KeyBackend for Custody {
    fn environment(&self) -> Environment {
        match self {
            Self::File(b) => b.environment(),
            Self::Kms(b) => b.environment(),
        }
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        match self {
            Self::File(b) => b.scheme(id),
            Self::Kms(b) => b.scheme(id),
        }
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self {
            Self::File(b) => b.public_key(id),
            Self::Kms(b) => b.public_key(id),
        }
    }

    async fn sign(&self, id: &str, message: &[u8]) -> Result<Vec<u8>, KeyError> {
        match self {
            Self::File(b) => b.sign(id, message).await,
            Self::Kms(b) => b.sign(id, message).await,
        }
    }
}

impl fmt::Debug for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(b) => b.fmt(f),
            Self::Kms(b) => b.fmt(f),
        }
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The engines the signer runs on, chosen by configuration (`keys.backend`, PLAN `DEC-11` and the
//! ruling `d-delivery-keys`), joined into one [`SigningEngine`]: test keys from a file; or delivery
//! keys in a signing service (AWS KMS, Vault Transit or a Vault signing plugin), with the other keys
//! (statement, pause) optionally from Vault KV. The configuration names which engine holds each
//! key: a key is the signing service's if its list names it, Vault KV's otherwise, and no id may be
//! in both.

use std::fmt;
use std::future::Future;

use crate::engine::{EngineKind, Health, Signable, SigningEngine};
use crate::keys::{Environment, FileBackend, KeyError, Scheme};
use crate::kms::{AwsKms, KmsBackend};
use crate::transit::{TransitBackend, VaultTransit};
use crate::vault_plugin::{PluginBackend, VaultPlugin};

/// Delivery keys in a signing service and, optionally, the other keys from Vault KV, of one
/// environment.
pub struct Mixed<D: SigningEngine> {
    service: D,
    vault: Option<FileBackend>,
}

impl<D: SigningEngine + fmt::Debug> Mixed<D> {
    /// Joins the two engines. They must be of one environment and share no key id.
    pub fn new(service: D, vault: Option<FileBackend>) -> Result<Self, KeyError> {
        if let Some(vault) = &vault {
            if vault.environment() != service.environment() {
                return Err(KeyError::Refused(format!(
                    "the {} keys and the {} keys are of different environments (REQ-KY-6)",
                    service.name(),
                    vault.name()
                )));
            }
            let held = service.key_ids();
            if let Some(id) = vault.ids().find(|id| held.iter().any(|k| k == id)) {
                return Err(KeyError::Source(format!(
                    "key {id} is in both {} and {}",
                    service.name(),
                    vault.name()
                )));
            }
        }
        Ok(Self { service, vault })
    }
}

impl<D: SigningEngine> Mixed<D> {
    fn vault_for(&self, id: &str) -> Option<&FileBackend> {
        self.vault.as_ref().filter(|v| v.ids().any(|k| k == id))
    }
}

impl<D: SigningEngine> SigningEngine for Mixed<D> {
    fn kind(&self) -> EngineKind {
        self.service.kind()
    }

    fn name(&self) -> &'static str {
        self.service.name()
    }

    fn environment(&self) -> Environment {
        self.service.environment()
    }

    fn key_ids(&self) -> Vec<String> {
        let mut ids = self.service.key_ids();
        if let Some(v) = &self.vault {
            ids.extend(v.key_ids());
        }
        ids
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.scheme(id),
            None => self.service.scheme(id),
        }
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.public_key(id),
            None => self.service.public_key(id),
        }
    }

    fn address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.address(id),
            None => self.service.address(id),
        }
    }

    /// A key of the signing service is never released; one read from Vault KV is in the signer's
    /// memory.
    fn non_exportable(&self, id: &str) -> bool {
        match self.vault_for(id) {
            Some(_) => false,
            None => self.service.non_exportable(id),
        }
    }

    fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let vault = self.vault_for(id);
        async move {
            match vault {
                Some(v) => v.sign_prepared(id, scheme, input).await,
                None => self.service.sign_prepared(id, scheme, input).await,
            }
        }
    }

    async fn health(&self) -> Health {
        // The Vault KV keys are in memory: the service's health is the custody's.
        self.service.health().await
    }
}

impl<D: SigningEngine + fmt::Debug> fmt::Debug for Mixed<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.service)?;
        if let Some(v) = &self.vault {
            write!(f, " + {v:?}")?;
        }
        Ok(())
    }
}

/// The custody the configuration names (`keys.backend`).
pub enum Custody {
    /// `file`: test keys from a file.
    File(FileBackend),
    /// `kms`: AWS KMS (and Vault KV).
    Kms(Mixed<KmsBackend<AwsKms>>),
    /// `transit`: Vault Transit (and Vault KV for the other keys).
    Transit(Mixed<TransitBackend<VaultTransit>>),
    /// `vault-plugin`: a Vault signing plugin (and Vault KV for the other keys).
    VaultPlugin(Mixed<PluginBackend<VaultPlugin>>),
}

/// The same call on whichever engine the custody is.
macro_rules! each {
    ($self:expr, $b:ident => $call:expr) => {
        match $self {
            Custody::File($b) => $call,
            Custody::Kms($b) => $call,
            Custody::Transit($b) => $call,
            Custody::VaultPlugin($b) => $call,
        }
    };
}

impl SigningEngine for Custody {
    fn kind(&self) -> EngineKind {
        each!(self, b => b.kind())
    }

    fn name(&self) -> &'static str {
        each!(self, b => b.name())
    }

    fn environment(&self) -> Environment {
        each!(self, b => b.environment())
    }

    fn key_ids(&self) -> Vec<String> {
        each!(self, b => b.key_ids())
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        each!(self, b => b.scheme(id))
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        each!(self, b => b.public_key(id))
    }

    fn address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        each!(self, b => b.address(id))
    }

    fn non_exportable(&self, id: &str) -> bool {
        each!(self, b => b.non_exportable(id))
    }

    async fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> Result<Vec<u8>, KeyError> {
        each!(self, b => b.sign_prepared(id, scheme, input).await)
    }

    async fn health(&self) -> Health {
        each!(self, b => b.health().await)
    }
}

impl fmt::Debug for Custody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        each!(self, b => b.fmt(f))
    }
}

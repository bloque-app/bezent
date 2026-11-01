// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The engines the signer runs on, chosen by configuration (`keys.backend`, PLAN `DEC-11` and the
//! ruling `d-delivery-keys`), joined into one [`SigningEngine`]: test keys from a file; or delivery
//! keys in a signing service (AWS KMS, Vault Transit or a Vault signing plugin), with the other keys
//! (statement, pause) optionally from Vault KV. The configuration names which engine holds each
//! key: a key is the signing service's if its list names it, Vault KV's otherwise, and no id may be
//! in both.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

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

/// A boxed future, for the one place the engines are held behind a pointer.
type Boxed<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// [`SigningEngine`] with its futures boxed, so engines of different types sit in one list
/// ([`SigningEngine`] itself is not dyn-compatible).
trait Member: Send + Sync {
    fn m_name(&self) -> &'static str;
    fn m_environment(&self) -> Environment;
    fn m_scheme(&self, id: &str) -> Result<Scheme, KeyError>;
    fn m_public_key(&self, id: &str) -> Result<Vec<u8>, KeyError>;
    fn m_address(&self, id: &str) -> Result<Vec<u8>, KeyError>;
    fn m_non_exportable(&self, id: &str) -> bool;
    fn m_sign_prepared<'a>(
        &'a self,
        id: &'a str,
        scheme: Scheme,
        input: Signable<'a>,
    ) -> Boxed<'a, Result<Vec<u8>, KeyError>>;
    fn m_health(&self) -> Boxed<'_, Health>;
    fn m_debug(&self) -> String;
}

impl<E: SigningEngine + fmt::Debug> Member for E {
    fn m_name(&self) -> &'static str {
        SigningEngine::name(self)
    }
    fn m_environment(&self) -> Environment {
        SigningEngine::environment(self)
    }
    fn m_scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        SigningEngine::scheme(self, id)
    }
    fn m_public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        SigningEngine::public_key(self, id)
    }
    fn m_address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        SigningEngine::address(self, id)
    }
    fn m_non_exportable(&self, id: &str) -> bool {
        SigningEngine::non_exportable(self, id)
    }
    fn m_sign_prepared<'a>(
        &'a self,
        id: &'a str,
        scheme: Scheme,
        input: Signable<'a>,
    ) -> Boxed<'a, Result<Vec<u8>, KeyError>> {
        Box::pin(SigningEngine::sign_prepared(self, id, scheme, input))
    }
    fn m_health(&self) -> Boxed<'_, Health> {
        Box::pin(SigningEngine::health(self))
    }
    fn m_debug(&self) -> String {
        format!("{self:?}")
    }
}

/// Several signing services at once, each holding its own keys, and optionally the other keys from
/// Vault KV (`keys.backend = "multi"`): the central signer signs Kreivo through Vault Transit and
/// Polygon through a Vault signing plugin, each with its own mount and login role.
///
/// A key is the engine's whose list names it, so which engine signs a key is a fact of the
/// configuration and never of the request. The engines must be of one environment and share no
/// key id; the custody is ready only when every engine is; and whether a key is exportable is
/// decided by the engine that holds it.
pub struct Routed {
    services: Vec<Box<dyn Member>>,
    vault: Option<FileBackend>,
    /// Key id to the index of its service.
    owner: BTreeMap<String, usize>,
}

impl Routed {
    /// An empty custody, with the keys read from Vault KV (if any). Add the signing services with
    /// [`Routed::with`].
    pub fn new(vault: Option<FileBackend>) -> Self {
        Self {
            services: Vec::new(),
            vault,
            owner: BTreeMap::new(),
        }
    }

    /// Adds a signing service. It must be of the custody's environment and hold no key id that
    /// another engine holds.
    pub fn with<E: SigningEngine + fmt::Debug>(mut self, engine: E) -> Result<Self, KeyError> {
        let environment = self
            .services
            .first()
            .map(|s| s.m_environment())
            .or_else(|| self.vault.as_ref().map(SigningEngine::environment));
        if environment.is_some_and(|e| e != SigningEngine::environment(&engine)) {
            return Err(KeyError::Refused(format!(
                "the {} keys are of another environment than the other engines' (REQ-KY-6)",
                SigningEngine::name(&engine)
            )));
        }
        let index = self.services.len();
        for id in SigningEngine::key_ids(&engine) {
            let kv = self
                .vault
                .as_ref()
                .is_some_and(|v| v.ids().any(|k| k == id));
            if kv || self.owner.insert(id.clone(), index).is_some() {
                return Err(KeyError::Source(format!(
                    "key {id} is held by more than one engine"
                )));
            }
        }
        self.services.push(Box::new(engine));
        Ok(self)
    }

    fn vault_for(&self, id: &str) -> Option<&FileBackend> {
        self.vault.as_ref().filter(|v| v.ids().any(|k| k == id))
    }

    fn service_for(&self, id: &str) -> Result<&dyn Member, KeyError> {
        self.owner
            .get(id)
            .and_then(|&i| self.services.get(i))
            .map(|s| &**s)
            .ok_or_else(|| KeyError::UnknownKey(id.to_owned()))
    }
}

impl SigningEngine for Routed {
    fn kind(&self) -> EngineKind {
        EngineKind::SigningService
    }

    fn name(&self) -> &'static str {
        "multiple engines"
    }

    fn environment(&self) -> Environment {
        self.services
            .first()
            .map(|s| s.m_environment())
            .or_else(|| self.vault.as_ref().map(SigningEngine::environment))
            .unwrap_or(Environment::Production)
    }

    fn key_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.owner.keys().cloned().collect();
        if let Some(v) = &self.vault {
            ids.extend(v.key_ids());
        }
        ids
    }

    fn scheme(&self, id: &str) -> Result<Scheme, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.scheme(id),
            None => self.service_for(id)?.m_scheme(id),
        }
    }

    fn public_key(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.public_key(id),
            None => self.service_for(id)?.m_public_key(id),
        }
    }

    fn address(&self, id: &str) -> Result<Vec<u8>, KeyError> {
        match self.vault_for(id) {
            Some(v) => v.address(id),
            None => self.service_for(id)?.m_address(id),
        }
    }

    fn non_exportable(&self, id: &str) -> bool {
        match self.vault_for(id) {
            Some(_) => false,
            None => self.service_for(id).is_ok_and(|s| s.m_non_exportable(id)),
        }
    }

    fn sign_prepared(
        &self,
        id: &str,
        scheme: Scheme,
        input: Signable<'_>,
    ) -> impl Future<Output = Result<Vec<u8>, KeyError>> + Send {
        let vault = self.vault_for(id);
        let service = self.service_for(id);
        async move {
            match (vault, service) {
                (Some(v), _) => v.sign_prepared(id, scheme, input).await,
                (None, Ok(s)) => s.m_sign_prepared(id, scheme, input).await,
                (None, Err(e)) => Err(e),
            }
        }
    }

    async fn health(&self) -> Health {
        // Ready only if every engine is; the first that is not says why. The Vault KV keys are in
        // memory.
        let mut health = Health::ready();
        for service in &self.services {
            let one = service.m_health().await;
            health = health.and(if one.ready {
                one
            } else {
                Health::not_ready(format!("{}: {}", service.m_name(), one.detail))
            });
        }
        health
    }
}

impl fmt::Debug for Routed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shown: Vec<String> = self.services.iter().map(|s| s.m_debug()).collect();
        write!(f, "{}", shown.join(" + "))?;
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
    /// `multi`: several signing services at once, each with its own keys (and Vault KV for the
    /// other keys).
    Multi(Routed),
}

/// The same call on whichever engine the custody is.
macro_rules! each {
    ($self:expr, $b:ident => $call:expr) => {
        match $self {
            Custody::File($b) => $call,
            Custody::Kms($b) => $call,
            Custody::Transit($b) => $call,
            Custody::VaultPlugin($b) => $call,
            Custody::Multi($b) => $call,
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

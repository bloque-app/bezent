// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! Turns a [`SecretRef`] into the secret's value. A service builds one [`Resolver`] from its
//! `[vault]` table (or none), resolves every reference it has at start, and drops the values it no
//! longer needs.
//!
//! A reference to Vault logs in first. The references of a start-up burst share one login (the
//! token is good for a minute); a login older than a few seconds is never reused, so a later
//! resolution logs in again. `--check` and `health` never use a resolver: they validate the
//! configuration ([`VaultConfig::check`]) and touch no network.

use std::path::Path;

use tokio::sync::Mutex;
use zeroize::Zeroizing;

use crate::Error;
use crate::client::{Client, Session};
use crate::config::{SecretRef, StoreSource, VaultConfig};

/// Resolves [`SecretRef`]s.
pub struct Resolver {
    client: Option<Client>,
    session: Mutex<Option<Session>>,
}

impl Resolver {
    /// A resolver for a service whose configuration has `vault` (or none). Contacts nothing.
    pub fn new(vault: Option<&VaultConfig>) -> Result<Self, Error> {
        Ok(Self::with_client(vault.map(Client::new).transpose()?))
    }

    /// A resolver over `client`.
    pub fn with_client(client: Option<Client>) -> Self {
        Self {
            client,
            session: Mutex::new(None),
        }
    }

    /// A session for this burst: the last one while it is fresh, a new login otherwise.
    pub async fn session(&self) -> Result<Session, Error> {
        let client = self.client.as_ref().ok_or_else(|| {
            Error::Config("a secret is read from Vault but there is no [vault] table".to_owned())
        })?;
        let mut held = self.session.lock().await;
        if let Some(session) = held.as_ref().filter(|s| s.fresh()) {
            return Ok(session.clone());
        }
        let session = client.login().await?;
        *held = Some(session.clone());
        Ok(session)
    }

    /// The value `secret` names.
    pub async fn resolve(&self, secret: &SecretRef) -> Result<Zeroizing<String>, Error> {
        match secret {
            SecretRef::File(path) => read_file(path),
            SecretRef::Vault(v) => self.session().await?.read_field(&v.path, &v.field).await,
        }
    }

    /// The store's URL, built from its login and password.
    pub async fn store_url(&self, store: &StoreSource) -> Result<Zeroizing<String>, Error> {
        store.check()?;
        let login = self.resolve(&store.login).await?;
        let password = self.resolve(&store.password).await?;
        store.url(&login, &password)
    }
}

/// The contents of a secret file, surrounding whitespace removed. A file with nothing in it is an
/// [`Error::Empty`].
pub fn read_file(path: &Path) -> Result<Zeroizing<String>, Error> {
    let text = Zeroizing::new(std::fs::read_to_string(path).map_err(|e| Error::File {
        path: path.display().to_string(),
        kind: e.kind(),
    })?);
    let value = Zeroizing::new(text.trim().to_owned());
    if value.is_empty() {
        return Err(Error::Empty {
            what: path.display().to_string(),
        });
    }
    Ok(value)
}

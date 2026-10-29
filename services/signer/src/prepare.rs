// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! `bezent-signer prepare <spec-path>`: sets up a signer that runs beside its one caller, a sidecar
//! in the caller's task (the Fargate design; `REQ-CU-1`, `REQ-AR-3`, `REQ-KY-1`). Images are
//! distroless, so the preparation is a subcommand of the binary, run once as root by an init
//! container before the signer and the caller start.
//!
//! It does three things, each leaving the file or directory to the one user who reads it:
//!
//! - writes the caller's token, 32 random bytes as 64 hex digits, to `token_file` (mode 0600, the
//!   caller's user and group). The signer never sees it;
//! - renders the signer's configuration from `template` to `config` (mode 0400, the signer's
//!   user), with exactly one caller (its name, its user id and the SHA-256 of that token) and
//!   nothing else changed. The template's `callers` must be absent or empty, so the keys the
//!   template binds go to the caller named here and to no other;
//! - creates `socket_dir`, owned by the signer's user and the caller's group, mode 2750 (the
//!   caller reaches the signer's socket, mode 0660, through the group; nobody else can).
//!
//! The spec:
//!
//! ```json
//! { "template": "/etc/bezent-signer/sidecar.json",
//!   "config": "/run/bezent/signer-config/signer.json",
//!   "signer": { "uid": 10001, "gid": 10001 },
//!   "caller": { "name": "evm-endpoint", "uid": 10002, "gid": 10002 },
//!   "token_file": "/run/bezent/caller-token/token",
//!   "socket_dir": "/run/bezent/signer" }
//! ```
//!
//! The template is a signer configuration, validated as one after rendering; its `socket` must sit
//! in `socket_dir`. A run replaces the token, so the token and the hash in the configuration always
//! belong together; running it again is safe and changes no mode or owner. Files are written to a
//! temporary name and renamed, so a reader never sees half a file. The token is never printed.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rustix::fs::{Gid, Uid, chown};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::config::Config;

/// The user and group a file or directory is given to.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    /// The user id.
    pub uid: u32,
    /// The group id.
    pub gid: u32,
}

/// The one caller of this signer.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallerSpec {
    /// A name for the log.
    pub name: String,
    /// The caller's user id.
    pub uid: u32,
    /// The caller's group id.
    pub gid: u32,
}

/// What to prepare (module documentation).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The signer's configuration, without callers.
    pub template: PathBuf,
    /// Where the rendered configuration goes.
    pub config: PathBuf,
    /// The user and group the signer runs as.
    pub signer: Owner,
    /// The caller.
    pub caller: CallerSpec,
    /// Where the caller's token goes.
    pub token_file: PathBuf,
    /// The directory of the signer's socket.
    pub socket_dir: PathBuf,
}

/// Why the preparation was refused or failed. Never carries the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareError(pub String);

impl std::fmt::Display for PrepareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "prepare refused: {}", self.0)
    }
}

impl std::error::Error for PrepareError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, PrepareError> {
    Err(PrepareError(why.into()))
}

fn io(what: &str, path: &Path, e: &std::io::Error) -> PrepareError {
    PrepareError(format!("{what} {}: {}", path.display(), e.kind()))
}

/// Mode of the caller's token file.
pub const TOKEN_MODE: u32 = 0o600;
/// Mode of the rendered configuration.
pub const CONFIG_MODE: u32 = 0o400;
/// Mode of the socket directory: setgid, so the socket belongs to the caller's group.
pub const SOCKET_DIR_MODE: u32 = 0o2750;
/// Mode of the directories that hold the token and the configuration.
pub const PRIVATE_DIR_MODE: u32 = 0o700;

fn give(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<(), PrepareError> {
    chown(path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).map_err(|e| {
        PrepareError(format!(
            "cannot give {} to {uid}:{gid}: {e}",
            path.display()
        ))
    })?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| io("cannot set the mode of", path, &e))
}

fn directory(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<(), PrepareError> {
    std::fs::create_dir_all(path).map_err(|e| io("cannot create", path, &e))?;
    give(path, uid, gid, mode)
}

/// Writes `bytes` to `path` for `owner` with `mode`: the file is made private to the user that
/// creates it, given away, then renamed into place.
fn write(path: &Path, bytes: &[u8], owner: Owner, mode: u32) -> Result<(), PrepareError> {
    let name = path
        .file_name()
        .ok_or_else(|| PrepareError(format!("{} is not a file path", path.display())))?;
    let temporary = path.with_file_name(format!(".{}.tmp", name.to_string_lossy()));
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| io("cannot create", &temporary, &e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| io("cannot write", &temporary, &e))?;
    give(&temporary, owner.uid, owner.gid, mode)?;
    std::fs::rename(&temporary, path).map_err(|e| io("cannot replace", path, &e))
}

fn random_token() -> Result<Zeroizing<String>, PrepareError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(bytes.as_mut_slice()))
        .map_err(|e| PrepareError(format!("cannot read random bytes: {}", e.kind())))?;
    Ok(Zeroizing::new(hex::encode(bytes.as_slice())))
}

/// The configuration the signer runs on: `template` with `callers` set to the one caller.
fn render(spec: &Spec, token_sha256: &str) -> Result<Vec<u8>, PrepareError> {
    let bytes = std::fs::read(&spec.template).map_err(|e| io("cannot read", &spec.template, &e))?;
    let mut template: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| PrepareError("the template is not JSON".to_owned()))?;
    let Some(object) = template.as_object_mut() else {
        return refuse("the template is not a JSON object");
    };
    if object
        .get("callers")
        .is_some_and(|c| c.as_array().is_none_or(|a| !a.is_empty()))
    {
        return refuse("the template names callers; the caller is the spec's alone");
    }
    object.insert(
        "callers".to_owned(),
        serde_json::json!([{
            "name": spec.caller.name,
            "uid": spec.caller.uid,
            "token_sha256": [token_sha256],
        }]),
    );
    let rendered = serde_json::to_vec_pretty(&template)
        .map_err(|_| PrepareError("the configuration cannot be written".to_owned()))?;
    let config = Config::parse(&rendered).map_err(|e| PrepareError(e.to_string()))?;
    if config.bindings.is_empty() {
        return refuse("the template binds no key, so the caller would be granted nothing");
    }
    if config.socket.parent() != Some(spec.socket_dir.as_path()) || !config.socket.is_absolute() {
        return refuse("the template's socket is not a file in socket_dir");
    }
    Ok(rendered)
}

/// Prepares the token, the configuration and the socket directory.
pub fn run(spec: &Spec) -> Result<(), PrepareError> {
    if spec.caller.uid == spec.signer.uid {
        return refuse(format!(
            "caller {} would run as the signer's own user id; the signer's key must not be \
             readable by any other process (REQ-CU-1, REQ-AR-3)",
            spec.caller.name
        ));
    }
    apply(spec)
}

/// As [`run`], without the check that the caller is not the signer's user. **Tests only**: a test
/// runs the signer and its caller as one user, and an ordinary user can give nothing to another.
/// The binary never calls it.
#[doc(hidden)]
pub fn run_as_one_user_in_tests(spec: &Spec) -> Result<(), PrepareError> {
    apply(spec)
}

fn apply(spec: &Spec) -> Result<(), PrepareError> {
    let token = random_token()?;
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    // Rendered first: a template that is refused leaves no new token behind.
    let rendered = render(spec, &hash)?;
    let (signer, caller) = (spec.signer, &spec.caller);
    let parent = |path: &Path| -> Result<PathBuf, PrepareError> {
        path.parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| PrepareError(format!("{} has no directory", path.display())))
    };
    directory(
        &parent(&spec.token_file)?,
        caller.uid,
        caller.gid,
        PRIVATE_DIR_MODE,
    )?;
    write(
        &spec.token_file,
        token.as_bytes(),
        Owner {
            uid: caller.uid,
            gid: caller.gid,
        },
        TOKEN_MODE,
    )?;
    directory(
        &parent(&spec.config)?,
        signer.uid,
        signer.gid,
        PRIVATE_DIR_MODE,
    )?;
    write(&spec.config, &rendered, signer, CONFIG_MODE)?;
    directory(&spec.socket_dir, signer.uid, caller.gid, SOCKET_DIR_MODE)
}

/// Reads the spec at `path` and prepares.
pub fn run_file(path: &Path) -> Result<Spec, PrepareError> {
    let bytes = std::fs::read(path).map_err(|e| io("cannot read", path, &e))?;
    let spec: Spec = serde_json::from_slice(&bytes).map_err(|e| {
        PrepareError(format!(
            "not a valid spec ({:?} error at line {}, column {})",
            e.classify(),
            e.line(),
            e.column()
        ))
    })?;
    run(&spec)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use super::*;

    fn me() -> Owner {
        let meta = std::fs::metadata(".").unwrap();
        Owner {
            uid: meta.uid(),
            gid: meta.gid(),
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bezent-prepare-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn template(socket: &Path) -> serde_json::Value {
        serde_json::json!({
            "socket": socket,
            "callers": [],
            "networks": [{"id": "kreivo-local", "environment": "test", "issuing": true,
                          "contract_account": "0x01", "genesis": format!("0x{}", "4b".repeat(32))}],
            "keys": {"backend": "file", "path": "keys.json"},
            "bindings": [{"key": "kreivo-mint", "network": "kreivo-local", "purposes": ["mint"],
                          "window_cap": "1000", "window_seconds": 60}],
        })
    }

    /// A spec in `d` that gives everything to the test's own user: an ordinary user can give
    /// nothing to another (`apply` skips the check that the caller is not the signer).
    fn spec(d: &Path, template: &serde_json::Value) -> Spec {
        std::fs::write(d.join("template.json"), template.to_string()).unwrap();
        let o = me();
        Spec {
            template: d.join("template.json"),
            config: d.join("config/signer.json"),
            signer: o,
            caller: CallerSpec {
                name: "evm-endpoint".into(),
                uid: o.uid,
                gid: o.gid,
            },
            token_file: d.join("token/token"),
            socket_dir: d.join("sock"),
        }
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    #[test]
    fn req_ky_1_the_token_the_hash_the_modes_and_the_socket_directory() {
        let d = dir("modes");
        let s = spec(&d, &template(&d.join("sock/signer.sock")));
        apply(&s).unwrap();
        let token = std::fs::read_to_string(&s.token_file).unwrap();
        assert_eq!(token.len(), 64);
        assert!(hex::decode(&token).is_ok());
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&s.config).unwrap()).unwrap();
        let callers = config["callers"].as_array().unwrap();
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["name"], "evm-endpoint");
        assert_eq!(
            callers[0]["token_sha256"],
            serde_json::json!([hex::encode(Sha256::digest(token.as_bytes()))])
        );
        // The configuration holds the hash and nothing of the token.
        assert!(!std::fs::read_to_string(&s.config).unwrap().contains(&token));
        assert_eq!(mode(&s.token_file), 0o600);
        assert_eq!(mode(&s.config), 0o400);
        assert_eq!(mode(s.token_file.parent().unwrap()), 0o700);
        assert_eq!(mode(s.config.parent().unwrap()), 0o700);
        assert_eq!(mode(&s.socket_dir), 0o2750);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn req_ky_1_a_second_run_replaces_the_token_and_the_hash_together() {
        let d = dir("again");
        let s = spec(&d, &template(&d.join("sock/signer.sock")));
        apply(&s).unwrap();
        let first = std::fs::read_to_string(&s.token_file).unwrap();
        apply(&s).unwrap();
        let second = std::fs::read_to_string(&s.token_file).unwrap();
        assert_ne!(first, second);
        let config = std::fs::read_to_string(&s.config).unwrap();
        assert!(config.contains(&hex::encode(Sha256::digest(second.as_bytes()))));
        assert!(!config.contains(&hex::encode(Sha256::digest(first.as_bytes()))));
        assert_eq!(
            (mode(&s.token_file), mode(&s.config), mode(&s.socket_dir)),
            (0o600, 0o400, 0o2750)
        );
        // No temporary file is left behind.
        assert!(
            std::fs::read_dir(s.token_file.parent().unwrap())
                .unwrap()
                .all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp"))
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn req_cu_1_a_caller_that_is_the_signers_own_user_is_refused() {
        let d = dir("shared");
        let s = spec(&d, &template(&d.join("sock/signer.sock")));
        let err = run(&s).unwrap_err();
        assert!(err.0.contains("REQ-CU-1"), "{err}");
        assert!(!s.token_file.exists() && !s.config.exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn req_ar_3_a_template_that_names_callers_binds_nothing_or_misplaces_the_socket_is_refused() {
        let d = dir("template");
        let sock = d.join("sock/signer.sock");
        let refused = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut t = template(&sock);
            edit(&mut t);
            let s = spec(&d, &t);
            let err = apply(&s).unwrap_err();
            assert!(!s.token_file.exists() && !s.config.exists(), "{err}");
            err.0
        };
        let named = refused(&|t| {
            t["callers"] = serde_json::json!([{"name": "x", "uid": 1, "token_sha256": ["00"]}]);
        });
        assert!(named.contains("callers"), "{named}");
        let bare = refused(&|t| t["bindings"] = serde_json::json!([]));
        assert!(bare.contains("binds no key"), "{bare}");
        let elsewhere = refused(&|t| t["socket"] = "/run/other/signer.sock".into());
        assert!(elsewhere.contains("socket_dir"), "{elsewhere}");
        let relative = refused(&|t| t["socket"] = "signer.sock".into());
        assert!(relative.contains("socket_dir"), "{relative}");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_spec_with_an_unknown_field_is_refused() {
        let d = dir("spec");
        let path = d.join("spec.json");
        std::fs::write(&path, r#"{"template":"t","extra":1}"#).unwrap();
        assert!(run_file(&path).is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! What the signer process does to itself at start (`REQ-CU-1`, `REQ-KY-1`; review S4).
//!
//! Key material must never be readable outside the component that signs with it. A core dump is a
//! copy of the process's memory, keys included, written where other users may read it; and on
//! Linux a dumpable process's memory is open to `ptrace` and `/proc/<pid>/mem` by any process of
//! its user. [`forbid_core_dumps`] closes both before a key is loaded: the core-file limit goes to
//! zero (soft and hard, so it cannot be raised again), and on Linux the process is marked not
//! dumpable (`PR_SET_DUMPABLE 0`). macOS has no `PR_SET_DUMPABLE`; there the limit is the whole of
//! it, and a failure is reported, not fatal.

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The user id the signer runs as (its effective one).
pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Forbids core dumps of this process. See the module documentation.
pub fn forbid_core_dumps() -> Result<(), String> {
    setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )
    .map_err(|e| format!("cannot set the core-file limit to 0 (REQ-CU-1): {e}"))?;
    #[cfg(target_os = "linux")]
    rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable)
        .map_err(|e| format!("cannot mark the process not dumpable (REQ-CU-1): {e}"))?;
    Ok(())
}

/// Whether this process can dump core: its core-file limit is not zero, or (on Linux) it is
/// dumpable.
pub fn can_dump_core() -> bool {
    let limit = getrlimit(Resource::Core);
    if limit.current != Some(0) || limit.maximum != Some(0) {
        return true;
    }
    #[cfg(target_os = "linux")]
    if rustix::process::dumpable_behavior() != Ok(rustix::process::DumpableBehavior::NotDumpable) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn req_cu_1_the_signer_process_cannot_dump_core() {
        forbid_core_dumps().expect("core dumps forbidden");
        assert!(!can_dump_core());
        // The hard limit is zero: it cannot be raised again.
        assert!(
            setrlimit(
                Resource::Core,
                Rlimit {
                    current: Some(1 << 20),
                    maximum: Some(1 << 20),
                },
            )
            .is_err()
        );
    }
}

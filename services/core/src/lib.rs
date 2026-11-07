// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! # bezent-core-service
//!
//! The core of PLAN `DEC-3` (`F-003`): the one process that decides. It runs the state machine of
//! SPEC §5.4 over the store, the named conditions of §10, deadlines and review (§5.9), and records
//! the intents the signer signs. The endpoints only observe and the signer only signs what the
//! core recorded; nothing a requester says is taken as an outcome (`REQ-AR-1`, `REQ-AR-2`,
//! `REQ-RQ-7`).
//!
//! - [`engine`]: [`Core::cycle`], the state machine, deliveries in two steps (order, then signing
//!   intent over the endpoint's prepared bytes), verification of every returned signature
//!   (`AC-I1.2`), conditions and review.
//! - [`store`]: what the core reads, and the transition it appends (`REQ-TR-6`).
//! - [`refs`]: the references the core derives (`REQ-ID-3`, `REQ-IN-4`, `REQ-RF-3`).
//! - [`cutoff`]: route cut-offs (`CND-CutOff`, amendment 0010 A2).
//! - [`late_mint`] (issue #26): a mint of a reference after its proof of non-execution.
//! - [`reserve`] (`F-004`): the reserve book, the one-minute reserve check and the daily signed
//!   statement.
//! - [`signals`] (`F-014`): a key-compromise alert raised by the signer or an endpoint, put in front of a human.
//! - [`fees`] (`F-014`): the floor of each network's fee position, in this configuration, and the page when a position is below it.
//! - [`observe`] (`F-014`): the observables of `REQ-ST-6`, written once a minute for the alarms.
//! - [`config`]: the configuration file.

pub mod config;
pub mod cutoff;
pub mod engine;
pub mod fees;
pub mod health;
pub mod late_mint;
pub mod observe;
pub mod refs;
pub mod reserve;
pub mod signals;
pub mod store;

pub use config::Config;
pub use engine::{Core, Report, Settings};

/// One pass of the running core (amendment 0028): the reserve check first, so an asset it cannot
/// see is held before the cycle can start new work for it, then the cycle. Returns whether each
/// answered: the check's `Err` is a store that could not list the assets (every asset is then held,
/// `reserve::check::Unseen::all`), the cycle's a cycle that failed. `core` must be watching
/// `check` ([`Core::watching`]).
pub async fn pass<R, P>(
    core: &Core,
    check: &reserve::check::ReserveCheck<R, P>,
    clock: &impl reserve::check::Clock,
) -> (
    sqlx::Result<Vec<reserve::check::Evaluation>>,
    sqlx::Result<Report>,
)
where
    R: reserve::readings::Readings,
    P: reserve::check::Pager,
{
    let evaluations = check.tick(clock).await;
    let report = core.cycle(clock.now()).await;
    (evaluations, report)
}

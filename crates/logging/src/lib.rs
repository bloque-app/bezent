// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: Apache-2.0 OR MIT
//! # bezent-logging
//!
//! The one place the services' log format is decided (`F-014`, `deploy/runbooks/alerts.md`). A
//! page is an ERROR line a metric filter matches on the string `kind=<kind>`. `tracing-subscriber`
//! colours its output with ANSI escapes unless `NO_COLOR` is set in the environment, whether or not
//! the output is a terminal; a colourised line wraps the field names in escapes
//! (`\x1b[3mkind\x1b[0m\x1b[2m=\x1b[0m...`), so `kind=` would not appear in it. The deployment
//! sets `NO_COLOR=1` (the infrastructure repository), but nothing in this repository did, and a
//! filter should not depend on a setting made elsewhere. The format is therefore fixed here, not
//! left to the environment: plain text, no colour, no target.
//!
//! Every service starts its log with [`init`]; a test that checks what a page looks like in the
//! log builds its subscriber with [`subscriber`], which is the same one.

use tracing_subscriber::fmt::{MakeWriter, SubscriberBuilder};

/// The services' formatter, before it has a writer.
fn builder() -> SubscriberBuilder {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_ansi(false)
}

/// Starts the process's log, to standard output, in the services' format. Panics if a subscriber is
/// already set, as `tracing_subscriber`'s own `init` does.
pub fn init() {
    builder().init();
}

/// The services' subscriber writing to `writer` instead of standard output, for a test to capture
/// what the real log would hold.
pub fn subscriber<W>(writer: W) -> impl tracing::Subscriber + Send + Sync
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    builder().with_writer(writer).finish()
}

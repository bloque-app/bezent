// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The signer's log: one line per signature and per refusal, each with the intent id (`REQ-AR-3`:
//! the signer logs every signature). Nothing written here carries key material (`REQ-KY-1`).

use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex};

/// Where the signer's lines go: standard error in the service, a buffer in tests.
#[derive(Clone)]
pub struct Log(Arc<Mutex<Box<dyn Write + Send>>>);

impl Log {
    /// A log to standard error.
    pub fn stderr() -> Self {
        Self::to(std::io::stderr())
    }

    /// A log to `writer`.
    pub fn to(writer: impl Write + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(writer))))
    }

    /// A log that keeps its lines in memory, and the handle that reads them.
    pub fn capture() -> (Self, Captured) {
        let captured = Captured::default();
        (Self::to(captured.clone()), captured)
    }

    /// Writes one line.
    pub fn line(&self, args: fmt::Arguments<'_>) {
        let mut out = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "bezent-signer: {args}");
        let _ = out.flush();
    }
}

impl fmt::Debug for Log {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Log")
    }
}

/// The lines a [`Log::capture`] log has received.
#[derive(Clone, Default)]
pub struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    /// Everything written so far.
    pub fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Everything written so far, as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The submission gate's state, put where an alarm can read it (issue #104, `REQ-ST-6`).
//!
//! A closed gate leaves the endpoint healthy: it keeps observing, so the liveness file
//! ([`crate::health`]) says nothing of it, while every delivery waits. This module writes the gate's
//! state once a minute to standard output as a CloudWatch *embedded metric format* line, in the
//! same namespace and the same shape as the core's observables (`services/core/src/observe.rs`,
//! `deploy/runbooks/alerts.md`), so an alarm on it needs no filter and no agent.
//!
//! The metric is [`METRIC`], dimension `Network`: `0` while the gate is open, `1` while it is
//! closed. A gate that could not be judged ([`GateState::Unavailable`]), or a finalized head that
//! could not be read, is not a number: the metric is left out of the line and named in its
//! `unknown` list, so an alarm that treats missing data as breaching catches it, and it is never a
//! zero (`REQ-NX-3`). The line carries the network and the state, and no reason: the reason is in the
//! log line that records the change of state.

use std::time::{Duration, Instant};

use serde_json::json;

use crate::submit::GateState;

/// The namespace the core's observables use.
pub const NAMESPACE: &str = "bezent";

/// The metric: `0` the gate is open, `1` it is closed.
pub const METRIC: &str = "submission_gate_state";

/// How often the state is written.
pub const EVERY: Duration = Duration::from_secs(60);

/// The metric's value for `state`; `None` is "not known".
pub fn value(state: Option<&GateState>) -> Option<u8> {
    match state {
        Some(GateState::Open(_)) => Some(0),
        Some(GateState::Closed(_)) => Some(1),
        Some(GateState::Unavailable(_)) | None => None,
    }
}

/// The embedded-metric-format line for `network` at `timestamp_ms`.
pub fn emf_line(network: &str, value: Option<u8>, timestamp_ms: i64) -> String {
    let mut record = json!({
        "Network": network,
        "kind": "observables",
        "_aws": {
            "Timestamp": timestamp_ms,
            "CloudWatchMetrics": [{
                "Namespace": NAMESPACE,
                "Dimensions": [["Network"]],
                "Metrics": value.map_or_else(Vec::new, |_| vec![json!({"Name": METRIC, "Unit": "None"})]),
            }],
        },
    });
    match value {
        Some(v) => record[METRIC] = json!(v),
        None => record["unknown"] = json!([METRIC]),
    }
    record.to_string()
}

/// Says when the state is due: at its first call, then once a minute.
#[derive(Debug, Default)]
pub struct Emitter {
    last: Option<Instant>,
}

impl Emitter {
    /// An emitter that is due at its first call.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a line is due at `now`; if it is, the minute starts again.
    pub fn due(&mut self, now: Instant) -> bool {
        if self.last.is_some_and(|l| now.duration_since(l) < EVERY) {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::chain::RuntimeVersion;

    fn line(value: Option<u8>) -> Value {
        serde_json::from_str(&emf_line("kreivo", value, 1_700_000_000_000)).unwrap()
    }

    #[test]
    fn issue_104_an_open_gate_is_0_and_a_closed_one_is_1() {
        let open = GateState::Open(RuntimeVersion {
            spec_version: 136,
            transaction_version: 14,
        });
        assert_eq!(value(Some(&open)), Some(0));
        assert_eq!(value(Some(&GateState::Closed("why".into()))), Some(1));
    }

    #[test]
    fn issue_104_a_gate_that_is_not_judged_is_unknown_never_zero() {
        assert_eq!(value(Some(&GateState::Unavailable("no".into()))), None);
        assert_eq!(value(None), None);
        let v = line(None);
        assert!(v.get(METRIC).is_none());
        assert_eq!(v["unknown"], json!([METRIC]));
        assert_eq!(v["_aws"]["CloudWatchMetrics"][0]["Metrics"], json!([]));
    }

    #[test]
    fn issue_104_the_line_is_embedded_metric_format_in_the_core_namespace() {
        for state in [0u8, 1] {
            let v = line(Some(state));
            let m = &v["_aws"]["CloudWatchMetrics"][0];
            assert_eq!(m["Namespace"], "bezent");
            assert_eq!(m["Dimensions"], json!([["Network"]]));
            assert_eq!(m["Metrics"][0]["Name"], METRIC);
            assert_eq!(v[METRIC], json!(state));
            assert_eq!(v["Network"], "kreivo");
            assert_eq!(v["_aws"]["Timestamp"], 1_700_000_000_000_i64);
            assert!(v.get("unknown").is_none());
        }
        // No reason, address or key: the log line has those.
        assert!(!emf_line("kreivo", Some(1), 0).contains("0x"));
    }

    #[test]
    fn issue_104_the_state_is_due_at_the_first_call_then_once_a_minute() {
        let mut e = Emitter::new();
        let t = Instant::now();
        assert!(e.due(t));
        assert!(!e.due(t + Duration::from_secs(59)));
        assert!(e.due(t + EVERY));
        assert!(!e.due(t + EVERY + Duration::from_secs(1)));
    }
}

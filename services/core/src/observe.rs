// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! The observables of `REQ-ST-6`, put where an alarm can read them (`F-014`, `AC-H3.1`).
//!
//! The store keeps every quantity and `ops_observables()` (migration 0090) reads them as one
//! table. This module reads that table with the core's own login once a minute and writes it to
//! standard output as CloudWatch *embedded metric format* lines: one JSON object per line and per
//! set of dimensions, which the log service turns into metrics with no filter and no agent. The
//! services serve no port, so the log is the only exporter there is.
//!
//! A value the store does not know (a network that never recorded a final head, a reserve whose
//! reading is unknown) is not written as a number: it is named in the line's `unknown` list and
//! left out of the metrics, so an alarm on a missing metric catches it, and it is never a zero
//! (`REQ-NX-3`). A read that fails writes nothing, which is the same alarm.
//!
//! The lines carry counts, ages, outcomes and reserve amounts, and nothing that names a
//! transfer, a destination or a requester.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use sqlx::PgPool;

/// The namespace the metrics are published under.
pub const NAMESPACE: &str = "bezent";

/// How often the observables are written (`NFR-5` asks the reserve equation each minute; one
/// line a minute keeps the metrics at CloudWatch's standard resolution).
pub const EVERY: Duration = Duration::from_secs(60);

/// One row of `ops_observables()`.
#[derive(Clone, Debug, PartialEq)]
pub struct Observable {
    /// The metric's name.
    pub metric: String,
    /// The asset, when the metric is per asset.
    pub asset: Option<String>,
    /// The network, when the metric is per network or per route.
    pub network: Option<String>,
    /// The direction, when the metric is per route.
    pub direction: Option<String>,
    /// The value; `None` is "not known".
    pub value: Option<f64>,
}

type Row = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<f64>,
);
type Values<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>);

/// Reads the observables with the core's login.
pub async fn read(pool: &PgPool) -> sqlx::Result<Vec<Observable>> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT metric, asset, network, direction, value::float8 FROM ops_observables() \
             ORDER BY metric, asset, network, direction",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(metric, asset, network, direction, value)| Observable {
            metric,
            asset,
            network,
            direction,
            value,
        })
        .collect())
}

fn unit(metric: &str) -> &'static str {
    if metric.ends_with("_seconds") {
        "Seconds"
    } else if metric == "reserve_amount" || metric == "reserve_equation_outcome" {
        "None"
    } else {
        "Count"
    }
}

/// The embedded-metric-format lines for `rows` at `timestamp_ms`: one per set of dimensions the
/// rows carry (`Asset`, `Network`, `Direction`, each present or not).
pub fn emf_lines(rows: &[Observable], timestamp_ms: i64) -> Vec<String> {
    let mut groups: BTreeMap<(bool, bool, bool), Vec<&Observable>> = BTreeMap::new();
    for r in rows {
        groups
            .entry((
                r.asset.is_some(),
                r.network.is_some(),
                r.direction.is_some(),
            ))
            .or_default()
            .push(r);
    }
    // One record per distinct dimension *values* too: a record holds one value per metric name.
    let mut lines = Vec::new();
    for ((has_asset, has_network, has_direction), members) in groups {
        let mut by_values: BTreeMap<Values<'_>, Vec<&Observable>> = BTreeMap::new();
        for m in members {
            by_values
                .entry((
                    m.asset.as_deref(),
                    m.network.as_deref(),
                    m.direction.as_deref(),
                ))
                .or_default()
                .push(m);
        }
        for ((asset, network, direction), metrics) in by_values {
            let mut dimensions = Vec::new();
            let mut record = Map::new();
            for (present, name, value) in [
                (has_asset, "Asset", asset),
                (has_network, "Network", network),
                (has_direction, "Direction", direction),
            ] {
                if present {
                    dimensions.push(name);
                    record.insert(name.to_owned(), json!(value.unwrap_or_default()));
                }
            }
            let mut declared = Vec::new();
            let mut unknown = Vec::new();
            for m in metrics {
                match m.value {
                    Some(v) if v.is_finite() => {
                        declared.push(json!({"Name": m.metric, "Unit": unit(&m.metric)}));
                        record.insert(m.metric.clone(), json!(v));
                    }
                    _ => unknown.push(json!(m.metric)),
                }
            }
            record.insert(
                "_aws".to_owned(),
                json!({
                    "Timestamp": timestamp_ms,
                    "CloudWatchMetrics": [{
                        "Namespace": NAMESPACE,
                        "Dimensions": [dimensions],
                        "Metrics": declared,
                    }],
                }),
            );
            record.insert("kind".to_owned(), json!("observables"));
            if !unknown.is_empty() {
                record.insert("unknown".to_owned(), Value::Array(unknown));
            }
            // An empty dimension set is `[[]]`, which the format allows (a metric with none).
            lines.push(Value::Object(record).to_string());
        }
    }
    lines
}

/// Writes the observables once a minute.
#[derive(Debug)]
pub struct Emitter {
    last: Option<Instant>,
}

impl Default for Emitter {
    fn default() -> Self {
        Self::new()
    }
}

impl Emitter {
    /// An emitter that writes at its first call.
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Writes the observables if a minute has passed since the last time; a read that fails is
    /// logged and writes nothing, and is tried again at the next call.
    pub async fn tick(&mut self, pool: &PgPool, now: Instant, timestamp_ms: i64) {
        if self.last.is_some_and(|l| now.duration_since(l) < EVERY) {
            return;
        }
        match read(pool).await {
            Ok(rows) => {
                self.last = Some(now);
                for line in emf_lines(&rows, timestamp_ms) {
                    println!("{line}");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "REQ-ST-6: the observables could not be read; none written this minute")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        metric: &str,
        a: Option<&str>,
        n: Option<&str>,
        d: Option<&str>,
        v: Option<f64>,
    ) -> Observable {
        Observable {
            metric: metric.into(),
            asset: a.map(Into::into),
            network: n.map(Into::into),
            direction: d.map(Into::into),
            value: v,
        }
    }

    #[test]
    fn a_route_metric_is_one_line_with_its_three_dimensions() {
        let lines = emf_lines(
            &[
                row(
                    "oldest_nonfinal_transfer_age_seconds",
                    Some("bUSDC"),
                    Some("polygon"),
                    Some("deposit"),
                    Some(12.5),
                ),
                row(
                    "unknown_legs",
                    Some("bUSDC"),
                    Some("polygon"),
                    Some("deposit"),
                    Some(2.0),
                ),
            ],
            1_000,
        );
        assert_eq!(lines.len(), 1);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["Asset"], "bUSDC");
        assert_eq!(v["Network"], "polygon");
        assert_eq!(v["Direction"], "deposit");
        assert_eq!(v["unknown_legs"], 2.0);
        let m = &v["_aws"]["CloudWatchMetrics"][0];
        assert_eq!(m["Namespace"], "bezent");
        assert_eq!(m["Dimensions"][0], json!(["Asset", "Network", "Direction"]));
        let names: Vec<_> = m["Metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["Name"].clone())
            .collect();
        assert_eq!(
            names,
            [
                json!("oldest_nonfinal_transfer_age_seconds"),
                json!("unknown_legs")
            ]
        );
        assert_eq!(m["Metrics"][0]["Unit"], "Seconds");
        assert_eq!(m["Metrics"][1]["Unit"], "Count");
    }

    #[test]
    fn an_unknown_value_is_named_and_never_written_as_a_number() {
        let lines = emf_lines(
            &[
                row(
                    "observer_lag_estimate_seconds",
                    None,
                    Some("polygon"),
                    None,
                    None,
                ),
                row("open_gaps", None, Some("polygon"), None, Some(0.0)),
            ],
            1,
        );
        assert_eq!(lines.len(), 1);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert!(v.get("observer_lag_estimate_seconds").is_none(), "{v}");
        assert_eq!(v["unknown"], json!(["observer_lag_estimate_seconds"]));
        assert_eq!(v["open_gaps"], 0.0);
        let declared: Vec<_> = v["_aws"]["CloudWatchMetrics"][0]["Metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["Name"].clone())
            .collect();
        assert_eq!(declared, [json!("open_gaps")]);
    }

    #[test]
    fn global_metrics_have_no_dimensions_and_every_line_is_one_json_object() {
        let lines = emf_lines(
            &[
                row("undelivered_notifications", None, None, None, Some(3.0)),
                row(
                    "reserve_equation_outcome",
                    Some("bUSDC"),
                    None,
                    None,
                    Some(1.0),
                ),
                row(
                    "reserve_equation_outcome",
                    Some("bEURC"),
                    None,
                    None,
                    Some(0.0),
                ),
            ],
            1,
        );
        // Two assets are two records (an EMF record holds one value per metric), plus the global one.
        assert_eq!(lines.len(), 3);
        for l in &lines {
            assert!(!l.contains('\n'));
            let v: Value = serde_json::from_str(l).unwrap();
            assert_eq!(v["kind"], "observables");
        }
        let global: Value = lines
            .iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v.get("undelivered_notifications").is_some())
            .unwrap();
        assert_eq!(
            global["_aws"]["CloudWatchMetrics"][0]["Dimensions"][0],
            json!([])
        );
    }

    #[test]
    fn nothing_that_names_a_transfer_a_destination_or_a_requester_is_written() {
        let lines = emf_lines(
            &[row(
                "unknown_legs",
                Some("bUSDC"),
                Some("polygon"),
                Some("deposit"),
                Some(1.0),
            )],
            1,
        );
        for banned in ["reference", "destination", "requester", "secret", "url"] {
            assert!(!lines[0].to_lowercase().contains(banned), "{banned}");
        }
    }
}

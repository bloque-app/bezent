// Copyright (c) 2026 Pagos Cube LLC
// SPDX-License-Identifier: GPL-3.0-or-later
//! Route cut-offs (`REQ-LM-1`, `CND-CutOff`, amendment 0010 A2).
//!
//! A route declares, in `routes.cutoffs`, the windows in which it is closed: an array of
//! `{"weekdays": [1..7], "from": "HH:MM", "to": "HH:MM"}`, ISO weekdays (Monday is 1), in UTC, `to`
//! exclusive and after `from`. A transfer whose next step falls inside one holds `CND-CutOff` and
//! resumes by itself when the window ends. No request is refused for a cut-off.
//!
//! The spec leaves the form of a cut-off to the route table; this is the form the core reads,
//! recorded in the column's comment (migration 0014). A declaration the core cannot read closes the
//! route: a cut-off it cannot understand is never taken as open.

use chrono::{DateTime, Datelike, NaiveTime, Utc};
use serde_json::Value;

/// One closed window.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Window {
    weekdays: Vec<u32>,
    from: NaiveTime,
    to: NaiveTime,
}

fn window(v: &Value) -> Option<Window> {
    let weekdays = v
        .get("weekdays")?
        .as_array()?
        .iter()
        .map(|d| d.as_u64().and_then(|d| u32::try_from(d).ok()))
        .collect::<Option<Vec<u32>>>()?;
    if weekdays.is_empty() || weekdays.iter().any(|d| !(1..=7).contains(d)) {
        return None;
    }
    let time = |field: &str| NaiveTime::parse_from_str(v.get(field)?.as_str()?, "%H:%M").ok();
    let (from, to) = (time("from")?, time("to")?);
    (to > from).then_some(Window { weekdays, from, to })
}

/// Whether the route whose `cutoffs` are `declared` is closed at `now`.
pub fn closed(declared: &Value, now: DateTime<Utc>) -> bool {
    let Some(windows) = declared.as_array() else {
        return true;
    };
    let day = now.weekday().number_from_monday();
    let time = now.time();
    windows.iter().any(|w| match window(w) {
        Some(w) => w.weekdays.contains(&day) && w.from <= time && time < w.to,
        None => true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn cnd_cutoff_route_is_closed_only_inside_a_declared_window() {
        // Weekdays 17:00 to 23:59 closed.
        let declared = json!([{"weekdays": [1, 2, 3, 4, 5], "from": "17:00", "to": "23:59"}]);
        // 2026-10-13 is a Tuesday.
        assert!(!closed(&declared, at("2026-10-13T16:59:00Z")));
        assert!(closed(&declared, at("2026-10-13T17:00:00Z")));
        assert!(!closed(&declared, at("2026-10-13T23:59:00Z")));
        // Saturday is not declared.
        assert!(!closed(&declared, at("2026-10-17T18:00:00Z")));
        // No cut-offs: always open.
        assert!(!closed(&json!([]), at("2026-10-13T18:00:00Z")));
        // A declaration the core cannot read closes the route.
        assert!(closed(
            &json!([{"weekdays": [9], "from": "x"}]),
            at("2026-10-13T18:00:00Z")
        ));
        assert!(closed(&json!({}), at("2026-10-13T18:00:00Z")));
    }
}

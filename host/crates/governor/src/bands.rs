//! The governor's settings and bands. Pure.
//!
//! Plan usage is the higher of the five-hour and weekly windows' percent used. A window whose
//! reset time has passed, or a reading the meter marks stale, counts as unknown: the governor
//! never acts on a number it cannot trust.

use helm_usage::{UsageSnapshot, UsageWindow};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Band {
    /// No usable reading: the governor does nothing.
    Unknown,
    /// Room to spare.
    Surplus,
    /// Getting used up: only the user's own work and replies should start (the run-slot hook).
    Selective,
    /// Close to the limit: running agents are told to wrap up.
    WindDown,
    /// At the limit: running agents are stopped (or only told to wrap up, per `atStop`).
    Stop,
}

impl Band {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Surplus => "surplus",
            Self::Selective => "selective",
            Self::WindDown => "windDown",
            Self::Stop => "stop",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AtStop {
    Stop,
    WrapUp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernorSettings {
    pub enabled: bool,
    /// Percent used at which each band begins; strictly increasing, 1–100.
    pub selective_pct: u32,
    pub wind_down_pct: u32,
    pub stop_pct: u32,
    pub at_stop: AtStop,
}

impl Default for GovernorSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            selective_pct: 60,
            wind_down_pct: 85,
            stop_pct: 95,
            at_stop: AtStop::Stop,
        }
    }
}

fn pct_of(v: &Value) -> Option<u32> {
    let n = v.as_u64()?;
    (1..=100).contains(&n).then_some(n as u32)
}

fn ordered(s: &GovernorSettings) -> bool {
    s.selective_pct < s.wind_down_pct && s.wind_down_pct < s.stop_pct
}

/// Lenient read: each field on its own; thresholds out of order fall back to the defaults.
pub fn read_lenient(v: &Value) -> GovernorSettings {
    let mut s = GovernorSettings::default();
    let Some(o) = v.as_object() else { return s };
    if let Some(b) = o.get("enabled").and_then(Value::as_bool) {
        s.enabled = b;
    }
    let mut t = s.clone();
    if let Some(n) = o.get("selectivePct").and_then(pct_of) {
        t.selective_pct = n;
    }
    if let Some(n) = o.get("windDownPct").and_then(pct_of) {
        t.wind_down_pct = n;
    }
    if let Some(n) = o.get("stopPct").and_then(pct_of) {
        t.stop_pct = n;
    }
    if ordered(&t) {
        s.selective_pct = t.selective_pct;
        s.wind_down_pct = t.wind_down_pct;
        s.stop_pct = t.stop_pct;
    }
    match o.get("atStop").and_then(Value::as_str) {
        Some("stop") => s.at_stop = AtStop::Stop,
        Some("wrapUp") => s.at_stop = AtStop::WrapUp,
        _ => {}
    }
    s
}

/// Strict write: every key checked, or a specific refusal.
pub fn parse_strict(v: &Value) -> Result<GovernorSettings, String> {
    let o = v
        .as_object()
        .ok_or("Governor settings must be an object.")?;
    if let Some(k) = o.keys().find(|k| {
        ![
            "enabled",
            "selectivePct",
            "windDownPct",
            "stopPct",
            "atStop",
        ]
        .contains(&k.as_str())
    }) {
        return Err(format!("The governor has no setting called \"{k}\"."));
    }
    let mut s = GovernorSettings::default();
    if let Some(v) = o.get("enabled") {
        s.enabled = v.as_bool().ok_or("enabled must be true or false.")?;
    }
    let pct = |k: &str, cur: u32| -> Result<u32, String> {
        match o.get(k) {
            None => Ok(cur),
            Some(v) => pct_of(v).ok_or(format!("{k} must be a whole percent from 1 to 100.")),
        }
    };
    s.selective_pct = pct("selectivePct", s.selective_pct)?;
    s.wind_down_pct = pct("windDownPct", s.wind_down_pct)?;
    s.stop_pct = pct("stopPct", s.stop_pct)?;
    if !ordered(&s) {
        return Err("The thresholds must rise: selective, then wind down, then stop.".into());
    }
    if let Some(v) = o.get("atStop") {
        s.at_stop = match v.as_str() {
            Some("stop") => AtStop::Stop,
            Some("wrapUp") => AtStop::WrapUp,
            _ => return Err("atStop must be \"stop\" or \"wrapUp\".".into()),
        };
    }
    Ok(s)
}

fn usable(w: &Option<UsageWindow>, now: i64) -> Option<f64> {
    let w = w.as_ref()?;
    if w.resets_at.is_some_and(|r| r <= now) {
        return None;
    }
    w.pct.filter(|p| p.is_finite())
}

/// The percent used the governor acts on, if it has one it can trust.
pub fn used_pct(snap: &UsageSnapshot, now: i64) -> Option<f64> {
    let r = snap.reading.as_ref()?;
    if r.stale {
        return None;
    }
    match (usable(&r.five_hour, now), usable(&r.weekly, now)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

pub fn band_of(used: Option<f64>, s: &GovernorSettings) -> Band {
    let Some(u) = used else { return Band::Unknown };
    if u >= s.stop_pct as f64 {
        Band::Stop
    } else if u >= s.wind_down_pct as f64 {
        Band::WindDown
    } else if u >= s.selective_pct as f64 {
        Band::Selective
    } else {
        Band::Surplus
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(
        five: Option<(f64, Option<i64>)>,
        weekly: Option<(f64, Option<i64>)>,
        stale: bool,
    ) -> UsageSnapshot {
        let w = |x: Option<(f64, Option<i64>)>| {
            x.map(|(p, r)| json!({"pct": p, "resetsAt": r, "at": 0, "source": "control"}))
        };
        serde_json::from_value(json!({
            "reading": {
                "fiveHour": w(five),
                "weekly": w(weekly),
                "perModel": [],
                "source": "control",
                "fetchedAt": 0,
                "stale": stale
            },
            "readiness": {"state": "ready", "reason": "ok"},
            "refreshing": false
        }))
        .unwrap()
    }

    #[test]
    fn used_is_the_higher_trusted_window() {
        assert_eq!(
            used_pct(&snap(Some((40.0, None)), Some((70.0, None)), false), 0),
            Some(70.0)
        );
        assert_eq!(
            used_pct(&snap(Some((40.0, None)), None, false), 0),
            Some(40.0)
        );
        // An expired window is ignored.
        assert_eq!(
            used_pct(&snap(Some((99.0, Some(10))), Some((20.0, None)), false), 10),
            Some(20.0)
        );
        assert_eq!(
            used_pct(&snap(Some((99.0, None)), None, true), 0),
            None,
            "stale"
        );
    }

    #[test]
    fn bands_begin_at_their_thresholds() {
        let s = GovernorSettings::default();
        assert_eq!(band_of(None, &s), Band::Unknown);
        assert_eq!(band_of(Some(59.9), &s), Band::Surplus);
        assert_eq!(band_of(Some(60.0), &s), Band::Selective);
        assert_eq!(band_of(Some(85.0), &s), Band::WindDown);
        assert_eq!(band_of(Some(94.9), &s), Band::WindDown);
        assert_eq!(band_of(Some(95.0), &s), Band::Stop);
        assert_eq!(band_of(Some(120.0), &s), Band::Stop);
    }

    #[test]
    fn strict_and_lenient() {
        assert!(parse_strict(&json!({"stopPct": 0}))
            .unwrap_err()
            .contains("1 to 100"));
        assert!(parse_strict(&json!({"windDownPct": 96}))
            .unwrap_err()
            .contains("rise"));
        assert!(parse_strict(&json!({"nope": 1}))
            .unwrap_err()
            .contains("nope"));
        assert!(parse_strict(&json!({"atStop": "panic"}))
            .unwrap_err()
            .contains("atStop"));
        let s = parse_strict(&json!({"enabled": true, "selectivePct": 10, "windDownPct": 20, "stopPct": 30, "atStop": "wrapUp"})).unwrap();
        assert_eq!(
            (s.selective_pct, s.wind_down_pct, s.stop_pct, s.at_stop),
            (10, 20, 30, AtStop::WrapUp)
        );
        let l = read_lenient(&json!({"enabled": true, "windDownPct": 99}));
        assert!(l.enabled);
        assert_eq!(l.wind_down_pct, 85, "out of order falls back");
    }
}

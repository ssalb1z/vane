//! Shared time primitives: dispatch granularity and the peak-shaving window.

use std::str::FromStr;

use chrono::NaiveTime;
use serde::{Deserialize, Serialize};

/// A small, clap/serde-friendly parse error.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

/// Time resolution of a simulation run. `1h` matches most IESO series; `15m`
/// is available for finer runs (see DESIGN.md §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Granularity {
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "15m")]
    FifteenMin,
}

impl Granularity {
    /// Length of one step in minutes.
    pub fn minutes(self) -> u32 {
        match self {
            Granularity::Hour => 60,
            Granularity::FifteenMin => 15,
        }
    }

    /// Δt in hours — the factor that turns a power (kW) into energy (kWh) per step.
    pub fn dt_hours(self) -> f64 {
        f64::from(self.minutes()) / 60.0
    }

    /// Number of steps in a full day.
    pub fn steps_per_day(self) -> usize {
        (24 * 60 / self.minutes()) as usize
    }
}

impl FromStr for Granularity {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1h" | "h" | "hour" | "hourly" | "60m" => Ok(Granularity::Hour),
            "15m" | "15min" | "15" => Ok(Granularity::FifteenMin),
            other => Err(ParseError(format!(
                "unknown granularity {other:?} (expected `1h` or `15m`)"
            ))),
        }
    }
}

/// A peak-shaving window, e.g. `17:00-20:00`. `end` is exclusive of the final
/// instant but inclusive of the step that begins at `end - Δt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl Window {
    /// The step-start times covered by this window at the given granularity.
    pub fn steps(&self, g: Granularity) -> Vec<NaiveTime> {
        let step = chrono::Duration::minutes(i64::from(g.minutes()));
        let mut out = Vec::new();
        let mut t = self.start;
        while t < self.end {
            out.push(t);
            t += step;
        }
        out
    }
}

impl FromStr for Window {
    type Err = ParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| ParseError(format!("window {s:?} must look like `17:00-20:00`")))?;
        let parse = |x: &str| {
            NaiveTime::parse_from_str(x.trim(), "%H:%M")
                .map_err(|e| ParseError(format!("bad time {x:?}: {e}")))
        };
        let start = parse(a)?;
        let end = parse(b)?;
        if end <= start {
            return Err(ParseError(format!(
                "window end {end} must be after start {start}"
            )));
        }
        Ok(Window { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granularity_dt() {
        assert_eq!(Granularity::Hour.dt_hours(), 1.0);
        assert_eq!(Granularity::FifteenMin.dt_hours(), 0.25);
        assert_eq!(Granularity::FifteenMin.steps_per_day(), 96);
    }

    #[test]
    fn window_steps_hourly() {
        let w: Window = "17:00-20:00".parse().unwrap();
        assert_eq!(w.steps(Granularity::Hour).len(), 3);
        assert_eq!(w.steps(Granularity::FifteenMin).len(), 12);
    }

    #[test]
    fn window_rejects_backwards() {
        assert!("20:00-17:00".parse::<Window>().is_err());
        assert!("nonsense".parse::<Window>().is_err());
    }
}

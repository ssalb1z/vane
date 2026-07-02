//! `vane-forecast` — predict neighborhood-driving demand and solar for the
//! dispatch day (DESIGN.md §7). Forecasting is *subordinate* to the optimizer:
//! it produces the `demand_mw` and `ghi_w_m2` the planner assumes; the plan is
//! then scored against actuals (that gap is the "forecast error").
//!
//! Two stages:
//! * [`baseline`] — pure-Rust climatology: the per-hour mean of the training
//!   days. No weather awareness, no Python. The default.
//! * [`python`] — the thin Python ML layer: hands training history + the target
//!   day's temperature to a Python subprocess over a JSON contract and reads a
//!   forecast back. The reference model (a per-hour temperature regression) is
//!   stdlib-only; swap in LightGBM without touching this boundary.

use std::io::Write;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use vane_data::{DaySeries, Scenario};
use vane_model::Granularity;

/// A demand + solar + temperature forecast for one day, with optional demand
/// prediction intervals. Temperature is carried because it sets how much cheap
/// thermostat curtailment the planner believes is available — the channel
/// through which forecast skill becomes dispatch value (DESIGN.md §7).
#[derive(Debug, Clone)]
pub struct Forecast {
    pub demand_mw: DaySeries,
    pub ghi_w_m2: DaySeries,
    pub temperature_c: DaySeries,
    pub demand_lo: Option<Vec<f64>>,
    pub demand_hi: Option<Vec<f64>>,
}

impl Forecast {
    /// Overlay this forecast's demand, solar, and temperature onto `actual`,
    /// keeping actual price/emissions. The planner optimizes on the result;
    /// scoring uses `actual`. (We forecast load/generation/weather, not prices.)
    pub fn as_scenario(&self, actual: &Scenario) -> Scenario {
        Scenario {
            date: actual.date,
            granularity: actual.granularity,
            demand_mw: self.demand_mw.clone(),
            price_per_mwh: actual.price_per_mwh.clone(),
            emissions_g_per_kwh: actual.emissions_g_per_kwh.clone(),
            temperature_c: self.temperature_c.clone(),
            ghi_w_m2: self.ghi_w_m2.clone(),
        }
    }
}

fn per_hour_mean(
    train: &[Scenario],
    pick: impl Fn(&Scenario) -> &DaySeries,
    g: Granularity,
) -> Vec<f64> {
    let steps = g.steps_per_day();
    let mut sums = vec![0.0; steps];
    let mut counts = vec![0usize; steps];
    for s in train {
        let series = pick(s);
        for i in 0..steps.min(series.values.len()) {
            sums[i] += series.values[i];
            counts[i] += 1;
        }
    }
    (0..steps)
        .map(|i| if counts[i] > 0 { sums[i] / counts[i] as f64 } else { 0.0 })
        .collect()
}

/// Stage 0: climatology — the per-hour mean of the training days.
pub fn baseline(
    train: &[Scenario],
    date: chrono::NaiveDate,
    g: Granularity,
) -> anyhow::Result<Forecast> {
    if train.is_empty() {
        anyhow::bail!("baseline forecast needs at least one training day");
    }
    let demand = per_hour_mean(train, |s| &s.demand_mw, g);
    let ghi = per_hour_mean(train, |s| &s.ghi_w_m2, g);
    // Climatology has no weather insight: it assumes an average-temperature day,
    // so on a hot day it under-estimates available thermostat curtailment.
    let temp = per_hour_mean(train, |s| &s.temperature_c, g);
    Ok(Forecast {
        demand_mw: DaySeries::new(date, g, demand)?,
        ghi_w_m2: DaySeries::new(date, g, ghi)?,
        temperature_c: DaySeries::new(date, g, temp)?,
        demand_lo: None,
        demand_hi: None,
    })
}

// --- Python bridge (Stage 1) ---

#[derive(Serialize)]
struct TrainDay {
    demand: Vec<f64>,
    temp: Vec<f64>,
    ghi: Vec<f64>,
}

#[derive(Serialize)]
struct PredictInput {
    temp: Vec<f64>,
}

#[derive(Serialize)]
struct Request {
    train: Vec<TrainDay>,
    predict: PredictInput,
}

#[derive(Deserialize)]
struct Response {
    demand: Vec<f64>,
    ghi: Vec<f64>,
    demand_lo: Option<Vec<f64>>,
    demand_hi: Option<Vec<f64>>,
}

/// Stage 1: forecast via a Python subprocess. `cmd` is the argv to run (e.g.
/// `["python3", "python/vane_forecast/predict.py"]`); it receives the JSON
/// [`Request`] on stdin and must print a JSON response on stdout.
/// `target_temp` is the (assumed-known) temperature forecast for the day.
pub fn python(
    train: &[Scenario],
    target_temp: &DaySeries,
    date: chrono::NaiveDate,
    g: Granularity,
    cmd: &[String],
) -> anyhow::Result<Forecast> {
    if cmd.is_empty() {
        anyhow::bail!("empty python command");
    }
    let req = Request {
        train: train
            .iter()
            .map(|s| TrainDay {
                demand: s.demand_mw.values.clone(),
                temp: s.temperature_c.values.clone(),
                ghi: s.ghi_w_m2.values.clone(),
            })
            .collect(),
        predict: PredictInput {
            temp: target_temp.values.clone(),
        },
    };
    let payload = serde_json::to_vec(&req)?;

    let mut child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning forecaster {:?}: {e}", cmd))?;

    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(&payload)?;

    let out = child.wait_with_output()?;
    if !out.status.success() {
        anyhow::bail!(
            "forecaster {:?} failed: {}",
            cmd,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let resp: Response = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow::anyhow!("parsing forecaster output: {e}"))?;

    Ok(Forecast {
        demand_mw: DaySeries::new(date, g, resp.demand)?,
        ghi_w_m2: DaySeries::new(date, g, resp.ghi)?,
        // The weather forecast (temperature) is the model's *input* and is
        // assumed skillful, so planning uses it directly — this is why the
        // temperature-aware model provisions cheap curtailment correctly.
        temperature_c: target_temp.clone(),
        demand_lo: resp.demand_lo,
        demand_hi: resp.demand_hi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn train_days(n: i64) -> Vec<Scenario> {
        let base = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        (1..=n)
            .map(|d| {
                vane_data::synthetic::scenario(
                    base - chrono::Duration::days(d),
                    Granularity::Hour,
                )
            })
            .collect()
    }

    #[test]
    fn baseline_is_hourly_mean() {
        let train = train_days(7);
        let date = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let f = baseline(&train, date, Granularity::Hour).unwrap();
        assert_eq!(f.demand_mw.values.len(), 24);
        let peak = (0..24)
            .max_by(|&a, &b| f.demand_mw.at(a).total_cmp(&f.demand_mw.at(b)))
            .unwrap();
        assert!((15..=19).contains(&peak));
    }
}

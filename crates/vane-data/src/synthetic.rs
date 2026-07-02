//! A deterministic synthetic summer-peak day, so the pipeline runs with no
//! network. Shapes are stylized but plausible for Ontario in July: a
//! late-afternoon AC-driven demand peak, price tracking demand, average
//! emissions rising as gas comes on the margin at peak, a diurnal temperature
//! curve, and a bell-shaped irradiance profile.
//!
//! It is NOT real data — `vane simulate` labels synthetic runs accordingly.

use std::f64::consts::PI;

use chrono::{Datelike, NaiveDate};
use vane_model::Granularity;

use crate::series::{DaySeries, Scenario};

/// Cheap deterministic jitter in `[-1, 1]` from an integer key (hash-ish LCG).
fn wiggle(seed: u64, step: usize) -> f64 {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(step as u64 + 1);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    // map to [-1, 1]
    (x as f64 / u64::MAX as f64) * 2.0 - 1.0
}

/// Build a synthetic [`Scenario`] for `date` at `g`. Deterministic in `date`.
pub fn scenario(date: NaiveDate, g: Granularity) -> Scenario {
    let steps = g.steps_per_day();
    let dt_h = g.dt_hours();
    let seed = date.num_days_from_ce() as u64;

    // Per-day heat anomaly in [-1, 1]: hotter days run warmer AND draw more
    // evening AC load, so demand co-varies with temperature across days. This is
    // the signal a weather-aware forecaster can learn that climatology cannot.
    let heat = wiggle(seed ^ 0xABCDEF, 0);

    let mut demand = Vec::with_capacity(steps);
    let mut price = Vec::with_capacity(steps);
    let mut emis = Vec::with_capacity(steps);
    let mut temp = Vec::with_capacity(steps);
    let mut ghi = Vec::with_capacity(steps);

    for i in 0..steps {
        let hour = i as f64 * dt_h; // 0.0 .. 24.0

        // Temperature: diurnal, min ~05:00, max ~16:00, shifted by the day's heat.
        let t_c = 24.0 + 4.0 * heat
            + 8.0 * (2.0 * PI * (hour - 16.0) / 24.0).cos() * 0.5
            + 8.0 * (2.0 * PI * (hour - 9.0) / 24.0).sin() * 0.5
            + 0.6 * wiggle(seed ^ 0x7, i);

        // Irradiance: zero at night, bell peaking at solar noon (~13:00).
        let ghi_w = if (6.0..=20.0).contains(&hour) {
            (950.0 * (PI * (hour - 6.0) / 14.0).sin()).max(0.0)
        } else {
            0.0
        };

        // Demand (MW): baseload + morning shoulder + strong late-afternoon AC peak.
        let morning = 1800.0 * (-((hour - 8.0).powi(2)) / 6.0).exp();
        let evening = 5200.0 * (1.0 + 0.12 * heat) * (-((hour - 17.5).powi(2)) / 9.0).exp();
        let d = 13500.0 + morning + evening + 250.0 * wiggle(seed ^ 0x11, i);

        // Price ($/MWh): tracks demand above a floor, mild spikes at peak.
        let p = (12.0 + 0.006 * (d - 13500.0) + 4.0 * wiggle(seed ^ 0x23, i)).max(0.0);

        // Emissions (gCO2e/kWh): low baseline (nuclear/hydro), rises with gas on margin.
        let e = (28.0 + 0.011 * (d - 13500.0) + 2.0 * wiggle(seed ^ 0x31, i)).clamp(15.0, 160.0);

        demand.push(d);
        price.push(p);
        emis.push(e);
        temp.push(t_c);
        ghi.push(ghi_w);
    }

    Scenario {
        date,
        granularity: g,
        demand_mw: DaySeries::new(date, g, demand).unwrap(),
        price_per_mwh: DaySeries::new(date, g, price).unwrap(),
        emissions_g_per_kwh: DaySeries::new(date, g, emis).unwrap(),
        temperature_c: DaySeries::new(date, g, temp).unwrap(),
        ghi_w_m2: DaySeries::new(date, g, ghi).unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_sane() {
        let d = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let s = scenario(d, Granularity::Hour);
        assert_eq!(s.demand_mw.values.len(), 24);

        // Night irradiance is zero; midday is high.
        assert_eq!(s.ghi_w_m2.at(2), 0.0);
        assert!(s.ghi_w_m2.at(13) > 500.0);

        // Peak demand lands in the late afternoon (HE17-19), not at night.
        let peak_step = (0..24)
            .max_by(|&a, &b| s.demand_mw.at(a).total_cmp(&s.demand_mw.at(b)))
            .unwrap();
        assert!((15..=19).contains(&peak_step), "peak at step {peak_step}");

        // Prices and emissions are non-negative and finite.
        assert!(s.price_per_mwh.values.iter().all(|v| v.is_finite() && *v >= 0.0));
        assert!(s.emissions_g_per_kwh.values.iter().all(|v| (15.0..=160.0).contains(v)));
    }

    #[test]
    fn deterministic() {
        let d = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        assert_eq!(
            scenario(d, Granularity::Hour).demand_mw,
            scenario(d, Granularity::Hour).demand_mw
        );
    }
}

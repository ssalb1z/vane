//! Average grid emissions intensity.
//!
//! Ontario does NOT publish marginal intensity, and free marginal series were
//! discontinued (DESIGN.md §9). For real (`--source ieso`) runs we approximate
//! *average* intensity from the demand shape with a gas-on-margin heuristic:
//! a low nuclear/hydro baseline that rises as gas comes on at peak. This is a
//! stand-in for The Atmospheric Fund's hourly factors or an IESO fuel-mix
//! computation, and is always labeled "average / heuristic" in output.

use crate::series::DaySeries;

/// Heuristic average intensity (gCO2e/kWh) from a demand series: `28 + 0.011·
/// (demand − mean)`, clamped to `[15, 160]`. Shapes emissions to rise with load.
pub fn heuristic_intensity(demand: &DaySeries) -> DaySeries {
    let mean = demand.mean();
    let values = demand
        .values
        .iter()
        .map(|d| (28.0 + 0.011 * (d - mean)).clamp(15.0, 160.0))
        .collect();
    DaySeries {
        date: demand.date,
        granularity: demand.granularity,
        values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic;
    use chrono::NaiveDate;
    use vane_model::Granularity;

    #[test]
    fn intensity_rises_with_demand() {
        let d = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let s = synthetic::scenario(d, Granularity::Hour);
        let e = heuristic_intensity(&s.demand_mw);
        let peak = (0..24).max_by(|&a, &b| s.demand_mw.at(a).total_cmp(&s.demand_mw.at(b))).unwrap();
        let trough = (0..24).min_by(|&a, &b| s.demand_mw.at(a).total_cmp(&s.demand_mw.at(b))).unwrap();
        assert!(e.at(peak) > e.at(trough));
        assert!(e.values.iter().all(|v| (15.0..=160.0).contains(v)));
    }
}

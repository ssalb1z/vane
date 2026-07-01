//! `vane-sim` — medium-fidelity asset physics, and the bridge from a raw
//! [`Scenario`] to the [`SimInputs`] the MILP consumes.
//!
//! Modeling choices (DESIGN.md §5):
//! * **Baseline load** — the neighborhood's own demand is the *shape* of the
//!   real IESO system/zonal demand, normalized and scaled to a per-home average,
//!   times the number of homes. Solar generation is netted off.
//! * **Solar** — exogenous: `min(inverter_ac, capacity_dc · derate · GHI/1000)`.
//! * **Thermostats** — aggregated into one curtailment resource whose available
//!   kW scales with outdoor temperature (more AC running when hotter). A linear
//!   snapback adds a fraction of curtailed load back the following step.
//! * **Battery / EV** — kept as individual instances for the optimizer.
//!
//! The optimization **horizon is the full day**; the peak-shave **target applies
//! only within the window**. This gives the optimizer room to pre-charge
//! batteries and shift EV load out of the peak.

use chrono::Timelike;
use vane_data::Scenario;
use vane_model::time::Window;
use vane_model::{Asset, Granularity, Neighborhood};

/// Average per-home electrical load (kW) used to scale the system demand shape
/// down to this neighborhood. Residential Ontario average is ~1.2–1.3 kW.
pub const AVG_HOME_KW: f64 = 1.3;

/// Everything the MILP needs for one dispatch run. Powers are kW, energy kWh,
/// prices $/kWh; index is the step-of-day over the full-day horizon.
#[derive(Debug, Clone)]
pub struct SimInputs {
    pub granularity: Granularity,
    pub dt_h: f64,
    pub steps: usize,
    /// Steps (of the full-day horizon) where the peak-shave target is enforced.
    pub window_steps: Vec<usize>,
    pub price_per_kwh: Vec<f64>,
    pub emissions_g_per_kwh: Vec<f64>,
    /// Neighborhood net load without any dispatch (baseline demand − solar), kW.
    pub baseline_net_kw: Vec<f64>,
    /// Aggregate solar generation, kW (already reflected in `baseline_net_kw`).
    pub solar_kw: Vec<f64>,
    pub thermostats: ThermostatAgg,
    pub batteries: Vec<BatterySpec>,
    pub evs: Vec<EvSpec>,
}

/// Aggregated thermostat curtailment resource.
#[derive(Debug, Clone)]
pub struct ThermostatAgg {
    pub count: usize,
    /// Max curtailable kW at each step (temperature-scaled).
    pub avail_kw: Vec<f64>,
    /// Fraction of curtailed load that rebounds one step later (snapback).
    pub snapback_ratio: f64,
    /// Cost weight on curtailment energy (discomfort proxy).
    pub discomfort_weight: f64,
}

#[derive(Debug, Clone)]
pub struct BatterySpec {
    pub capacity_kwh: f64,
    pub power_kw: f64,
    pub charge_eff: f64,
    pub discharge_eff: f64,
    pub soc_init_kwh: f64,
    pub soc_min_kwh: f64,
    pub cycle_cost_per_kwh: f64,
}

#[derive(Debug, Clone)]
pub struct EvSpec {
    pub power_kw: f64,
    pub required_kwh: f64,
    /// Per-step availability: `true` where the vehicle is plugged in.
    pub available: Vec<bool>,
}

/// Cooling-availability multiplier: 0 below ~18 °C, rising to a cap ~1.3 at
/// ~34 °C. Warmer outdoor temps mean more AC runtime available to shed.
fn cooling_factor(temp_c: f64) -> f64 {
    ((temp_c - 18.0) / 12.0).clamp(0.0, 1.3)
}

fn step_of(t: chrono::NaiveTime, g: Granularity) -> usize {
    ((t.hour() * 60 + t.minute()) / g.minutes()) as usize
}

/// Build [`SimInputs`] for a dispatch run. `scenario` is re-sampled to `g`.
pub fn build(
    neighborhood: &Neighborhood,
    scenario: &Scenario,
    window: Window,
    g: Granularity,
) -> anyhow::Result<SimInputs> {
    let scenario = scenario.to_granularity(g);
    let steps = g.steps_per_day();
    let dt_h = g.dt_hours();

    // --- Baseline neighborhood load from the system demand shape ---
    let sys_mean = scenario.demand_mw.mean();
    let homes = neighborhood.homes.len() as f64;
    let baseline_demand_kw: Vec<f64> = (0..steps)
        .map(|i| (scenario.demand_mw.at(i) / sys_mean) * AVG_HOME_KW * homes)
        .collect();

    // --- Aggregate solar generation ---
    let mut solar_kw = vec![0.0; steps];
    for home in &neighborhood.homes {
        for asset in &home.assets {
            if let Asset::Solar(pv) = asset {
                for i in 0..steps {
                    let g_ratio = scenario.ghi_w_m2.at(i) / 1000.0;
                    let dc = pv.capacity_kw_dc * pv.derate * g_ratio;
                    solar_kw[i] += dc.min(pv.inverter_kw_ac);
                }
            }
        }
    }

    let baseline_net_kw: Vec<f64> = (0..steps)
        .map(|i| baseline_demand_kw[i] - solar_kw[i])
        .collect();

    // --- Thermostat aggregate ---
    let mut th_count = 0usize;
    let mut nameplate_kw = 0.0;
    let mut snapback_kw = 0.0;
    let mut discomfort_weight = 1.0;
    for home in &neighborhood.homes {
        for asset in &home.assets {
            if let Asset::Thermostat(t) = asset {
                th_count += 1;
                nameplate_kw += t.avail_reduction_kw;
                snapback_kw += t.snapback_kw;
                discomfort_weight = t.discomfort_weight;
            }
        }
    }
    let avail_kw: Vec<f64> = (0..steps)
        .map(|i| nameplate_kw * cooling_factor(scenario.temperature_c.at(i)))
        .collect();
    let snapback_ratio = if nameplate_kw > 0.0 {
        (snapback_kw / nameplate_kw).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // --- Batteries & EVs ---
    let mut batteries = Vec::new();
    let mut evs = Vec::new();
    for home in &neighborhood.homes {
        for asset in &home.assets {
            match asset {
                Asset::Battery(b) => {
                    let eff = b.roundtrip_eff.sqrt();
                    batteries.push(BatterySpec {
                        capacity_kwh: b.capacity_kwh,
                        power_kw: b.power_kw,
                        charge_eff: eff,
                        discharge_eff: eff,
                        soc_init_kwh: b.soc_init_kwh,
                        soc_min_kwh: b.soc_min_kwh,
                        cycle_cost_per_kwh: b.cycle_cost_per_kwh,
                    });
                }
                Asset::Ev(e) => {
                    let plugin = step_of(e.plugin, g);
                    let available: Vec<bool> = (0..steps).map(|i| i >= plugin).collect();
                    evs.push(EvSpec {
                        power_kw: e.power_kw,
                        required_kwh: e.required_kwh,
                        available,
                    });
                }
                _ => {}
            }
        }
    }

    let window_steps: Vec<usize> = window
        .steps(g)
        .into_iter()
        .map(|t| step_of(t, g))
        .collect();

    let price_per_kwh: Vec<f64> = (0..steps)
        .map(|i| scenario.price_per_mwh.at(i) / 1000.0)
        .collect();
    let emissions_g_per_kwh: Vec<f64> =
        (0..steps).map(|i| scenario.emissions_g_per_kwh.at(i)).collect();

    Ok(SimInputs {
        granularity: g,
        dt_h,
        steps,
        window_steps,
        price_per_kwh,
        emissions_g_per_kwh,
        baseline_net_kw,
        solar_kw,
        thermostats: ThermostatAgg {
            count: th_count,
            avail_kw,
            snapback_ratio,
            discomfort_weight,
        },
        batteries,
        evs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use vane_model::NeighborhoodSpec;

    fn fixture() -> (Neighborhood, Scenario) {
        let toml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/oakville-demo.toml"
        ))
        .unwrap();
        let spec = NeighborhoodSpec::from_toml_str(&toml).unwrap();
        let n = vane_model::neighborhood::generate(&spec, 42).unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let s = vane_data::synthetic::scenario(date, Granularity::Hour);
        (n, s)
    }

    #[test]
    fn builds_inputs() {
        let (n, s) = fixture();
        let w: Window = "17:00-20:00".parse().unwrap();
        let si = build(&n, &s, w, Granularity::Hour).unwrap();

        assert_eq!(si.steps, 24);
        assert_eq!(si.window_steps, vec![17, 18, 19]);
        assert!(!si.batteries.is_empty());
        assert!(si.thermostats.count > 0);

        // Baseline load is positive and peaks in the afternoon/evening.
        assert!(si.baseline_net_kw.iter().all(|v| v.is_finite()));
        let peak = (0..24)
            .max_by(|&a, &b| si.baseline_net_kw[a].total_cmp(&si.baseline_net_kw[b]))
            .unwrap();
        assert!((15..=20).contains(&peak), "baseline peak at {peak}");

        // Thermostat curtailment is available at the hot evening peak, ~0 at night.
        assert!(si.thermostats.avail_kw[18] > 0.0);
        assert_eq!(si.thermostats.avail_kw[3], 0.0);

        // Solar contributes midday but ~nothing during the evening window.
        assert!(si.solar_kw[13] > 0.0);
    }
}

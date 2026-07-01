//! `vane-optimize` — the MILP dispatch core (DESIGN.md §6).
//!
//! Minimize energy cost + discomfort + battery cycling + target shortfall,
//! subject to battery SoC dynamics, EV energy-by-deadline, thermostat
//! curtailment limits with snapback, and a *soft* peak-shave target over the
//! window. The target is soft (a penalized shortfall) so the problem is always
//! feasible and the scorecard can report achieved-vs-target honestly.
//!
//! Horizon = the full day; the target constraint binds only on window steps.
//! Batteries carry a binary charge/discharge lock, which is why this is a MILP
//! rather than an LP: under negative prices a cost-only deterrent to
//! simultaneous charge+discharge would not hold.

use good_lp::{constraint, variable, variables, Expression, Solution, SolverModel, Variable};
use vane_sim::SimInputs;

/// Penalty ($/kWh of unmet target) — large enough to dominate real costs so the
/// optimizer meets the target whenever physically possible.
const SHORTFALL_PENALTY: f64 = 1000.0;

/// The optimized dispatch plan plus the counterfactual baseline it's scored
/// against. All power vectors are indexed by step-of-day; sign convention for
/// `battery_net_kw` is positive = net discharging (load reduction).
#[derive(Debug, Clone)]
pub struct DispatchSchedule {
    pub steps: usize,
    pub dt_h: f64,
    pub window_steps: Vec<usize>,
    pub target_kw: f64,
    pub baseline_grid_kw: Vec<f64>,
    pub optimized_grid_kw: Vec<f64>,
    pub reduction_kw: Vec<f64>,
    pub shortfall_kw: Vec<f64>,
    pub curtail_kw: Vec<f64>,
    pub battery_net_kw: Vec<f64>,
    pub ev_charge_kw: Vec<f64>,
    pub naive_ev_kw: Vec<f64>,
    pub objective: f64,
}

impl DispatchSchedule {
    /// Mean reduction achieved across the window (kW).
    pub fn achieved_mean_kw(&self) -> f64 {
        if self.window_steps.is_empty() {
            return 0.0;
        }
        let sum: f64 = self.window_steps.iter().map(|&t| self.reduction_kw[t]).sum();
        sum / self.window_steps.len() as f64
    }

    /// Worst (minimum) reduction across the window (kW).
    pub fn achieved_min_kw(&self) -> f64 {
        self.window_steps
            .iter()
            .map(|&t| self.reduction_kw[t])
            .fold(f64::INFINITY, f64::min)
    }
}

/// Naive counterfactual EV charging: charge at full power from plug-in until the
/// required energy is delivered. This is the load the optimizer shifts away from.
fn naive_ev_profile(si: &SimInputs) -> Vec<f64> {
    let mut out = vec![0.0; si.steps];
    for ev in &si.evs {
        let mut remaining = ev.required_kwh;
        for t in 0..si.steps {
            if remaining <= 0.0 {
                break;
            }
            if ev.available[t] {
                let deliverable = ev.power_kw.min(remaining / si.dt_h);
                out[t] += deliverable;
                remaining -= deliverable * si.dt_h;
            }
        }
    }
    out
}

/// Solve the dispatch MILP for a peak-shave `target_kw`.
pub fn optimize(si: &SimInputs, target_kw: f64) -> anyhow::Result<DispatchSchedule> {
    let n = si.steps;
    let dt = si.dt_h;
    let naive_ev = naive_ev_profile(si);

    let mut vars = variables!();

    // Battery variables: charge, discharge, soc, and the charge/discharge lock.
    let nb = si.batteries.len();
    let mut pch: Vec<Vec<Variable>> = vec![vec![]; nb];
    let mut pdis: Vec<Vec<Variable>> = vec![vec![]; nb];
    let mut soc: Vec<Vec<Variable>> = vec![vec![]; nb];
    let mut ylock: Vec<Vec<Variable>> = vec![vec![]; nb];
    for (b, spec) in si.batteries.iter().enumerate() {
        for _ in 0..n {
            pch[b].push(vars.add(variable().min(0.0).max(spec.power_kw)));
            pdis[b].push(vars.add(variable().min(0.0).max(spec.power_kw)));
            soc[b].push(vars.add(variable().min(spec.soc_min_kwh).max(spec.capacity_kwh)));
            ylock[b].push(vars.add(variable().binary()));
        }
    }

    // EV charging (0 when unplugged).
    let nv = si.evs.len();
    let mut pev: Vec<Vec<Variable>> = vec![vec![]; nv];
    for (v, ev) in si.evs.iter().enumerate() {
        for t in 0..n {
            let cap = if ev.available[t] { ev.power_kw } else { 0.0 };
            pev[v].push(vars.add(variable().min(0.0).max(cap)));
        }
    }

    // Thermostat curtailment, capped by temperature-scaled availability.
    let cur: Vec<Variable> = (0..n)
        .map(|t| vars.add(variable().min(0.0).max(si.thermostats.avail_kw[t])))
        .collect();

    // Target shortfall on window steps.
    let sf: Vec<Variable> = si
        .window_steps
        .iter()
        .map(|_| vars.add(variable().min(0.0)))
        .collect();

    let sb = si.thermostats.snapback_ratio;

    // --- Objective ---
    let mut obj = Expression::from(0.0);
    for t in 0..n {
        let mut grid = Expression::from(si.baseline_net_kw[t]);
        for b in 0..nb {
            grid += pch[b][t];
            grid -= pdis[b][t];
        }
        for v in 0..nv {
            grid += pev[v][t];
        }
        grid -= cur[t];
        if t > 0 {
            grid += sb * cur[t - 1]; // snapback rebound
        }
        obj += (si.price_per_kwh[t] * dt) * grid;
        obj += (si.thermostats.discomfort_weight * dt) * cur[t];
    }
    for b in 0..nb {
        let cc = si.batteries[b].cycle_cost_per_kwh * dt;
        for t in 0..n {
            obj += cc * pch[b][t];
            obj += cc * pdis[b][t];
        }
    }
    for &s in &sf {
        obj += (SHORTFALL_PENALTY * dt) * s;
    }

    let mut model = vars.minimise(obj).using(good_lp::highs);

    // --- Battery constraints ---
    for (b, spec) in si.batteries.iter().enumerate() {
        for t in 0..n {
            let prev = if t == 0 {
                Expression::from(spec.soc_init_kwh)
            } else {
                Expression::from(soc[b][t - 1])
            };
            let flow = (spec.charge_eff * dt) * pch[b][t] - (dt / spec.discharge_eff) * pdis[b][t];
            model = model.with(constraint!(soc[b][t] == prev + flow));
            model = model.with(constraint!(pch[b][t] <= spec.power_kw * ylock[b][t]));
            model = model.with(constraint!(pdis[b][t] <= spec.power_kw * (1 - ylock[b][t])));
        }
    }

    // --- EV energy delivered by end of horizon ---
    for (v, ev) in si.evs.iter().enumerate() {
        let mut delivered = Expression::from(0.0);
        for t in 0..n {
            delivered += dt * pev[v][t];
        }
        model = model.with(constraint!(delivered >= ev.required_kwh));
    }

    // --- Target (soft) on window steps ---
    for (wi, &t) in si.window_steps.iter().enumerate() {
        let mut red = Expression::from(naive_ev[t]);
        for v in 0..nv {
            red -= pev[v][t];
        }
        for b in 0..nb {
            red += pdis[b][t];
            red -= pch[b][t];
        }
        red += cur[t];
        if t > 0 {
            red -= sb * cur[t - 1];
        }
        model = model.with(constraint!(red + sf[wi] >= target_kw));
    }

    let sol = model.solve()?;

    // --- Extract ---
    let mut baseline_grid_kw = vec![0.0; n];
    let mut optimized_grid_kw = vec![0.0; n];
    let mut reduction_kw = vec![0.0; n];
    let mut curtail_kw = vec![0.0; n];
    let mut battery_net_kw = vec![0.0; n];
    let mut ev_charge_kw = vec![0.0; n];
    let mut cycle_cost_total = 0.0;

    for t in 0..n {
        let cur_v = sol.value(cur[t]);
        curtail_kw[t] = cur_v;
        let mut batt = 0.0;
        for b in 0..nb {
            let c = sol.value(pch[b][t]);
            let d = sol.value(pdis[b][t]);
            batt += d - c;
            cycle_cost_total += si.batteries[b].cycle_cost_per_kwh * dt * (c + d);
        }
        battery_net_kw[t] = batt;
        let mut evc = 0.0;
        for v in 0..nv {
            evc += sol.value(pev[v][t]);
        }
        ev_charge_kw[t] = evc;

        let snap = if t > 0 { sb * curtail_kw[t - 1] } else { 0.0 };
        optimized_grid_kw[t] = si.baseline_net_kw[t] - batt + evc - cur_v + snap;
        baseline_grid_kw[t] = si.baseline_net_kw[t] + naive_ev[t];
        reduction_kw[t] = baseline_grid_kw[t] - optimized_grid_kw[t];
    }

    let mut shortfall_kw = vec![0.0; n];
    for (wi, &t) in si.window_steps.iter().enumerate() {
        shortfall_kw[t] = sol.value(sf[wi]);
    }

    // Report the objective recomputed from the extracted solution.
    let mut objective = cycle_cost_total;
    for t in 0..n {
        objective += si.price_per_kwh[t] * dt * optimized_grid_kw[t];
        objective += si.thermostats.discomfort_weight * dt * curtail_kw[t];
        objective += SHORTFALL_PENALTY * dt * shortfall_kw[t];
    }

    Ok(DispatchSchedule {
        steps: n,
        dt_h: dt,
        window_steps: si.window_steps.clone(),
        target_kw,
        baseline_grid_kw,
        optimized_grid_kw,
        reduction_kw,
        shortfall_kw,
        curtail_kw,
        battery_net_kw,
        ev_charge_kw,
        naive_ev_kw: naive_ev,
        objective,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use vane_model::time::Window;
    use vane_model::{Granularity, NeighborhoodSpec};

    fn inputs() -> SimInputs {
        let toml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/oakville-demo.toml"
        ))
        .unwrap();
        let spec = NeighborhoodSpec::from_toml_str(&toml).unwrap();
        let n = vane_model::neighborhood::generate(&spec, 42).unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let s = vane_data::synthetic::scenario(date, Granularity::Hour);
        let w: Window = "17:00-20:00".parse().unwrap();
        vane_sim::build(&n, &s, w, Granularity::Hour).unwrap()
    }

    #[test]
    fn solves_and_shaves() {
        let si = inputs();
        // A modest, achievable target for a ~200-home neighborhood.
        let sched = optimize(&si, 80.0).unwrap();

        // Reduction is delivered during the window.
        assert!(sched.achieved_mean_kw() > 0.0);
        // Target is (near) met — shortfall small.
        let total_shortfall: f64 = sched.shortfall_kw.iter().sum();
        assert!(total_shortfall < 5.0, "shortfall {total_shortfall}");

        // No simultaneous charge & discharge: battery_net has a definite sign per step.
        // (Implied by the binary lock; here we just sanity-check finiteness.)
        assert!(sched.optimized_grid_kw.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn infeasible_target_reports_shortfall() {
        let si = inputs();
        // Absurd target (2 MW from ~200 homes) — must remain feasible via shortfall.
        let sched = optimize(&si, 2000.0).unwrap();
        let total_shortfall: f64 = sched.shortfall_kw.iter().sum();
        assert!(total_shortfall > 0.0, "expected shortfall on impossible target");
    }
}

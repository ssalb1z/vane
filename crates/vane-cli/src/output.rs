//! Human-readable output: neighborhood summary, dispatch table, scorecard.

use chrono::NaiveDate;
use vane_data::Scenario;
use vane_forecast::Forecast;
use vane_model::neighborhood::Summary;
use vane_optimize::DispatchSchedule;
use vane_score::Scorecard;

/// Per-hour forecast vs actual for demand and solar irradiance.
pub fn print_forecast(actual: &Scenario, fc: &Forecast, label: &str) {
    println!("forecast ({label}) vs actual — {}", actual.date);
    println!(
        "  {:>5}  {:>12}  {:>12}  {:>10}  {:>10}",
        "hour", "demand f MW", "demand a MW", "ghi f", "ghi a"
    );
    let n = actual.demand_mw.values.len();
    let mut abs_err = 0.0;
    for t in 0..n {
        let df = fc.demand_mw.at(t);
        let da = actual.demand_mw.at(t);
        abs_err += (df - da).abs();
        println!(
            "  {:>02}:00  {:>12.0}  {:>12.0}  {:>10.0}  {:>10.0}",
            t,
            df,
            da,
            fc.ghi_w_m2.at(t),
            actual.ghi_w_m2.at(t),
        );
    }
    println!("  mean abs demand error: {:.0} MW", abs_err / n as f64);
}

pub fn print_summary(name: &str, s: &Summary) {
    println!("neighborhood: {name}");
    println!("  homes            {}", s.homes);
    println!(
        "  thermostats      {}  ({:.0}%)",
        s.thermostats,
        pct(s.thermostats, s.homes)
    );
    println!(
        "  batteries        {}  ({:.1} kWh, {:.1} kW total)",
        s.batteries, s.battery_capacity_kwh, s.battery_power_kw
    );
    println!("  ev chargers      {}", s.evs);
    println!(
        "  rooftop solar    {}  ({:.1} kW-dc total)",
        s.solar, s.solar_capacity_kw_dc
    );
}

fn pct(a: usize, b: usize) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64 * 100.0
    }
}

/// Per-hour dispatch table for the whole day, marking window rows with `*`.
pub fn print_dispatch(sched: &DispatchSchedule) {
    let step_min = (sched.dt_h * 60.0).round() as i64;
    let in_window: Vec<bool> = {
        let mut v = vec![false; sched.steps];
        for &t in &sched.window_steps {
            v[t] = true;
        }
        v
    };
    println!(
        "  {:>5}  {:>10}  {:>10}  {:>10}  {:>8}",
        "time", "base kW", "opt kW", "reduce kW", "batt kW"
    );
    for t in 0..sched.steps {
        let mins = t as i64 * step_min;
        let hh = mins / 60;
        let mm = mins % 60;
        let mark = if in_window[t] { "*" } else { " " };
        println!(
            "{}{:02}:{:02}  {:>10.1}  {:>10.1}  {:>10.1}  {:>8.1}",
            mark,
            hh,
            mm,
            sched.baseline_grid_kw[t],
            sched.optimized_grid_kw[t],
            sched.reduction_kw[t],
            sched.battery_net_kw[t],
        );
    }
    println!("  (* = within peak-shave window)");
}

#[allow(clippy::too_many_arguments)]
pub fn print_scorecard(
    card: &Scorecard,
    neighborhood: &str,
    date: NaiveDate,
    window: &str,
    synthetic: bool,
    forecast_label: &str,
    perfect_achieved_mw: Option<f64>,
    marginal_cad_per_mw: Option<f64>,
) {
    let src = if synthetic {
        "SYNTHETIC data (not real IESO)"
    } else {
        "real IESO + Open-Meteo data"
    };
    println!();
    println!(
        "vane — {neighborhood} — {date} — window {window} — program {}",
        card.program.label()
    );
    println!("  data source          {src}");
    println!("  forecast             {forecast_label}");
    println!();
    println!("  Target shave         {:.2} MW", card.target_mw);
    println!(
        "  Achieved (mean)      {:.3} MW      ({:.1}% of target)",
        card.achieved_mean_mw, card.attainment_pct
    );
    println!("  Achieved (min hour)  {:.3} MW", card.achieved_min_mw);
    println!(
        "  From: thermostats {:.1} kWh · battery {:.1} kWh · EV shifted {:.1} kWh",
        card.thermostat_curtail_kwh, card.battery_discharge_kwh, card.ev_shifted_kwh
    );
    println!("  EV deadlines met     {}", if card.ev_deadlines_met { "yes" } else { "NO" });
    println!();
    println!("  Wholesale cost saved $ {:>8.2}", card.wholesale_saved_cad);
    println!("  Program payment      $ {:>8.2}", card.program_payment_cad);
    println!("      {}", card.program_payment_note);
    println!(
        "  Emissions avoided    {:.3} tCO2e   (AVERAGE intensity, not marginal)",
        card.emissions_avoided_tonnes()
    );
    if let Some(p) = perfect_achieved_mw {
        let delta = card.achieved_mean_mw - p;
        println!(
            "  Forecast vs perfect  {:.3} vs {:.3} MW achieved ({:+.3} MW from forecast error)",
            card.achieved_mean_mw, p, delta
        );
    }
    if let Some(m) = marginal_cad_per_mw {
        if m.is_finite() {
            println!();
            println!("  Marginal cost of the last MW shaved: $ {:.0}/MW", m);
        }
    } else if card.attainment_pct < 99.0 {
        println!();
        println!(
            "  Note: target exceeds available flexibility — the binding hour falls short.\n\
             \x20       Max achievable here is ~{:.3} MW (mean).",
            card.achieved_mean_mw
        );
    }
    println!();
}

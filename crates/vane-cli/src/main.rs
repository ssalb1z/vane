//! `vane` — DER dispatch / VPP simulator CLI (DESIGN.md §4).

mod output;
mod scenario;

use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use scenario::Source;
use vane_data::Scenario;
use vane_forecast::Forecast;
use vane_model::time::Window;
use vane_model::{Granularity, NeighborhoodSpec};
use vane_score::Program;

#[derive(Parser)]
#[command(name = "vane", version, about = "DER dispatch / virtual power plant simulator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

/// How the planner obtains the demand/solar it optimizes against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum ForecastMode {
    /// Plan on the actual day (upper bound — no forecast error).
    Perfect,
    /// Plan on pure-Rust climatology (per-hour mean of prior days).
    Baseline,
    /// Plan on the Python ML forecaster (temperature-aware).
    Python,
}

impl ForecastMode {
    fn label(self) -> &'static str {
        match self {
            ForecastMode::Perfect => "perfect foresight",
            ForecastMode::Baseline => "baseline (climatology)",
            ForecastMode::Python => "python (ML)",
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Summarize a generated neighborhood (asset counts and capacities).
    Info {
        spec: PathBuf,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Solve the dispatch MILP and print the per-hour schedule.
    Dispatch {
        spec: PathBuf,
        #[arg(long)]
        date: NaiveDate,
        #[arg(long)]
        target_mw: f64,
        #[arg(long)]
        window: Window,
        #[arg(long, default_value = "1h")]
        granularity: Granularity,
        #[arg(long, default_value = "synthetic")]
        source: Source,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Show the demand/solar forecast vs actuals for a day.
    Forecast {
        spec: PathBuf,
        #[arg(long)]
        date: NaiveDate,
        #[arg(long, value_enum, default_value = "baseline")]
        forecast: ForecastMode,
        #[arg(long, default_value = "1h")]
        granularity: Granularity,
        #[arg(long, default_value = "synthetic")]
        source: Source,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value = "python3 python/vane_forecast/predict.py")]
        python_cmd: String,
    },
    /// Forecast → dispatch → settle → scorecard (the headline command).
    Simulate {
        spec: PathBuf,
        #[arg(long)]
        date: NaiveDate,
        #[arg(long)]
        target_mw: f64,
        #[arg(long)]
        window: Window,
        #[arg(long, default_value = "peak-perks")]
        program: Program,
        #[arg(long, value_enum, default_value = "perfect")]
        forecast: ForecastMode,
        #[arg(long, default_value = "1h")]
        granularity: Granularity,
        #[arg(long, default_value = "synthetic")]
        source: Source,
        #[arg(long, default_value_t = 42)]
        seed: u64,
        #[arg(long, default_value = "python3 python/vane_forecast/predict.py")]
        python_cmd: String,
    },
    /// Download raw IESO + Open-Meteo data for a date into a directory.
    Fetch {
        #[arg(long)]
        date: NaiveDate,
        #[arg(long, default_value = "43.45")]
        lat: f64,
        #[arg(long, default_value = "-79.68")]
        lon: f64,
        #[arg(long, default_value = "data")]
        out: PathBuf,
    },
}

fn window_str(w: &Window) -> String {
    format!("{}-{}", w.start.format("%H:%M"), w.end.format("%H:%M"))
}

/// Prior-day training scenarios for the forecaster: the 14 days before `date`.
/// Synthetic source generates them; IESO source fetches real history (and on a
/// fetch failure warns and returns empty, so callers fall back to perfect).
fn training_scenarios(
    date: NaiveDate,
    g: Granularity,
    source: Source,
    lat: f64,
    lon: f64,
) -> Vec<Scenario> {
    match source {
        Source::Synthetic => (1..=14)
            .map(|d| vane_data::synthetic::scenario(date - chrono::Duration::days(d), g))
            .collect(),
        Source::Ieso => {
            let dates: Vec<NaiveDate> = (1..=14).map(|d| date - chrono::Duration::days(d)).collect();
            match scenario::ieso_training(&dates, lat, lon, g) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("warn: IESO training fetch failed: {e}");
                    Vec::new()
                }
            }
        }
    }
}

fn build_forecast(
    mode: ForecastMode,
    train: &[Scenario],
    actual: &Scenario,
    date: NaiveDate,
    g: Granularity,
    python_cmd: &str,
) -> Result<Forecast> {
    match mode {
        ForecastMode::Baseline => vane_forecast::baseline(train, date, g),
        ForecastMode::Python => {
            let argv: Vec<String> = python_cmd.split_whitespace().map(String::from).collect();
            vane_forecast::python(train, &actual.temperature_c, date, g, &argv)
        }
        ForecastMode::Perfect => unreachable!("perfect foresight needs no forecast"),
    }
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Info { spec, seed } => {
            let spec = NeighborhoodSpec::from_path(&spec)?;
            let n = vane_model::neighborhood::generate(&spec, seed)?;
            output::print_summary(&n.name, &n.summary());
        }

        Cmd::Dispatch {
            spec,
            date,
            target_mw,
            window,
            granularity,
            source,
            seed,
        } => {
            let spec = NeighborhoodSpec::from_path(&spec)?;
            let n = vane_model::neighborhood::generate(&spec, seed)?;
            let (sc, synth) = scenario::resolve(source, date, granularity, n.lat, n.lon)?;
            let si = vane_sim::build(&n, &sc, window, granularity)?;
            let sched = vane_optimize::optimize(&si, target_mw * 1000.0)?;
            println!(
                "dispatch — {} — {date} — window {} — {}",
                n.name,
                window_str(&window),
                if synth { "SYNTHETIC" } else { "real data" }
            );
            output::print_dispatch(&sched);
        }

        Cmd::Forecast {
            spec,
            date,
            forecast,
            granularity,
            source,
            seed,
            python_cmd,
        } => {
            let spec = NeighborhoodSpec::from_path(&spec)?;
            let n = vane_model::neighborhood::generate(&spec, seed)?;
            let (actual, _synth) = scenario::resolve(source, date, granularity, n.lat, n.lon)?;
            let train = training_scenarios(date, granularity, source, n.lat, n.lon);
            if train.is_empty() {
                anyhow::bail!("no training data available for this date/source");
            }
            let mode = if forecast == ForecastMode::Perfect {
                ForecastMode::Baseline
            } else {
                forecast
            };
            let fc = build_forecast(mode, &train, &actual, date, granularity, &python_cmd)?;
            output::print_forecast(&actual, &fc, mode.label());
        }

        Cmd::Simulate {
            spec,
            date,
            target_mw,
            window,
            program,
            forecast,
            granularity,
            source,
            seed,
            python_cmd,
        } => {
            let spec = NeighborhoodSpec::from_path(&spec)?;
            let n = vane_model::neighborhood::generate(&spec, seed)?;
            let (actual, synth) = scenario::resolve(source, date, granularity, n.lat, n.lon)?;
            let actual_si = vane_sim::build(&n, &actual, window, granularity)?;
            let target_kw = target_mw * 1000.0;

            let (sched, perfect_achieved_mw, fc_label) = if forecast == ForecastMode::Perfect {
                (
                    vane_optimize::optimize(&actual_si, target_kw)?,
                    None,
                    ForecastMode::Perfect.label().to_string(),
                )
            } else {
                let train = training_scenarios(date, granularity, source, n.lat, n.lon);
                if train.is_empty() {
                    eprintln!(
                        "note: no training history available; using perfect foresight"
                    );
                    (
                        vane_optimize::optimize(&actual_si, target_kw)?,
                        None,
                        "perfect (IESO history unavailable)".to_string(),
                    )
                } else {
                    // Plan on the forecast, then execute that plan under actuals,
                    // and compare achieved reduction to a perfect-foresight plan.
                    let fc = build_forecast(forecast, &train, &actual, date, granularity, &python_cmd)?;
                    let fc_si = vane_sim::build(&n, &fc.as_scenario(&actual), window, granularity)?;
                    let plan = vane_optimize::optimize(&fc_si, target_kw)?;
                    let realized = vane_optimize::replay(&actual_si, &plan, target_kw);
                    let perfect = vane_optimize::optimize(&actual_si, target_kw)?;
                    let perfect_mw = perfect.achieved_mean_kw() / 1000.0;
                    (realized, Some(perfect_mw), format!("{} (planned on forecast)", forecast.label()))
                }
            };

            let card = vane_score::score(&actual_si, &sched, program);

            // Marginal cost of the last MW shaved (perfect mode, target met).
            let total_shortfall: f64 = sched.shortfall_kw.iter().sum();
            let marginal = if forecast == ForecastMode::Perfect && total_shortfall < 1.0 {
                vane_optimize::optimize(&actual_si, target_kw + 1.0)
                    .ok()
                    .map(|s2| (s2.objective - sched.objective) / 0.001)
            } else {
                None
            };

            output::print_scorecard(
                &card,
                &n.name,
                date,
                &window_str(&window),
                synth,
                &fc_label,
                perfect_achieved_mw,
                marginal,
            );
        }

        Cmd::Fetch { date, lat, lon, out } => {
            use chrono::Datelike;
            use vane_data::fetch;
            std::fs::create_dir_all(&out)?;
            let year = date.year();
            let ds = date.format("%Y-%m-%d").to_string();
            let jobs = [
                ("demand.csv", fetch::ieso_demand_url(year)),
                ("hoep.csv", fetch::ieso_hoep_url(year)),
                ("weather.json", fetch::open_meteo_url(lat, lon, &ds)),
            ];
            for (name, url) in jobs {
                print!("fetching {name} … ");
                match fetch::get_text(&url) {
                    Ok(body) => {
                        let path = out.join(name);
                        std::fs::write(&path, &body)?;
                        println!("ok ({} bytes) → {}", body.len(), path.display());
                    }
                    Err(e) => println!("FAILED: {e}"),
                }
            }
        }
    }
    Ok(())
}

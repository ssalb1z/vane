//! `vane` — DER dispatch / VPP simulator CLI (DESIGN.md §4).

mod output;
mod scenario;

use std::path::PathBuf;

use anyhow::Result;
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use scenario::Source;
use vane_model::time::Window;
use vane_model::{Granularity, NeighborhoodSpec};
use vane_score::Program;

#[derive(Parser)]
#[command(name = "vane", version, about = "DER dispatch / virtual power plant simulator")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
        #[arg(long, default_value = "1h")]
        granularity: Granularity,
        #[arg(long, default_value = "synthetic")]
        source: Source,
        #[arg(long, default_value_t = 42)]
        seed: u64,
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

        Cmd::Simulate {
            spec,
            date,
            target_mw,
            window,
            program,
            granularity,
            source,
            seed,
        } => {
            let spec = NeighborhoodSpec::from_path(&spec)?;
            let n = vane_model::neighborhood::generate(&spec, seed)?;
            let (sc, synth) = scenario::resolve(source, date, granularity, n.lat, n.lon)?;
            let si = vane_sim::build(&n, &sc, window, granularity)?;

            let target_kw = target_mw * 1000.0;
            let sched = vane_optimize::optimize(&si, target_kw)?;
            let card = vane_score::score(&si, &sched, program);

            // Marginal cost of the last MW shaved, via a +1 kW re-solve. Only
            // meaningful when the target is met; under shortfall the "marginal"
            // is just the shortfall penalty, so we suppress it.
            let total_shortfall: f64 = sched.shortfall_kw.iter().sum();
            let marginal = if total_shortfall < 1.0 {
                vane_optimize::optimize(&si, target_kw + 1.0)
                    .ok()
                    .map(|s2| (s2.objective - sched.objective) / 0.001)
            } else {
                None
            };

            output::print_scorecard(&card, &n.name, date, &window_str(&window), synth, marginal);
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

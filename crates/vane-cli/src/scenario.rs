//! Resolve the raw [`Scenario`] for a run: deterministic synthetic (offline
//! default) or real IESO + Open-Meteo data (`--source ieso`).

use std::collections::{BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};
use vane_data::{emissions, fetch, ieso, synthetic, weather, Scenario};
use vane_model::Granularity;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Source {
    /// Deterministic synthetic summer-peak day (no network).
    Synthetic,
    /// Real IESO demand + HOEP and Open-Meteo weather (needs network).
    Ieso,
}

/// Returns `(scenario, is_synthetic)`.
pub fn resolve(
    source: Source,
    date: NaiveDate,
    g: Granularity,
    lat: f64,
    lon: f64,
) -> anyhow::Result<(Scenario, bool)> {
    match source {
        Source::Synthetic => Ok((synthetic::scenario(date, g), true)),
        Source::Ieso => {
            let year = date.year();
            let date_str = date.format("%Y-%m-%d").to_string();

            let demand_txt = fetch::get_text(&fetch::ieso_demand_url(year))?;
            let demand = ieso::parse_demand(&demand_txt, date)?;

            let hoep_txt = fetch::get_text(&fetch::ieso_hoep_url(year)).map_err(|e| {
                anyhow::anyhow!(
                    "{e}\nNote: HOEP was retired May 2025; for {year} there may be no HOEP file. \
                     Try a pre-2025 date or --source synthetic."
                )
            })?;
            let price = ieso::parse_hoep(&hoep_txt, date)?;

            let wx = fetch::get_text(&fetch::open_meteo_url(lat, lon, &date_str))?;
            let (temp, ghi) = weather::parse_open_meteo(&wx, date)?;

            // Average intensity heuristic — Ontario publishes no marginal series.
            let emis = emissions::heuristic_intensity(&demand);

            let s = Scenario {
                date,
                granularity: Granularity::Hour,
                demand_mw: demand,
                price_per_mwh: price,
                emissions_g_per_kwh: emis,
                temperature_c: temp,
                ghi_w_m2: ghi,
            };
            Ok((s.to_granularity(g), false))
        }
    }
}

/// Fetch real IESO + Open-Meteo scenarios for `dates` (forecaster training).
///
/// Efficient: each yearly demand/HOEP CSV is downloaded once, weather is one
/// range call. Days that fail to parse (e.g. HOEP absent post-May-2025) are
/// skipped with a warning rather than aborting — partial history still trains.
pub fn ieso_training(
    dates: &[NaiveDate],
    lat: f64,
    lon: f64,
    g: Granularity,
) -> anyhow::Result<Vec<Scenario>> {
    if dates.is_empty() {
        return Ok(Vec::new());
    }

    let years: BTreeSet<i32> = dates.iter().map(|d| d.year()).collect();
    let mut demand_txt: HashMap<i32, String> = HashMap::new();
    let mut hoep_txt: HashMap<i32, String> = HashMap::new();
    for y in years {
        demand_txt.insert(y, fetch::get_text(&fetch::ieso_demand_url(y))?);
        match fetch::get_text(&fetch::ieso_hoep_url(y)) {
            Ok(t) => {
                hoep_txt.insert(y, t);
            }
            Err(e) => eprintln!("warn: no HOEP for {y} ({e}); those days will be skipped"),
        }
    }

    let start = dates.iter().min().unwrap().format("%Y-%m-%d").to_string();
    let end = dates.iter().max().unwrap().format("%Y-%m-%d").to_string();
    let wx = fetch::get_text(&fetch::open_meteo_range_url(lat, lon, &start, &end))?;

    let mut out = Vec::new();
    for &d in dates {
        let y = d.year();
        let demand = match ieso::parse_demand(&demand_txt[&y], d) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("warn: skip training day {d}: {e}");
                continue;
            }
        };
        let price = match hoep_txt.get(&y).and_then(|t| ieso::parse_hoep(t, d).ok()) {
            Some(p) => p,
            None => {
                eprintln!("warn: skip training day {d}: no HOEP");
                continue;
            }
        };
        let (temp, ghi) = match weather::parse_open_meteo(&wx, d) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("warn: skip training day {d}: {e}");
                continue;
            }
        };
        let emis = emissions::heuristic_intensity(&demand);
        let s = Scenario {
            date: d,
            granularity: Granularity::Hour,
            demand_mw: demand,
            price_per_mwh: price,
            emissions_g_per_kwh: emis,
            temperature_c: temp,
            ghi_w_m2: ghi,
        };
        out.push(s.to_granularity(g));
    }
    Ok(out)
}

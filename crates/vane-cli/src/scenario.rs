//! Resolve the raw [`Scenario`] for a run: deterministic synthetic (offline
//! default) or real IESO + Open-Meteo data (`--source ieso`).

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

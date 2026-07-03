//! Thin HTTP downloader for the IESO/weather endpoints (DESIGN.md §11).
//!
//! Kept deliberately small: fetch text, and build the canonical report URLs.
//! Assembling a full [`crate::Scenario`] from fetched files is the CLI's job.
//! Network may be unavailable (e.g. in CI/sandbox); callers fall back to
//! [`crate::synthetic`].

/// GET a URL and return the body as text.
pub fn get_text(url: &str) -> anyhow::Result<String> {
    let body = ureq::get(url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?
        .into_string()?;
    Ok(body)
}

const IESO_PUBLIC: &str = "https://reports-public.ieso.ca/public";

/// Yearly Ontario Demand report, e.g. `.../Demand/PUB_Demand_2024.csv`.
pub fn ieso_demand_url(year: i32) -> String {
    format!("{IESO_PUBLIC}/Demand/PUB_Demand_{year}.csv")
}

/// Yearly HOEP report (valid 2002..=2025; HOEP was retired May 2025).
pub fn ieso_hoep_url(year: i32) -> String {
    format!("{IESO_PUBLIC}/PriceHOEPPredispOR/PUB_PriceHOEPPredispOR_{year}.csv")
}

/// Open-Meteo historical archive for hourly GHI + temperature at a point,
/// for a date range (inclusive). One call covers many days.
pub fn open_meteo_range_url(lat: f64, lon: f64, start: &str, end: &str) -> String {
    format!(
        "https://archive-api.open-meteo.com/v1/archive?latitude={lat}&longitude={lon}\
         &start_date={start}&end_date={end}\
         &hourly=temperature_2m,shortwave_radiation&timezone=America%2FToronto"
    )
}

/// Single-day convenience wrapper over [`open_meteo_range_url`].
pub fn open_meteo_url(lat: f64, lon: f64, date: &str) -> String {
    open_meteo_range_url(lat, lon, date, date)
}

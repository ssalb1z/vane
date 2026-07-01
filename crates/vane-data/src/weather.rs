//! Parse the Open-Meteo historical archive JSON into temperature and GHI
//! series (DESIGN.md §11). One-day requests return 24 hourly points.

use chrono::NaiveDate;
use serde::Deserialize;
use vane_model::Granularity;

use crate::series::DaySeries;

#[derive(Deserialize)]
struct OpenMeteo {
    hourly: Hourly,
}

#[derive(Deserialize)]
struct Hourly {
    time: Vec<String>,
    temperature_2m: Vec<f64>,
    shortwave_radiation: Vec<f64>,
}

/// Returns `(temperature_c, ghi_w_m2)` hourly series for `date`.
pub fn parse_open_meteo(json: &str, date: NaiveDate) -> anyhow::Result<(DaySeries, DaySeries)> {
    let om: OpenMeteo = serde_json::from_str(json)?;
    let date_str = date.format("%Y-%m-%d").to_string();

    let mut temp = Vec::with_capacity(24);
    let mut ghi = Vec::with_capacity(24);
    for (i, t) in om.hourly.time.iter().enumerate() {
        if t.starts_with(&date_str) {
            temp.push(om.hourly.temperature_2m[i]);
            ghi.push(om.hourly.shortwave_radiation[i]);
        }
    }
    if temp.len() != 24 {
        anyhow::bail!("expected 24 hourly weather points for {date}, got {}", temp.len());
    }
    Ok((
        DaySeries::new(date, Granularity::Hour, temp)?,
        DaySeries::new(date, Granularity::Hour, ghi)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_open_meteo() {
        let times: Vec<String> = (0..24).map(|h| format!("2024-08-01T{h:02}:00")).collect();
        let temps: Vec<f64> = (0..24).map(|h| 20.0 + h as f64 * 0.1).collect();
        let ghis: Vec<f64> = (0..24).map(|h| if (6..20).contains(&h) { 500.0 } else { 0.0 }).collect();
        let json = serde_json::json!({
            "hourly": { "time": times, "temperature_2m": temps, "shortwave_radiation": ghis }
        })
        .to_string();

        let d = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let (t, g) = parse_open_meteo(&json, d).unwrap();
        assert_eq!(t.values.len(), 24);
        assert_eq!(g.at(3), 0.0);
        assert_eq!(g.at(12), 500.0);
    }
}

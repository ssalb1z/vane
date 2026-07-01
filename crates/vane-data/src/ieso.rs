//! Parsers for the real IESO public CSV reports (DESIGN.md §11).
//!
//! IESO hourly reports look like:
//! ```text
//! \\ comment / provenance lines, backslash-prefixed
//! Date,Hour,Market Demand,Ontario Demand
//! 2024-08-01,1,15234,14567
//! ...
//! ```
//! `Hour` is 1..24 (hour-ending), so hour `h` maps to index `h - 1`. Yearly
//! files hold many dates; we filter to the requested one.

use chrono::NaiveDate;
use vane_model::Granularity;

use crate::series::DaySeries;

fn header_index(headers: &csv::StringRecord, name: &str) -> Option<usize> {
    let want = name.trim().to_ascii_lowercase();
    headers
        .iter()
        .position(|h| h.trim().to_ascii_lowercase() == want)
}

fn parse_date_cell(cell: &str) -> Option<NaiveDate> {
    let c = cell.trim();
    NaiveDate::parse_from_str(c, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(c, "%Y/%m/%d"))
        .ok()
}

/// Extract one hourly column for `date` from an IESO CSV, as a 24-value
/// [`DaySeries`] at [`Granularity::Hour`]. Fails if the date/column is absent
/// or the day is missing hours.
pub fn parse_hourly_column(
    text: &str,
    date: NaiveDate,
    date_col: &str,
    hour_col: &str,
    value_col: &str,
) -> anyhow::Result<DaySeries> {
    let mut rdr = csv::ReaderBuilder::new()
        .comment(Some(b'\\'))
        .flexible(true)
        .has_headers(true)
        .from_reader(text.as_bytes());

    let headers = rdr.headers()?.clone();
    let di = header_index(&headers, date_col)
        .ok_or_else(|| anyhow::anyhow!("no {date_col:?} column in {:?}", headers))?;
    let hi = header_index(&headers, hour_col)
        .ok_or_else(|| anyhow::anyhow!("no {hour_col:?} column in {:?}", headers))?;
    let vi = header_index(&headers, value_col)
        .ok_or_else(|| anyhow::anyhow!("no {value_col:?} column in {:?}", headers))?;

    let mut values = vec![f64::NAN; 24];
    for rec in rdr.records() {
        let rec = rec?;
        let Some(row_date) = rec.get(di).and_then(parse_date_cell) else {
            continue;
        };
        if row_date != date {
            continue;
        }
        let hour: usize = rec
            .get(hi)
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| anyhow::anyhow!("bad hour in row {rec:?}"))?;
        if !(1..=24).contains(&hour) {
            anyhow::bail!("hour {hour} out of range 1..24");
        }
        let v: f64 = rec
            .get(vi)
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| anyhow::anyhow!("bad {value_col} value in row {rec:?}"))?;
        values[hour - 1] = v;
    }

    if let Some(missing) = values.iter().position(|v| v.is_nan()) {
        anyhow::bail!("date {date} missing hour {} in column {value_col:?}", missing + 1);
    }
    DaySeries::new(date, Granularity::Hour, values)
}

/// Ontario Demand for `date` from the Hourly Demand Report.
pub fn parse_demand(text: &str, date: NaiveDate) -> anyhow::Result<DaySeries> {
    parse_hourly_column(text, date, "Date", "Hour", "Ontario Demand")
}

/// HOEP for `date` from the HOEP/Predispatch/OR report.
pub fn parse_hoep(text: &str, date: NaiveDate) -> anyhow::Result<DaySeries> {
    parse_hourly_column(text, date, "Date", "Hour", "HOEP")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Two backslash comment lines, a header, then two dates × 24 hours.
    fn demand_csv() -> String {
        let mut s = String::from("\\\\ CSV downloaded ...\n\\\\ Hourly Demand Report\nDate,Hour,Market Demand,Ontario Demand\n");
        for h in 1..=24 {
            s.push_str(&format!("2024-08-01,{h},{},{}\n", 16000 + h * 10, 15000 + h * 10));
            s.push_str(&format!("2024-08-02,{h},{},{}\n", 17000, 16000));
        }
        s
    }

    #[test]
    fn parses_demand_for_date() {
        let d = NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let s = parse_demand(&demand_csv(), d).unwrap();
        assert_eq!(s.values.len(), 24);
        assert_eq!(s.at(0), 15010.0); // hour 1 → 15000 + 10
        assert_eq!(s.at(23), 15240.0); // hour 24
    }

    #[test]
    fn errors_on_missing_date() {
        let d = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        assert!(parse_demand(&demand_csv(), d).is_err());
    }
}

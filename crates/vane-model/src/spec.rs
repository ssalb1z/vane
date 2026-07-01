//! The `neighborhood.toml` schema — deserialized, not yet materialized.
//!
//! Numbers here describe *classes* of asset and how prevalent they are; the
//! generator ([`crate::neighborhood::generate`]) samples a concrete population
//! from them. See DESIGN.md §5 for the annotated example and the provenance of
//! the default figures (e.g. the 0.59 kW/thermostat from IESO's PY2024 M&V).

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeighborhoodSpec {
    pub neighborhood: Meta,
    pub penetration: Penetration,
    pub assets: AssetParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    pub name: String,
    pub homes: usize,
    pub timezone: String,
    pub lat: f64,
    pub lon: f64,
}

/// Fraction of homes fitted with each asset class. Values are clamped to
/// `[0, 1]` at generation time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Penetration {
    pub smart_thermostat: f64,
    pub home_battery: f64,
    pub ev_charger: f64,
    pub rooftop_solar: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetParams {
    pub thermostat: ThermostatParams,
    pub battery: BatteryParams,
    pub ev_charger: EvParams,
    pub solar: SolarParams,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermostatParams {
    pub max_setback_c: f64,
    pub precool_c: f64,
    pub avg_reduction_kw: f64,
    pub snapback_kw: f64,
    pub discomfort_weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatteryParams {
    pub capacity_kwh: f64,
    pub power_kw: f64,
    pub roundtrip_eff: f64,
    pub soc_init_frac: f64,
    pub soc_min_frac: f64,
    pub cycle_cost_per_kwh: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvParams {
    pub power_kw: f64,
    pub required_kwh: f64,
    /// "HH:MM" — energy must be delivered by this time.
    pub deadline: String,
    /// "HH:MM" — earliest the vehicle is plugged in.
    pub plugin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolarParams {
    pub capacity_kw_dc: f64,
    pub inverter_kw_ac: f64,
    pub derate: f64,
}

impl NeighborhoodSpec {
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        Ok(toml::from_str(s)?)
    }

    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
        Self::from_toml_str(&text)
    }
}

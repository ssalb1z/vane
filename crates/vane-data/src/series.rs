//! Time-series primitives shared across the pipeline.

use chrono::{NaiveDate, NaiveTime, Timelike};
use vane_model::time::Window;
use vane_model::Granularity;

/// One calendar day of a single quantity, sampled at a fixed [`Granularity`].
/// `values[i]` is the value for the step beginning `i * Δt` after local midnight.
#[derive(Debug, Clone, PartialEq)]
pub struct DaySeries {
    pub date: NaiveDate,
    pub granularity: Granularity,
    pub values: Vec<f64>,
}

impl DaySeries {
    pub fn new(date: NaiveDate, granularity: Granularity, values: Vec<f64>) -> anyhow::Result<Self> {
        let want = granularity.steps_per_day();
        if values.len() != want {
            anyhow::bail!(
                "expected {want} values for a {:?} day, got {}",
                granularity,
                values.len()
            );
        }
        Ok(Self {
            date,
            granularity,
            values,
        })
    }

    /// Step index containing local time `t`.
    pub fn step_of(&self, t: NaiveTime) -> usize {
        let mins = t.hour() * 60 + t.minute();
        (mins / self.granularity.minutes()) as usize
    }

    /// The step indices covered by `window`.
    pub fn window_steps(&self, window: Window) -> Vec<usize> {
        window
            .steps(self.granularity)
            .into_iter()
            .map(|t| self.step_of(t))
            .collect()
    }

    pub fn at(&self, step: usize) -> f64 {
        self.values[step]
    }

    pub fn mean(&self) -> f64 {
        self.values.iter().sum::<f64>() / self.values.len() as f64
    }

    /// Upsample an hourly series to a finer granularity by holding each hourly
    /// value flat across its sub-steps (adequate for IESO hourly data on a
    /// 15-min run).
    pub fn to_granularity(&self, g: Granularity) -> DaySeries {
        if g == self.granularity {
            return self.clone();
        }
        let per_old = self.granularity.minutes();
        let per_new = g.minutes();
        let steps = g.steps_per_day();
        let mut values = Vec::with_capacity(steps);
        for i in 0..steps {
            let old_idx = ((i as u32 * per_new) / per_old) as usize;
            values.push(self.values[old_idx.min(self.values.len() - 1)]);
        }
        DaySeries {
            date: self.date,
            granularity: g,
            values,
        }
    }
}

/// The raw inputs a dispatch run consumes for one date. All series share the
/// same date and granularity.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub date: NaiveDate,
    pub granularity: Granularity,
    /// System (Ontario or zonal) demand, MW. Shapes the neighborhood baseline.
    pub demand_mw: DaySeries,
    /// Wholesale price, $/MWh (HOEP-era or post-MRP LMP/zonal).
    pub price_per_mwh: DaySeries,
    /// Grid emissions intensity, gCO2e/kWh. AVERAGE, not marginal (DESIGN.md §9).
    pub emissions_g_per_kwh: DaySeries,
    /// Outdoor dry-bulb temperature, °C.
    pub temperature_c: DaySeries,
    /// Global horizontal irradiance, W/m².
    pub ghi_w_m2: DaySeries,
}

impl Scenario {
    /// Re-sample every series to `g` (no-op if already at `g`).
    pub fn to_granularity(&self, g: Granularity) -> Scenario {
        Scenario {
            date: self.date,
            granularity: g,
            demand_mw: self.demand_mw.to_granularity(g),
            price_per_mwh: self.price_per_mwh.to_granularity(g),
            emissions_g_per_kwh: self.emissions_g_per_kwh.to_granularity(g),
            temperature_c: self.temperature_c.to_granularity(g),
            ghi_w_m2: self.ghi_w_m2.to_granularity(g),
        }
    }
}

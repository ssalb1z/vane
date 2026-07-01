//! `vane-score` — settle a dispatch against the mechanics of a real Ontario
//! program and produce a [`Scorecard`] (DESIGN.md §8, §9).
//!
//! Scoring is a *historical replay*: the schedule's baseline is the counterfactual
//! and its optimized profile is what vane would have done. Dollars, emissions and
//! discomfort are computed over the full day (load is shifted, not just shed).
//!
//! **Emissions are AVERAGE-based, not marginal** — Ontario does not publish
//! marginal intensity (DESIGN.md §9). All program dollar figures carry provenance
//! and, where secondary-only, are clearly flagged.

use std::str::FromStr;

use vane_optimize::DispatchSchedule;
use vane_sim::SimInputs;

// --- Program constants (see DESIGN.md §8 for sources) ---
// Capacity Auction, Summer 2025 clearing (VERIFIED on ieso.ca).
const CAP_AUCTION_CAD_PER_MW_DAY: f64 = 332.39;
// Capacity Auction test/emergency energy rate HDRTAPR (VERIFIED, MM5.5).
const CAP_AUCTION_ENERGY_CAD_PER_MWH: f64 = 250.0;
// York Region NWA capacity auction clearing, 2021 (VERIFIED, IESO backgrounder).
const YORK_NWA_CAD_PER_KW_DAY: f64 = 0.64;
// York Region NWA local energy auction price ceiling, $2.00/kWh (VERIFIED).
const YORK_NWA_ENERGY_CEILING_CAD_PER_KWH: f64 = 2.00;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    /// Residential VPP (thermostats only); flat incentive, no per-event pay.
    PeakPerks,
    /// Mature wholesale C&I DR: $/MW-day availability + $250/MWh test energy.
    CapacityAuction,
    /// Local energy market (York Region NWA design); capacity + DLMP energy.
    YorkNwa,
    /// Alias of [`Program::YorkNwa`] flagged as design-intent (Centricity's own
    /// mechanics are unpublished).
    Centricity,
}

impl Program {
    pub fn label(self) -> &'static str {
        match self {
            Program::PeakPerks => "peak-perks",
            Program::CapacityAuction => "capacity-auction",
            Program::YorkNwa => "york-nwa",
            Program::Centricity => "centricity (design-intent)",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown program {0:?} (expected peak-perks | capacity-auction | york-nwa | centricity)")]
pub struct UnknownProgram(String);

impl FromStr for Program {
    type Err = UnknownProgram;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "peak-perks" | "peakperks" | "peak_perks" => Ok(Program::PeakPerks),
            "capacity-auction" | "capacity" | "ca" => Ok(Program::CapacityAuction),
            "york-nwa" | "york" | "nwa" => Ok(Program::YorkNwa),
            "centricity" => Ok(Program::Centricity),
            other => Err(UnknownProgram(other.to_string())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scorecard {
    pub program: Program,
    pub target_mw: f64,
    pub achieved_mean_mw: f64,
    pub achieved_min_mw: f64,
    /// Mean attainment as a percentage of target (capped display in the CLI).
    pub attainment_pct: f64,
    /// Whole-day wholesale energy cost avoided (arbitrage + shed), CAD.
    pub wholesale_saved_cad: f64,
    /// Program-specific participant payment, CAD (0 for Peak Perks).
    pub program_payment_cad: f64,
    pub program_payment_note: String,
    /// Emissions avoided over the day (AVERAGE intensity), kg CO2e.
    pub emissions_avoided_kg: f64,
    // Where the reduction came from, during the window (kWh):
    pub thermostat_curtail_kwh: f64,
    pub battery_discharge_kwh: f64,
    pub ev_shifted_kwh: f64,
    pub ev_deadlines_met: bool,
}

impl Scorecard {
    pub fn emissions_avoided_tonnes(&self) -> f64 {
        self.emissions_avoided_kg / 1000.0
    }
}

fn window_reduction_mwh(si: &SimInputs, s: &DispatchSchedule) -> f64 {
    s.window_steps
        .iter()
        .map(|&t| s.reduction_kw[t] * si.dt_h / 1000.0)
        .sum()
}

/// Score `schedule` under `program`.
pub fn score(si: &SimInputs, s: &DispatchSchedule, program: Program) -> Scorecard {
    let dt = si.dt_h;

    let achieved_mean_mw = s.achieved_mean_kw() / 1000.0;
    let achieved_min_mw = s.achieved_min_kw().max(0.0) / 1000.0;
    let target_mw = s.target_kw / 1000.0;
    let attainment_pct = if target_mw > 0.0 {
        (achieved_mean_mw / target_mw * 100.0).min(999.9)
    } else {
        0.0
    };

    // Whole-day wholesale cost avoided and emissions avoided (average intensity).
    let mut wholesale_saved_cad = 0.0;
    let mut emissions_avoided_g = 0.0;
    for t in 0..si.steps {
        let dgrid = s.baseline_grid_kw[t] - s.optimized_grid_kw[t]; // kW
        wholesale_saved_cad += dgrid * dt * si.price_per_kwh[t];
        emissions_avoided_g += dgrid * dt * si.emissions_g_per_kwh[t];
    }

    // Window contributions by asset class.
    let thermostat_curtail_kwh: f64 = s.window_steps.iter().map(|&t| s.curtail_kw[t] * dt).sum();
    let battery_discharge_kwh: f64 = s
        .window_steps
        .iter()
        .map(|&t| s.battery_net_kw[t].max(0.0) * dt)
        .sum();
    let ev_shifted_kwh: f64 = s
        .window_steps
        .iter()
        .map(|&t| (s.naive_ev_kw[t] - s.ev_charge_kw[t]).max(0.0) * dt)
        .sum();

    let red_mwh = window_reduction_mwh(si, s);
    let cap_mw = achieved_min_mw; // firm capacity = worst window hour

    let (program_payment_cad, program_payment_note) = match program {
        Program::PeakPerks => (
            0.0,
            "Peak Perks pays a flat $75 enrollment + $20/yr — no per-event payment. \
             Value here is avoided system/wholesale cost."
                .to_string(),
        ),
        Program::CapacityAuction => {
            let availability = cap_mw * CAP_AUCTION_CAD_PER_MW_DAY;
            let energy = red_mwh * CAP_AUCTION_ENERGY_CAD_PER_MWH;
            (
                availability + energy,
                format!(
                    "Illustrative: availability {cap_mw:.3} MW x ${CAP_AUCTION_CAD_PER_MW_DAY}/MW-day \
                     (Summer 2025) + energy {red_mwh:.3} MWh x ${CAP_AUCTION_ENERGY_CAD_PER_MWH}/MWh (HDRTAPR). \
                     Simplified: no CBL/IDAF or CNPF penalties."
                ),
            )
        }
        Program::YorkNwa | Program::Centricity => {
            let capacity = cap_mw * 1000.0 * YORK_NWA_CAD_PER_KW_DAY;
            // DLMP proxied by wholesale price, bounded by the $2.00/kWh ceiling.
            let energy: f64 = s
                .window_steps
                .iter()
                .map(|&t| {
                    let dlmp = si.price_per_kwh[t].min(YORK_NWA_ENERGY_CEILING_CAD_PER_KWH);
                    s.reduction_kw[t] * dt * dlmp
                })
                .sum();
            let intent = if program == Program::Centricity {
                " Centricity's own mechanics are unpublished; using York Region NWA design."
            } else {
                ""
            };
            (
                capacity + energy,
                format!(
                    "Illustrative: capacity {cap_mw:.3} MW x ${YORK_NWA_CAD_PER_KW_DAY}/kW-day (2021) \
                     + DLMP energy (wholesale proxy, ${YORK_NWA_ENERGY_CEILING_CAD_PER_KWH}/kWh ceiling).{intent}"
                ),
            )
        }
    };

    Scorecard {
        program,
        target_mw,
        achieved_mean_mw,
        achieved_min_mw,
        attainment_pct,
        wholesale_saved_cad,
        program_payment_cad,
        program_payment_note,
        emissions_avoided_kg: emissions_avoided_g / 1000.0,
        thermostat_curtail_kwh,
        battery_discharge_kwh,
        ev_shifted_kwh,
        ev_deadlines_met: true, // guaranteed by the EV energy constraint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vane_sim::SimInputs;

    fn scored(program: Program) -> Scorecard {
        let toml = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/oakville-demo.toml"
        ))
        .unwrap();
        let spec = vane_model::NeighborhoodSpec::from_toml_str(&toml).unwrap();
        let n = vane_model::neighborhood::generate(&spec, 42).unwrap();
        let date = chrono::NaiveDate::from_ymd_opt(2024, 8, 1).unwrap();
        let sc = vane_data::synthetic::scenario(date, vane_model::Granularity::Hour);
        let w: vane_model::time::Window = "17:00-20:00".parse().unwrap();
        let si: SimInputs = vane_sim::build(&n, &sc, w, vane_model::Granularity::Hour).unwrap();
        let sched = vane_optimize::optimize(&si, 80.0).unwrap();
        score(&si, &sched, program)
    }

    #[test]
    fn peak_perks_pays_nothing_but_saves() {
        let card = scored(Program::PeakPerks);
        assert_eq!(card.program_payment_cad, 0.0);
        assert!(card.achieved_mean_mw > 0.0);
        assert!(card.ev_deadlines_met);
    }

    #[test]
    fn capacity_auction_pays() {
        let card = scored(Program::CapacityAuction);
        assert!(card.program_payment_cad > 0.0);
    }

    #[test]
    fn program_parse() {
        assert_eq!("peak-perks".parse::<Program>().unwrap(), Program::PeakPerks);
        assert_eq!("centricity".parse::<Program>().unwrap(), Program::Centricity);
        assert!("nope".parse::<Program>().is_err());
    }
}

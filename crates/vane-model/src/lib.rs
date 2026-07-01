//! `vane-model` — the data model for a simulated DER neighborhood.
//!
//! Two layers:
//! * [`spec`] — what you write in `neighborhood.toml` (population fractions and
//!   per-asset-class parameters).
//! * [`neighborhood`] — the materialized population of homes and asset instances
//!   produced by the seeded [`neighborhood::generate`] function.
//!
//! Plus shared [`time`] primitives ([`Granularity`], [`Window`]).

pub mod neighborhood;
pub mod spec;
pub mod time;

pub use neighborhood::{Asset, Battery, Ev, Home, Neighborhood, Solar, Thermostat};
pub use spec::NeighborhoodSpec;
pub use time::{Granularity, ParseError, Window};

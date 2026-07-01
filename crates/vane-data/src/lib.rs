//! `vane-data` — the data pipeline: *raw* Ontario grid + weather series.
//!
//! This crate owns time-series ingest only. It provides:
//! * [`series`] — [`DaySeries`] and the [`Scenario`] bundle the optimizer needs.
//! * [`synthetic`] — a deterministic summer-peak-day generator so the whole
//!   pipeline runs offline (the sandbox default).
//! * [`ieso`] — parsers for the real IESO public CSV formats (see DESIGN.md §11).
//! * [`fetch`] — a thin HTTP downloader for the IESO/weather endpoints.
//!
//! Turning these raw series into neighborhood baseline load and per-asset
//! availability is `vane-sim`'s job, not this crate's.

pub mod emissions;
pub mod fetch;
pub mod ieso;
pub mod series;
pub mod synthetic;
pub mod weather;

pub use series::{DaySeries, Scenario};

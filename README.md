# vane

A DER dispatch / virtual-power-plant (VPP) simulator grounded in Ontario's real
smart-grid programs. Simulate a neighborhood of ~200 homes (smart thermostats,
home batteries, EV chargers, rooftop solar), feed it Ontario grid + weather data,
and answer: **given a peak-shave target over the next N hours, which assets do I
dispatch, when, and how much — at minimum cost/discomfort?** Then score the plan
(achieved-vs-target MW, dollars, emissions avoided) under a real program's mechanics.

See [DESIGN.md](DESIGN.md) for the full architecture, data model, MILP formulation,
data sources, program mechanics, and out-of-scope notes.

## Quick start

```sh
cargo build

# Summarize a generated neighborhood
vane info examples/oakville-demo.toml

# Headline command: forecast → dispatch → settle → scorecard
vane simulate examples/oakville-demo.toml \
  --date 2024-08-01 --target-mw 0.15 --window 17:00-20:00 --program peak-perks

# Inspect the hour-by-hour dispatch schedule
vane dispatch examples/oakville-demo.toml \
  --date 2024-08-01 --target-mw 0.15 --window 17:00-20:00

# Forecasting: plan on a forecast, score against actuals. --forecast perfect
# (default) | baseline (pure-Rust climatology) | python (temperature-aware ML)
vane forecast examples/oakville-demo.toml --date 2024-07-15 --forecast python
vane simulate examples/oakville-demo.toml --date 2024-08-20 \
  --target-mw 0.32 --window 17:00-20:00 --forecast python

# Real IESO + Open-Meteo data (needs network). --source ieso also trains the
# forecaster on the real prior-14-day history.
vane fetch --date 2024-08-01 --out data
vane simulate examples/oakville-demo.toml --date 2024-08-01 \
  --target-mw 0.32 --window 17:00-20:00 --source ieso --forecast python
```

Runs default to a **deterministic synthetic** summer-peak day (no network). Use
`--source ieso` for real IESO demand + HOEP and Open-Meteo weather.

> A ~200-home neighborhood peaks near ~0.3–0.6 MW, so realistic targets are
> ~0.1–0.3 MW. Larger targets are handled gracefully (the target is a *soft*
> constraint) and the scorecard reports attainment honestly.

## Programs (`--program`)

| mode | what it models |
|------|----------------|
| `peak-perks` | Residential VPP (thermostats only); flat incentive, value = avoided cost |
| `capacity-auction` | Wholesale C&I DR: $/MW-day availability + $250/MWh test energy |
| `york-nwa` | Local energy market (York Region NWA design): capacity + DLMP energy |
| `centricity` | Alias of `york-nwa`, flagged design-intent (Centricity's rules are unpublished) |

## Architecture

Rust core + a thin Python forecaster (mirrors the `scrye` shape). Crates:
`vane-model` (data model) · `vane-data` (ingest: IESO/weather/synthetic) ·
`vane-sim` (asset physics) · `vane-optimize` (the MILP core, HiGHS via `good_lp`) ·
`vane-score` (program settlement) · `vane-forecast` (climatology + Python bridge) ·
`vane-cli` (`vane`). The forecaster is a stdlib-only Python subprocess over a JSON
contract (`python/vane_forecast/predict.py`) — swap in LightGBM without touching
the boundary.

## Status

Steps 1–7 of the build order (DESIGN.md §13) are implemented and tested end-to-end
(19 tests). Remaining: polish (`--out json`, more example neighborhoods).

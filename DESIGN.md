# vane — DESIGN.md

> A DER dispatch / virtual-power-plant (VPP) simulator grounded in Ontario's real smart-grid programs.
>
> **Status:** design draft. No implementation code until this settles.
> **Sibling project:** `scrye` (semantic video search) — same shape: a simple CLI, a real data pipeline, a deep math/ML core, and a crisp scored output.

---

## 1. What it does

Simulate a neighborhood of ~200 homes with a mix of distributed energy resources (DERs): smart thermostats, home batteries, EV chargers, rooftop solar. Feed it **real Ontario open data** (IESO historical demand & price, an emissions-intensity series, weather/solar irradiance). Then answer:

> *Given a peak-shaving target for the next N hours, which assets do I dispatch, when, and how much — to hit the target at minimum cost/discomfort?*

Then **score** the plan against what actually happened that day: achieved-vs-target MW, dollars, and emissions avoided — settled under the mechanics of a chosen real Ontario program.

**Headline CLI:**
```
vane simulate neighborhood.toml --date 2024-08-01 --target-mw 2.0 --window 17:00-20:00 --program peak-perks
```

---

## 2. Locked design decisions

| # | Decision | Choice | Why |
|---|----------|--------|-----|
| 1 | Where the interesting core sits | **MILP dispatch optimizer** (forecasting is a feeding layer; RL is out-of-scope) | The problem *is* an optimization: objective (min cost/discomfort) + constraints (asset physics, target). Bounded scope, explainable output, real math (duality → shadow prices = marginal $/MW value of each asset). RL is the unbounded time-sink we're avoiding. |
| 2 | Language split | **Rust core + thin Python forecaster** | Mirrors scrye. Rust owns CLI, ingest, asset simulation, the MILP (via `good_lp` → HiGHS), settlement/scoring, data model. Python does *only* demand/solar forecast inference. The interesting math stays in Rust. |
| 3 | Asset physical fidelity | **Medium** | Battery SoC + round-trip efficiency; thermostat as a simple thermal/rebound model; EV with a charge-by deadline; solar derated by irradiance. Realistic enough to be convincing, bounded enough to finish. |
| 4 | Program grounding | **Multi-program faithful** | Three selectable scoring modes, each modeling a real Ontario program's settlement mechanics (see §8). Peak Performance (commercial HVAC) cut from v1 — it needs commercial loads a residential neighborhood doesn't have. |

**Repo name:** `vane` — weathervane (reads conditions early → anticipate & dispatch ahead of peak) with a hidden "VPP". Runner-up was `kith`.

---

## 3. Architecture

```
                        ┌─────────────────────────────────────────────┐
                        │                  vane (Rust)                 │
                        │                                              │
  neighborhood.toml ───▶│  config      ─┐                              │
                        │  ingest      ─┤─▶ features ─▶ forecast ──────┼──┐
  IESO / weather cache ▶│  (data pipe)  │                (call)        │  │
                        │               │                              │  ▼
                        │  sim (assets) │              ┌───────────────┴──────────┐
                        │  optimize ◀───┴── forecast ──│  vane-forecast (Python)  │
                        │  (MILP core)      series      │  demand & solar models   │
                        │      │                        └──────────────────────────┘
                        │      ▼                                       │
                        │  settle / score ──▶ scorecard (stdout+json)  │
                        └──────────────────────────────────────────────┘
```

### Rust ↔ Python boundary (thin, data-oriented)

Following scrye's "thin Python service" pattern, but even lighter: **subprocess + Parquet/JSON handoff**, not PyO3.

- Rust writes a feature table (calendar features, lagged demand, temperature, irradiance, cloud cover) to a Parquet/JSON file.
- Rust invokes `vane-forecast predict --features f.parquet --out forecast.parquet` (a small Python CLI installed via `uv`).
- Python returns a forecast series (per-timestep predicted neighborhood demand + solar generation, with prediction intervals).
- Rust reads it back and feeds the MILP.

Rationale: keeps the boundary a clean serialization contract (easy to test, swap, or stub), avoids PyO3 build complexity for a hobby project, and lets the Python side evolve model choice freely. A `--forecast baseline` flag runs a pure-Rust naive forecaster so the whole tool works with zero Python for quick iteration.

### Rust crate layout (workspace)

```
vane/
├── crates/
│   ├── vane-cli/        # clap CLI, orchestration, output formatting
│   ├── vane-data/       # IESO + weather ingest, parse, local cache (data pipeline)
│   ├── vane-model/      # data model: neighborhood, assets, TOML schema (serde)
│   ├── vane-sim/        # asset physics simulation (battery/thermostat/EV/solar)
│   ├── vane-optimize/   # MILP formulation + good_lp/HiGHS solve (the core)
│   ├── vane-score/      # program settlement modes + scorecard
│   └── vane-forecast/   # Rust side of the forecast boundary (features + naive baseline + Python bridge)
├── python/
│   └── vane_forecast/   # thin Python forecaster (uv project)
├── data/                # local cache of fetched IESO/weather series (gitignored)
├── examples/            # sample neighborhood.toml files
└── DESIGN.md
```

**Key Rust crates:** `clap` (CLI), `serde`/`toml`/`serde_json` (config + boundary), `polars` or `csv`+`arrow` (data), `good_lp` with the `highs` backend (MILP), `reqwest` (fetch), `chrono`/`chrono-tz` (time, America/Toronto), `anyhow`/`thiserror`.

**Python:** `uv`-managed; `pandas`, `scikit-learn`/`statsmodels` (forecast), optionally `pvlib` (solar geometry). Kept deliberately thin.

---

## 4. CLI surface

Scrye-like: one headline verb, plus the pipeline stages exposed as subcommands for inspection.

```
vane fetch     --from 2024-06-01 --to 2024-09-30 [--dataset demand,price,emissions,weather]
                 # pull IESO + weather series into ./data cache

vane forecast  neighborhood.toml --date 2024-08-01 --window 17:00-20:00 [--model baseline|python]
                 # produce demand + solar forecast for the window; prints/writes series

vane dispatch  neighborhood.toml --date 2024-08-01 --target-mw 2.0 --window 17:00-20:00
                 # run the MILP; emit the dispatch schedule (which assets, when, how much)

vane score     <dispatch.json> --program peak-perks
                 # settle the schedule against actuals under a program's mechanics

vane simulate  neighborhood.toml --date 2024-08-01 --target-mw 2.0 --window 17:00-20:00 \
               --program peak-perks
                 # one-shot: forecast → dispatch → settle → scorecard (the headline command)
```

Global flags: `--granularity 1h|15m` (default `1h` — matches most IESO series; 15-min available for finer runs), `--out json|table` (default `table`), `--seed <n>` (neighborhood generation).

---

## 5. Data model — `neighborhood.toml`

Describes the population and its assets. A generator can synthesize ~200 homes from distributions given a `--seed`, or homes can be listed explicitly.

```toml
[neighborhood]
name = "Oakville-demo"
homes = 200
timezone = "America/Toronto"
# Weather/irradiance location (drives solar + thermostat thermal load)
lat = 43.45
lon = -79.68

# Asset penetration across the population (fractions), used by the generator
[penetration]
smart_thermostat = 0.55   # of homes with central AC / ASHP
home_battery     = 0.12
ev_charger       = 0.25
rooftop_solar    = 0.18

# --- Asset class parameters (medium fidelity) ---

[assets.thermostat]
# curtailment comes from cooling load reduction during setback
max_setback_c        = 2.0     # Peak Perks: up to 2 °C above setpoint
precool_c            = -1.0    # optional 30-min pre-cool
avg_reduction_kw     = 0.59    # per-thermostat measured kW (IESO PY2024 evaluation)
snapback_kw          = 0.173   # added load, first hour after event (rebound)
discomfort_weight    = 1.0     # λ multiplier on degree-hours of setback

[assets.battery]
capacity_kwh         = 13.5    # e.g. Powerwall-class
power_kw             = 5.0     # charge/discharge rating
roundtrip_eff        = 0.90    # η = sqrt split into charge/discharge
soc_init_frac        = 0.5
soc_min_frac         = 0.10
cycle_cost_per_kwh   = 0.02    # degradation cost on throughput

[assets.ev_charger]
power_kw             = 7.2     # L2
required_kwh         = 30.0    # energy needed by deadline
deadline             = "07:00" # must be met by next morning
plugin               = "18:00"

[assets.solar]
capacity_kw_dc       = 6.0
inverter_kw_ac       = 5.0
derate               = 0.85    # system losses; further scaled by irradiance/temperature
```

### Asset physics (medium fidelity)

- **Rooftop solar** — *non-dispatchable generation*, not a decision variable. Output per timestep = f(GHI/DNI, panel derate, temperature derate). Reduces net load; a given input to the optimizer.
- **Home battery** — decision variable. SoC dynamics with round-trip efficiency, power and energy bounds, cannot charge & discharge simultaneously, degradation cost per kWh throughput.
- **EV charger** — decision variable. Shiftable charging power bounded by rating; hard constraint that required energy is delivered by the deadline. Cheapest hours preferred; delaying incurs no discomfort until the deadline.
- **Smart thermostat** — decision variable (curtailment). Available reduction per timestep bounded by a simple thermal model driven by outdoor temperature (hotter → more AC load available to shed); discomfort penalty ∝ degree-hours of setback; **rebound/snapback** load added after the event (a real, measured effect — see §8).

---

## 6. The MILP core (`vane-optimize`)

The heart of the project. Sketch of the formulation (exact form to be refined in implementation).

**Time index** `t ∈ T` over the window at `Δt` granularity. **Assets** grouped: batteries `B`, EVs `V`, thermostat cohorts `H`. Solar is exogenous.

**Decision variables**
- Battery `b`: `pch[b,t] ≥ 0`, `pdis[b,t] ≥ 0`, SoC `e[b,t]`, binary `y[b,t]` (charge/discharge lock).
- EV `v`: `pev[v,t] ≥ 0`.
- Thermostat cohort `h`: curtailment `c[h,t] ≥ 0`.
- Target shortfall `s[t] ≥ 0` (soft target — keeps the problem always feasible and makes achieved-vs-target visible).

**Net grid load** at `t`:
```
grid[t] = base_demand[t] − solar[t]
          + Σ_b (pch[b,t] − pdis[b,t])
          + Σ_v pev[v,t]
          − Σ_h c[h,t]
          + Σ_h snapback[h,t]        # rebound after setback
```

**Reduction vs. baseline** (baseline = same net load with no dispatch):
```
reduction[t] = base_net[t] − grid[t]
```

**Constraints**
- Target (soft): `reduction[t] + s[t] ≥ target_mw`  for `t` in the peak window.
- Battery SoC: `e[b,t] = e[b,t−1] + η·pch[b,t]·Δt − (1/η)·pdis[b,t]·Δt`; `soc_min ≤ e ≤ capacity`; power bounds; `pch ≤ M·y`, `pdis ≤ M·(1−y)`.
- EV energy-by-deadline: `Σ_{t ≤ deadline} pev[v,t]·Δt ≥ required_kwh[v]`; `pev ≤ rating`.
- Thermostat: `0 ≤ c[h,t] ≤ available_reduction[h,t]` (from thermal model); snapback tied to prior curtailment.

**Objective** (minimize):
```
Σ_t price[t]·grid[t]·Δt                     # energy cost / wholesale exposure
  + λ · Σ_h discomfort(c[h,t])              # thermostat degree-hours
  + κ · Σ_b cycle_cost·(pch+pdis)·Δt        # battery degradation
  + μ · Σ_t s[t]                            # target shortfall penalty (μ large)
  − incentive_revenue(program)              # program-specific reward (see §8)
```

**Interesting output beyond the schedule:** shadow prices on the target constraint give the **marginal $/MW of shaving** at each hour; reduced costs rank assets by marginal value. That's a genuinely useful, explainable artifact — and the reason MILP beats a black-box policy here.

Solver: `good_lp` with the **HiGHS** backend (strong open-source MILP). Problem size (~200 homes × a handful of hourly timesteps × few vars/asset) is comfortably tractable; cohort-aggregating thermostats keeps the binary count down.

---

## 7. Where the ML sits (`vane-forecast` + `python/`)

Deliberately **subordinate** to the optimizer, and staged:

- **Stage 0 (default, pure Rust):** naive baseline forecast — hour-of-day × day-of-week demand profile from the cache; clear-sky solar scaled by forecast cloud cover. Lets the tool run end-to-end with no Python and proves the optimizer is the star.
- **Stage 1 (Python):** a real trained model on IESO history — gradient-boosted trees (LightGBM) or a seasonal model (statsmodels) using calendar + weather features to predict neighborhood demand and solar, with prediction intervals. This is the "thin Python ML inference" layer, mirroring scrye.

The forecast feeds the MILP's `base_demand[t]` and `solar[t]`. Because scoring replays a *historical* day (§9), we can also run the optimizer on **perfect foresight** (actuals) vs. **forecast** and report the cost of forecast error — a nice built-in evaluation.

**Implemented (build-order step 7).** `--forecast perfect|baseline|python`. Stage 0 (`baseline`) is pure-Rust climatology (per-hour mean of the prior 14 days). Stage 1 (`python`) is a subprocess over a JSON contract (`python/vane_forecast/predict.py`): a stdlib-only per-hour OLS of demand on temperature — swap in LightGBM without touching the boundary. Forecast modes **plan** on the forecast, then **replay** those committed controls under actuals; the scorecard reports achieved-vs-perfect MW.

A subtlety worth recording: achieved *reduction* is defined from the control decisions (curtailment + battery + EV shift), so a demand/solar level error **cancels out** and doesn't change achieved reduction. The channel through which forecast skill becomes dispatch value is **temperature → available thermostat curtailment**: on a hot day, climatology under-provisions cheap curtailment and falls short near the feasibility frontier, while the temperature-aware model matches perfect foresight. So forecast value is real but modest under a *reduction-delta* target; it would be larger under an *absolute-peak-cap* target (a natural follow-up). The forecaster's headline win is accuracy: the temperature-aware model roughly halves demand MAE vs climatology on anomalous days.

---

## 8. Program scoring modes (`vane-score`) — grounded in verified mechanics

Each `--program` selects a settlement model. **All figures below were verified against primary IESO/Save-on-Energy sources in July 2026;** items marked ⚠️ are secondary-only or unverified and are encoded as clearly-labeled, overridable placeholders — *not* asserted fact.

### `peak-perks` — residential VPP (thermostats only)
- **Operator:** IESO / Save on Energy, delivered by EnergyHub. Active & enrolling; ~280k thermostats (Q4 2025).
- **Eligibility:** smart thermostats **only** (central AC or ASHP), max 3/home. *No batteries/EV/water-heaters* — so in this mode only thermostat assets are dispatchable.
- **Events:** summer only (Jun 1–Sep 30), weekdays, typically 12:00–21:00, **max 3 h**, ~10 events/yr; optional −1 °C pre-cool 30 min prior; setback **up to 2 °C**; opt-out anytime, no per-event penalty.
- **Incentive:** flat **$75** enrollment + **$20/yr** — **no per-event payment**. (So "dollars saved" in this mode is dominated by *avoided wholesale/system cost*, not participant payments.)
- **M&V (scoring):** RCT + difference-in-differences on whole-home AMI data. Encode the evaluation's realism factors: measured **≈0.59 kW/thermostat** (2024); reported impacts scaled by **0.60** to match evaluated; **pre-cool +0.546 kW/thermostat**; **snapback +0.173 kW hour 1**. Program totals for sanity: ~82 MW avg, 101 MW peak (2024).
- ⚠️ Never published: quantitative event trigger (MW/temp threshold) and committed advance-notice lead time. Older copy says 2–4 °C setback; use 2 °C (current FAQ).

> **Peak Performance (commercial HVAC DR) — cut from v1.** It's a real, distinct Save-on-Energy program (first season Jun 2026, Class B C&I, ≥75% HVAC, min 500 kW, day-ahead notice), but it settles against *commercial* loads a residential neighborhood doesn't model. Deferred rather than stubbed to keep v1 honest. Verified mechanics are recorded here for a possible later commercial-neighborhood mode.

### `capacity-auction` — the mature wholesale C&I DR mechanism
The rigorous settlement model; useful even though it's not a "residential VPP," because its **baseline math is the real thing**.
- **Baseline (CBL):** `C&I_HDR_BL = StdBL × IDAF`.
  - `StdBL` = **High-15-of-20**: 20 most recent suitable business days, keep the 15 highest-consumption, average per hour-ending.
  - `IDAF` = In-Day Adjustment Factor = (event-day pre-window 3-h avg) ÷ (same window across the 15-of-20 days), **multiplicative, capped [0.80, 1.20]**. *(Correction to a common assumption: it is scalar/multiplicative, not symmetric-additive.)*
- **Performance:** `Curtailed MWh = max(0, CBL − actual)`; test pass = within a **15% dead-band** each interval.
- **Payment:** availability at auction clearing price in **$/MW-day** + test/emergency energy at **HDRTAPR = $250/MWh**.
- **Penalties:** availability/dispatch/admin charges scaled by **CNPF seasonal multiplier** (Jul/Aug/Sep = 2.0, Mar/Jun/Dec = 1.5, else 1.0); Performance Adjustment Factor floor 0.75.
- **Reference clearing prices:** Summer 2025 **$332.39/MW-day** (verified). ⚠️ 2025 auction (delivery 2026-27) clearing prices reported as record highs are **secondary-only** — flag on encode.

### `york-nwa` — local energy market (the real basis for "Centricity")
- **Important correction:** *Centricity* (launched Aug 2025, NRCan-funded, admin location **Mississauga** — **not** Oakville; Oakville Enterprises is a partner) has **no published market mechanics or results**. The encodable mechanics come from its documented predecessor, the **IESO/Alectra York Region NWA Demonstration** (2021–2022, evaluated Jul 2024). This mode is labeled accordingly.
- **Mechanics:** two-part market — a **uniform-price capacity auction** (**$0.64/kW-day** in 2021, **$0.40** in 2022) **plus** a daily **local energy auction** priced at a **Distribution LMP** with a **$2.00/kWh ceiling**; **direct dispatch** by the utility. Two-settlement stack (capacity availability + energy activation), with **performance (~85%)** and **availability (~83%)** multipliers on payout.
- A `centricity` alias runs the same math with a `# DESIGN-INTENT / UNSPECIFIED` banner, since the live program's rules aren't public.

---

## 9. Scoring & output

**Scoring = historical replay.** Pick a real IESO date. The "actual" demand/price/emissions is the historical record; vane's dispatch is the counterfactual, settled under the chosen program and compared to a **no-action baseline**.

**Scorecard** (table by default, `--out json` for machine use):
```
vane — Oakville-demo — 2024-08-01 — window 17:00–20:00 — program peak-perks

  Target shave         2.00 MW
  Achieved (avg)       1.83 MW      (91.5% of target)
  Achieved (min hour)  1.61 MW
  Dispatched assets    110 thermostats, 24 batteries, 41 EVs deferred
  Wholesale cost saved $ 3,140      (avoided energy at LMP/HOEP)
  Program payments     $ 0          (Peak Perks pays flat, not per-event)
  Emissions avoided    0.42 tCO2e   (AVERAGE intensity — see caveat)
  Discomfort           118 °C·h setback; EV deadlines all met
  Forecast error cost  $ 190        (vs perfect-foresight dispatch)

  Marginal value of the last MW shaved: $612/MW (hour 18:00)
```

**Emissions caveat (honest):** IESO does not publish marginal emissions intensity, and free marginal series were discontinued (§10). vane computes **average** intensity from The Atmospheric Fund's hourly factors (2014–2023) or from IESO fuel-mix data. The scorecard labels emissions "AVERAGE-based" — a real limitation, not marginal displacement. A `--emissions marginal-heuristic` flag can approximate gas-on-margin from fuel-mix data, clearly labeled as an estimate.

---

## 10. Out of scope (v1)

- **RL / learned dispatch policies.** Explicitly deferred. Possible future benchmark: *can a learned policy beat the LP optimum?*
- **Real-time / live operation.** vane replays historical days; no live IESO feed, no real device control.
- **Commercial load modeling & the Peak Performance program.** Cut from v1 — no commercial building thermal models, so the commercial HVAC DR program is deferred (verified mechanics kept in §8 for later).
- **High-fidelity physics.** No RC thermal networks, battery degradation curves, per-home stochastic occupancy, or EV arrival distributions. Medium fidelity only (§5).
- **Network/power-flow modeling.** No AC/DC load flow, voltage, or distribution constraints. Peak-shaving is treated as an aggregate MW target, not a nodal problem.
- **Post-MRP nodal (LMP) backtests beyond ~3 months.** Public LMP data is retained only ~3 months; deep historical replay uses the **HOEP archive (2002–Apr 2025)**. Continuous post-MRP history would require scraping the rolling directory over time or an archive source we haven't confirmed.
- **Billing-accurate resident savings.** Residential bills follow OEB RPP (TOU/ULO/Tiered), not wholesale; v1 reports wholesale-cost-avoided, with RPP bill modeling a possible later mode.

---

## 11. Data sources (verified July 2026)

Canonical IESO public host: **`reports-public.ieso.ca`** (older `reports.ieso.ca/public/...` links redirect here).

| Series | Source | Format / granularity | Notes |
|--------|--------|----------------------|-------|
| Demand (Ontario + Market) | `reports-public.ieso.ca/public/Demand/` | CSV, hourly, 2002– | `PUB_Demand_YYYY.csv` |
| Zonal demand (Toronto zone ≈ Oakville) | `reports-public.ieso.ca/public/DemandZonal/` | CSV, hourly, 2003– | |
| Price — HOEP (deep archive) | `reports-public.ieso.ca/public/PriceHOEPPredispOR/` | CSV, hourly, 2002–Apr 2025 | **Primary price series for historical replay** |
| Price — post-MRP LMP / Zonal | `.../RealtimeEnergyLMP/`, `.../DAHourlyOntarioZonalPrice/` etc. | CSV/XML, 5-min/hourly | ⚠️ **rolling ~3-month retention only** |
| Fuel mix (for emissions) | `reports-public.ieso.ca/public/GenOutputbyFuelHourly/` | XML, hourly, ~2015– | compute average intensity yourself |
| Emissions factors (average) | The Atmospheric Fund "Ontario Electricity Emissions Factors 2024" | PDF/interactive, hourly, 2014–2023 | best free source; **average, not marginal** |
| Temperature | ECCC bulk data (`climate.weather.gc.ca .../bulk_data_e.html?...timeframe=1`) | CSV, hourly | OGL-Canada; no irradiance |
| Solar GHI/DNI/DHI | Open-Meteo Archive API (`archive-api.open-meteo.com/v1/archive`) | JSON/CSV, hourly, 1940– | CC-BY, free non-commercial; ERA5 ~9–25 km |
| Solar (finer alt.) | NREL NSRDB (`developer.nrel.gov/docs/solar/nsrdb/`) | CSV, 30/10-min | covers southern Ontario; free w/ citation |

**Price regime note (verified):** Market Renewal (LMP, Single Schedule Market) went live **May 1, 2025**; HOEP was replaced by the OEMP/"Ontario Price". Residential customers pay OEB **RPP** (TOU/ULO/Tiered), never wholesale — vane treats wholesale (LMP/HOEP) and RPP as distinct price layers.

---

## 12. Open verification TODOs (before publishing claims / hard-coding)

Carried forward from research; confirm against live primary sources at implementation time:

1. Exact 2026 per-kWh RPP TOU/ULO/Tiered rates (OEB `rpp-price-report-20251017.pdf`).
2. Peak Performance `$/MW-season` incentive and 2026 enrollment deadline — **live saveonenergy.ca page** (currently secondary-only).
3. 2025 Capacity Auction clearing prices (secondary-only; confirm on IESO).
4. Post-MRP LMP archive location for data older than the rolling ~3-month window.
5. NSRDB precise coverage bounding box for the Oakville/Toronto point.
6. Peak Perks current eligible-thermostat model list (verify against live Eligible List).

---

## 13. Build order (suggested)

1. `vane-model` + example `neighborhood.toml` + generator.
2. `vane-data` fetch/parse/cache for demand + HOEP + weather (the real data pipeline).
3. `vane-sim` asset physics (medium fidelity) + `vane-forecast` naive baseline.
4. `vane-optimize` MILP with `good_lp`/HiGHS — the core; `dispatch` command working end-to-end.
5. `vane-score` with `peak-perks` first (simplest, best-sourced M&V), then `capacity-auction`, `york-nwa`.
6. `vane simulate` one-shot wiring + scorecard.
7. Python forecaster (Stage 1) + forecast-error reporting. ✅
8. Remaining polish (`--out json`, docs, more example neighborhoods).

Steps 1–7 implemented and tested end-to-end (19 tests). Step 8 remains.

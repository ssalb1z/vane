//! The materialized population: concrete [`Home`]s holding concrete [`Asset`]
//! instances, produced deterministically from a [`NeighborhoodSpec`] and a seed.

use chrono::NaiveTime;
use rand::Rng;
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha8Rng;

use crate::spec::NeighborhoodSpec;

#[derive(Debug, Clone)]
pub struct Neighborhood {
    pub name: String,
    pub timezone: String,
    pub lat: f64,
    pub lon: f64,
    pub homes: Vec<Home>,
}

#[derive(Debug, Clone)]
pub struct Home {
    pub id: usize,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone)]
pub enum Asset {
    Thermostat(Thermostat),
    Battery(Battery),
    Ev(Ev),
    Solar(Solar),
}

/// A dispatchable curtailment resource. `avail_reduction_kw` is the *nameplate*
/// per-thermostat reduction; the simulator scales it by outdoor temperature.
#[derive(Debug, Clone)]
pub struct Thermostat {
    pub max_setback_c: f64,
    pub precool_c: f64,
    pub avail_reduction_kw: f64,
    pub snapback_kw: f64,
    pub discomfort_weight: f64,
}

#[derive(Debug, Clone)]
pub struct Battery {
    pub capacity_kwh: f64,
    pub power_kw: f64,
    pub roundtrip_eff: f64,
    pub soc_init_kwh: f64,
    pub soc_min_kwh: f64,
    pub cycle_cost_per_kwh: f64,
}

#[derive(Debug, Clone)]
pub struct Ev {
    pub power_kw: f64,
    pub required_kwh: f64,
    pub deadline: NaiveTime,
    pub plugin: NaiveTime,
}

#[derive(Debug, Clone)]
pub struct Solar {
    pub capacity_kw_dc: f64,
    pub inverter_kw_ac: f64,
    pub derate: f64,
}

/// Counts and capacities of a generated population — handy for CLI summaries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    pub homes: usize,
    pub thermostats: usize,
    pub batteries: usize,
    pub evs: usize,
    pub solar: usize,
    pub battery_capacity_kwh: f64,
    pub battery_power_kw: f64,
    pub solar_capacity_kw_dc: f64,
}

impl Neighborhood {
    pub fn summary(&self) -> Summary {
        let mut s = Summary {
            homes: self.homes.len(),
            ..Default::default()
        };
        for home in &self.homes {
            for asset in &home.assets {
                match asset {
                    Asset::Thermostat(_) => s.thermostats += 1,
                    Asset::Battery(b) => {
                        s.batteries += 1;
                        s.battery_capacity_kwh += b.capacity_kwh;
                        s.battery_power_kw += b.power_kw;
                    }
                    Asset::Ev(_) => s.evs += 1,
                    Asset::Solar(pv) => {
                        s.solar += 1;
                        s.solar_capacity_kw_dc += pv.capacity_kw_dc;
                    }
                }
            }
        }
        s
    }
}

/// Multiply `base` by a uniform factor in `[1-frac, 1+frac]` so instances vary
/// realistically around the class default. `frac = 0.0` returns `base`.
fn jitter(rng: &mut ChaCha8Rng, base: f64, frac: f64) -> f64 {
    if frac <= 0.0 {
        return base;
    }
    base * (1.0 + rng.gen_range(-frac..frac))
}

fn parse_time(s: &str, field: &str) -> anyhow::Result<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M")
        .map_err(|e| anyhow::anyhow!("bad {field} time {s:?}: {e}"))
}

/// Materialize a population from `spec`, deterministically for a given `seed`.
///
/// Each home rolls independently for each asset class against its penetration
/// fraction; instance parameters are the class defaults jittered ±10% (except
/// efficiencies and fractions, which are physically bounded and left exact).
pub fn generate(spec: &NeighborhoodSpec, seed: u64) -> anyhow::Result<Neighborhood> {
    const JITTER: f64 = 0.10;

    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let p = &spec.penetration;
    let a = &spec.assets;

    let p_th = p.smart_thermostat.clamp(0.0, 1.0);
    let p_bat = p.home_battery.clamp(0.0, 1.0);
    let p_ev = p.ev_charger.clamp(0.0, 1.0);
    let p_pv = p.rooftop_solar.clamp(0.0, 1.0);

    let ev_deadline = parse_time(&a.ev_charger.deadline, "ev deadline")?;
    let ev_plugin = parse_time(&a.ev_charger.plugin, "ev plugin")?;

    let mut homes = Vec::with_capacity(spec.neighborhood.homes);
    for id in 0..spec.neighborhood.homes {
        let mut assets = Vec::new();

        if rng.gen_bool(p_th) {
            assets.push(Asset::Thermostat(Thermostat {
                max_setback_c: a.thermostat.max_setback_c,
                precool_c: a.thermostat.precool_c,
                avail_reduction_kw: jitter(&mut rng, a.thermostat.avg_reduction_kw, JITTER),
                snapback_kw: jitter(&mut rng, a.thermostat.snapback_kw, JITTER),
                discomfort_weight: a.thermostat.discomfort_weight,
            }));
        }
        if rng.gen_bool(p_bat) {
            let capacity_kwh = jitter(&mut rng, a.battery.capacity_kwh, JITTER);
            assets.push(Asset::Battery(Battery {
                capacity_kwh,
                power_kw: jitter(&mut rng, a.battery.power_kw, JITTER),
                roundtrip_eff: a.battery.roundtrip_eff,
                soc_init_kwh: capacity_kwh * a.battery.soc_init_frac,
                soc_min_kwh: capacity_kwh * a.battery.soc_min_frac,
                cycle_cost_per_kwh: a.battery.cycle_cost_per_kwh,
            }));
        }
        if rng.gen_bool(p_ev) {
            assets.push(Asset::Ev(Ev {
                power_kw: jitter(&mut rng, a.ev_charger.power_kw, JITTER),
                required_kwh: jitter(&mut rng, a.ev_charger.required_kwh, JITTER),
                deadline: ev_deadline,
                plugin: ev_plugin,
            }));
        }
        if rng.gen_bool(p_pv) {
            assets.push(Asset::Solar(Solar {
                capacity_kw_dc: jitter(&mut rng, a.solar.capacity_kw_dc, JITTER),
                inverter_kw_ac: jitter(&mut rng, a.solar.inverter_kw_ac, JITTER),
                derate: a.solar.derate,
            }));
        }

        homes.push(Home { id, assets });
    }

    Ok(Neighborhood {
        name: spec.neighborhood.name.clone(),
        timezone: spec.neighborhood.timezone.clone(),
        lat: spec.neighborhood.lat,
        lon: spec.neighborhood.lon,
        homes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML: &str = r#"
[neighborhood]
name = "test"
homes = 200
timezone = "America/Toronto"
lat = 43.45
lon = -79.68

[penetration]
smart_thermostat = 0.55
home_battery = 0.12
ev_charger = 0.25
rooftop_solar = 0.18

[assets.thermostat]
max_setback_c = 2.0
precool_c = -1.0
avg_reduction_kw = 0.59
snapback_kw = 0.173
discomfort_weight = 1.0

[assets.battery]
capacity_kwh = 13.5
power_kw = 5.0
roundtrip_eff = 0.90
soc_init_frac = 0.5
soc_min_frac = 0.10
cycle_cost_per_kwh = 0.02

[assets.ev_charger]
power_kw = 7.2
required_kwh = 30.0
deadline = "07:00"
plugin = "18:00"

[assets.solar]
capacity_kw_dc = 6.0
inverter_kw_ac = 5.0
derate = 0.85
"#;

    #[test]
    fn parses_and_generates() {
        let spec = NeighborhoodSpec::from_toml_str(TOML).unwrap();
        let n = generate(&spec, 42).unwrap();
        assert_eq!(n.homes.len(), 200);
        let s = n.summary();
        assert_eq!(s.homes, 200);
        // Roughly matches penetration on 200 homes (loose bounds).
        assert!((70..=150).contains(&s.thermostats), "thermostats={}", s.thermostats);
        assert!(s.battery_capacity_kwh > 0.0);
    }

    #[test]
    fn generation_is_deterministic() {
        let spec = NeighborhoodSpec::from_toml_str(TOML).unwrap();
        let a = generate(&spec, 7).unwrap().summary();
        let b = generate(&spec, 7).unwrap().summary();
        assert_eq!(a, b);
        // Different seed almost surely differs on 200 homes.
        let c = generate(&spec, 8).unwrap().summary();
        assert_ne!(a, c);
    }
}

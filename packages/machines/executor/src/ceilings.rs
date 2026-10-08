//! The most of each allowance one agent is given on this machine.
//!
//! The allowances are the author's to set, and nothing on the chain bounds
//! them: a vault could ask for gigabytes of memory on a machine it shares
//! with others. The executor holds each allowance to a ceiling of its own,
//! runs the agent at what it is held to, bills only that, and says so in the
//! vault's health report. Gas is not held here: it is the vault's own money,
//! capped by the contract, and holds no capacity on the machine.

use shared::Allowances;

/// The ceilings, each one settable when the executor is started.
#[derive(Clone, Copy, Debug, PartialEq, clap::Args)]
pub struct Ceilings {
    /// The most fuel a second one agent is given.
    #[arg(long = "ceiling-fuel-per-sec", default_value_t = Ceilings::FUEL_PER_SEC)]
    pub fuel_per_sec: u64,
    /// The most memory one agent may hold, in MiB.
    #[arg(long = "ceiling-memory-mib", default_value_t = Ceilings::MEMORY_MIB)]
    pub memory_mib: u32,
    /// The most requests a minute one agent is given.
    #[arg(long = "ceiling-requests-per-min", default_value_t = Ceilings::REQUESTS_PER_MIN)]
    pub requests_per_min: u32,
    /// The most bytes a day one agent may move.
    #[arg(long = "ceiling-bytes-per-day", default_value_t = Ceilings::BYTES_PER_DAY)]
    pub bytes_per_day: u64,
    /// The most streams one agent may hold open.
    #[arg(long = "ceiling-streams", default_value_t = Ceilings::STREAMS)]
    pub streams: u32,
    /// The most inference time a minute one agent is given, in milliseconds.
    #[arg(
        long = "ceiling-inference-ms-per-min",
        default_value_t = Ceilings::INFERENCE_MS_PER_MIN
    )]
    pub inference_ms_per_min: u32,
}

impl Ceilings {
    /// Four times the quoting shape's.
    pub const FUEL_PER_SEC: u64 = 64_000_000;
    /// 256 MiB: twice the quoting shape's, and room for an agent's models
    /// (at most 128 MiB in all, counted against its memory) and its own
    /// working memory beside them. An allowance asking for more (the
    /// thinking shape's 512 MiB) runs, and is billed, at this.
    pub const MEMORY_MIB: u32 = 256;
    /// Twice the quoting shape's.
    pub const REQUESTS_PER_MIN: u32 = 12_000;
    /// 50 GB: ten times the quoting shape's.
    pub const BYTES_PER_DAY: u64 = 50_000_000_000;
    /// Four times the quoting shape's.
    pub const STREAMS: u32 = 256;
    /// A whole minute a minute: one processor's worth.
    pub const INFERENCE_MS_PER_MIN: u32 = 60_000;

    /// What an agent asking for `asked` is given here: each allowance held
    /// to its ceiling, gas as asked.
    pub fn hold(&self, asked: &Allowances) -> Allowances {
        Allowances {
            fuel_per_sec: asked.fuel_per_sec.min(self.fuel_per_sec),
            memory_mib: asked.memory_mib.min(self.memory_mib),
            requests_per_min: asked.requests_per_min.min(self.requests_per_min),
            bytes_per_day: asked.bytes_per_day.min(self.bytes_per_day),
            streams: asked.streams.min(self.streams),
            inference_ms_per_min: asked.inference_ms_per_min.min(self.inference_ms_per_min),
            gas_per_day: asked.gas_per_day,
        }
    }

    /// Each allowance `asked` that is held below what it asks for, as
    /// "what (asked, held to)", for the health report. Empty when nothing is.
    pub fn held_below(&self, asked: &Allowances) -> Vec<String> {
        let held = self.hold(asked);
        let mut below = Vec::new();
        let mut note = |what: &str, asked: u64, held: u64, unit: &str| {
            if held < asked {
                below.push(format!("{what} {asked} {unit}, held to {held}"));
            }
        };
        note("fuel", asked.fuel_per_sec, held.fuel_per_sec, "a second");
        note(
            "memory",
            asked.memory_mib.into(),
            held.memory_mib.into(),
            "MiB",
        );
        note(
            "requests",
            asked.requests_per_min.into(),
            held.requests_per_min.into(),
            "a minute",
        );
        note(
            "bandwidth",
            asked.bytes_per_day,
            held.bytes_per_day,
            "bytes a day",
        );
        note("streams", asked.streams.into(), held.streams.into(), "");
        note(
            "inference",
            asked.inference_ms_per_min.into(),
            held.inference_ms_per_min.into(),
            "ms a minute",
        );
        below
    }
}

impl Default for Ceilings {
    fn default() -> Self {
        Self {
            fuel_per_sec: Self::FUEL_PER_SEC,
            memory_mib: Self::MEMORY_MIB,
            requests_per_min: Self::REQUESTS_PER_MIN,
            bytes_per_day: Self::BYTES_PER_DAY,
            streams: Self::STREAMS,
            inference_ms_per_min: Self::INFERENCE_MS_PER_MIN,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_an_author_picks_between_is_given_in_full_but_memory_past_the_ceiling() {
        let ceilings = Ceilings::default();
        for name in Allowances::SHAPES {
            let shape = Allowances::shape(name);
            let held = ceilings.hold(&shape);
            assert_eq!(
                held,
                Allowances {
                    memory_mib: shape.memory_mib.min(Ceilings::MEMORY_MIB),
                    ..shape
                },
                "{name}"
            );
            let below = ceilings.held_below(&shape);
            if shape.memory_mib > Ceilings::MEMORY_MIB {
                assert_eq!(below.len(), 1, "{name}: {below:?}");
                assert!(below[0].starts_with("memory"), "{name}: {below:?}");
            } else {
                assert!(below.is_empty(), "{name}: {below:?}");
            }
        }
    }

    #[test]
    fn the_thinking_shape_s_memory_is_held_to_the_ceiling() {
        let thinking = Allowances::thinking();
        assert!(thinking.memory_mib > Ceilings::MEMORY_MIB);
        let held = Ceilings::default().hold(&thinking);
        assert_eq!(held.memory_mib, 256);
        assert_eq!(held.fuel_per_sec, thinking.fuel_per_sec);
    }

    #[test]
    fn an_allowance_past_its_ceiling_is_held_to_it() {
        let asked = Allowances {
            memory_mib: u32::MAX,
            inference_ms_per_min: u32::MAX,
            gas_per_day: 1_000.0,
            ..Allowances::modest()
        };
        let held = Ceilings::default().hold(&asked);
        assert_eq!(held.memory_mib, Ceilings::MEMORY_MIB);
        assert_eq!(held.inference_ms_per_min, Ceilings::INFERENCE_MS_PER_MIN);
        // What is within its ceiling, and gas, are as asked.
        assert_eq!(held.fuel_per_sec, asked.fuel_per_sec);
        assert_eq!(held.gas_per_day, asked.gas_per_day);
        let below = Ceilings::default().held_below(&asked);
        assert_eq!(below.len(), 2, "{below:?}");
        assert!(below[0].starts_with("memory"), "{below:?}");
    }
}

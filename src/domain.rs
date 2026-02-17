use std::{collections::HashMap, fmt};

const REQUIRED_EVENTS: &[&str] = &[
    "Ir", "I1mr", "ILmr", "Dr", "D1mr", "DLmr", "Dw", "D1mw", "DLmw",
];

/// Parsed process invocation for the benchmark runner.
#[derive(Clone, Debug)]
pub(crate) struct Invocation {
    /// Binary path used to re-spawn child processes.
    pub(crate) executable: String,
    /// Whether this process is a parent or a target child run.
    pub(crate) mode: InvocationMode,
}

/// The invocation modes recognized by the runner.
#[derive(Clone, Debug)]
pub(crate) enum InvocationMode {
    /// Callgrind child: run all benchmarks sequentially under `--toggle-collect`.
    Child,
    /// Cachegrind child: run a single benchmark by index via `--iai-run <N>`.
    ChildIndexed { benchmark_index: isize },
    /// Primary harness invocation; drives all registered benches.
    Parent,
}

/// Parse process arguments into an invocation descriptor.
///
/// Expected shape:
/// - No special flags => parent mode
/// - `--iai-run` (no further arg) => callgrind child mode (all benchmarks)
/// - `--iai-run <N>` => cachegrind child mode (single benchmark by index)
pub(crate) fn parse_invocation<I>(mut args: I) -> Invocation
where
    I: Iterator<Item = String>,
{
    let executable = args.next().unwrap_or_default();

    match args.next() {
        Some(flag) if flag == "--iai-run" => match args.next() {
            Some(index_str) => {
                let benchmark_index = index_str.parse::<isize>().unwrap_or(-1);
                Invocation {
                    executable,
                    mode: InvocationMode::ChildIndexed { benchmark_index },
                }
            }
            None => Invocation {
                executable,
                mode: InvocationMode::Child,
            },
        },
        _ => Invocation {
            executable,
            mode: InvocationMode::Parent,
        },
    }
}

impl fmt::Display for Invocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mode {
            InvocationMode::Parent => write!(f, "{}", self.executable),
            InvocationMode::Child => write!(f, "{} --iai-run", self.executable),
            InvocationMode::ChildIndexed { benchmark_index } => {
                write!(f, "{} --iai-run {}", self.executable, benchmark_index)
            }
        }
    }
}

/// Valgrind cache-simulation counters shared by both cachegrind and callgrind.
///
/// Both tools emit the same 9 events (Ir, I1mr, ILmr, Dr, D1mr, DLmr, Dw, D1mw,
/// DLmw) so a single struct serves both profiling backends.
#[derive(Clone, Debug)]
pub(crate) struct BenchStats {
    pub(crate) instruction_reads: u64,
    pub(crate) instruction_l1_misses: u64,
    pub(crate) instruction_cache_misses: u64,
    pub(crate) data_reads: u64,
    pub(crate) data_l1_read_misses: u64,
    pub(crate) data_cache_read_misses: u64,
    pub(crate) data_writes: u64,
    pub(crate) data_l1_write_misses: u64,
    pub(crate) data_cache_write_misses: u64,
}

impl BenchStats {
    /// Build stats from parsed event values.
    ///
    /// Returns an error if any required event is missing.
    pub(crate) fn from_events(events: &HashMap<String, u64>) -> Result<Self, String> {
        let event_value = |key: &str| {
            events
                .get(key)
                .copied()
                .ok_or_else(|| format!("Missing required event: {key}"))
        };

        for key in REQUIRED_EVENTS {
            if !events.contains_key(*key) {
                return Err(format!("Missing required event: {key}"));
            }
        }

        Ok(Self {
            instruction_reads: event_value("Ir")?,
            instruction_l1_misses: event_value("I1mr")?,
            instruction_cache_misses: event_value("ILmr")?,
            data_reads: event_value("Dr")?,
            data_l1_read_misses: event_value("D1mr")?,
            data_cache_read_misses: event_value("DLmr")?,
            data_writes: event_value("Dw")?,
            data_l1_write_misses: event_value("D1mw")?,
            data_cache_write_misses: event_value("DLmw")?,
        })
    }

    /// RAM-level memory accesses derived from event counters.
    pub(crate) fn ram_accesses(&self) -> u64 {
        self.instruction_cache_misses + self.data_cache_read_misses + self.data_cache_write_misses
    }

    /// Reduce raw counters into cache hierarchy groupings used by reports.
    pub(crate) fn summarize(&self) -> BenchSummary {
        let ram_hits = self.ram_accesses();
        let l3_accesses =
            self.instruction_l1_misses + self.data_l1_read_misses + self.data_l1_write_misses;
        let l3_hits = l3_accesses - ram_hits;

        let total_memory_rw = self.instruction_reads + self.data_reads + self.data_writes;
        let l1_hits = total_memory_rw - (ram_hits + l3_hits);

        BenchSummary {
            l1_hits,
            l3_hits,
            ram_hits,
        }
    }

    /// Subtract calibration counters with saturating arithmetic.
    ///
    /// Used by the cachegrind backend to remove harness overhead from results.
    #[cfg(feature = "cachegrind")]
    pub(crate) fn subtract(&self, calibration: &BenchStats) -> BenchStats {
        BenchStats {
            instruction_reads: self
                .instruction_reads
                .saturating_sub(calibration.instruction_reads),
            instruction_l1_misses: self
                .instruction_l1_misses
                .saturating_sub(calibration.instruction_l1_misses),
            instruction_cache_misses: self
                .instruction_cache_misses
                .saturating_sub(calibration.instruction_cache_misses),
            data_reads: self.data_reads.saturating_sub(calibration.data_reads),
            data_l1_read_misses: self
                .data_l1_read_misses
                .saturating_sub(calibration.data_l1_read_misses),
            data_cache_read_misses: self
                .data_cache_read_misses
                .saturating_sub(calibration.data_cache_read_misses),
            data_writes: self.data_writes.saturating_sub(calibration.data_writes),
            data_l1_write_misses: self
                .data_l1_write_misses
                .saturating_sub(calibration.data_l1_write_misses),
            data_cache_write_misses: self
                .data_cache_write_misses
                .saturating_sub(calibration.data_cache_write_misses),
        }
    }
}

/// Derived cache summary values used for presentation.
#[derive(Clone, Debug)]
pub(crate) struct BenchSummary {
    pub(crate) l1_hits: u64,
    pub(crate) l3_hits: u64,
    pub(crate) ram_hits: u64,
}

impl BenchSummary {
    /// Estimate weighted memory-hierarchy cycles.
    pub(crate) fn cycles(&self) -> u64 {
        // Uses Itamar Turner-Trauring's formula from https://pythonspeed.com/articles/consistent-benchmarking-in-ci/
        self.l1_hits + (5 * self.l3_hits) + (35 * self.ram_hits)
    }
}

/// Compute relative percentage change between `new` and `old`.
///
/// Returns ` (N/A)` when `old == 0` to avoid divide-by-zero and keep output stable.
pub(crate) fn percentage_diff(new: u64, old: u64) -> String {
    if new == old {
        return " (No change)".to_owned();
    }

    if old == 0 {
        return " (N/A)".to_owned();
    }

    let new = new as f64;
    let old = old as f64;
    let pct = ((new - old) / old) * 100.0;

    format!(" ({:>+6}%)", signed_short(pct))
}

fn signed_short(n: f64) -> String {
    let n_abs = n.abs();

    if n_abs < 10.0 {
        format!("{:+.6}", n)
    } else if n_abs < 100.0 {
        format!("{:+.5}", n)
    } else if n_abs < 1000.0 {
        format!("{:+.4}", n)
    } else if n_abs < 10000.0 {
        format!("{:+.3}", n)
    } else if n_abs < 100000.0 {
        format!("{:+.2}", n)
    } else if n_abs < 1000000.0 {
        format!("{:+.1}", n)
    } else {
        format!("{:+.0}", n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_invocation_parent_mode_when_no_iai_flag() {
        let args = vec!["/path/to/test".to_owned(), "bench_a".to_owned()];
        let invocation = parse_invocation(args.into_iter());

        assert_eq!(invocation.executable, "/path/to/test");
        assert!(matches!(invocation.mode, InvocationMode::Parent));
    }

    #[test]
    fn parse_invocation_child_mode_no_index() {
        let args = vec!["/path/to/test".to_owned(), "--iai-run".to_owned()];
        let invocation = parse_invocation(args.into_iter());

        assert!(matches!(invocation.mode, InvocationMode::Child));
    }

    #[test]
    fn parse_invocation_child_indexed_mode() {
        let args = vec![
            "/path/to/test".to_owned(),
            "--iai-run".to_owned(),
            "2".to_owned(),
        ];
        let invocation = parse_invocation(args.into_iter());

        assert!(matches!(
            invocation.mode,
            InvocationMode::ChildIndexed { benchmark_index: 2 }
        ));
    }

    #[test]
    fn parse_invocation_child_indexed_non_numeric_defaults_to_negative() {
        let args = vec![
            "/path/to/test".to_owned(),
            "--iai-run".to_owned(),
            "not-a-number".to_owned(),
        ];
        let invocation = parse_invocation(args.into_iter());

        assert!(matches!(
            invocation.mode,
            InvocationMode::ChildIndexed {
                benchmark_index: -1
            }
        ));
    }

    #[test]
    fn percentage_diff_handles_no_change() {
        assert_eq!(percentage_diff(100, 100), " (No change)");
    }

    #[test]
    fn percentage_diff_handles_zero_baseline() {
        assert_eq!(percentage_diff(100, 0), " (N/A)");
    }

    #[test]
    fn percentage_diff_handles_positive_delta() {
        assert!(percentage_diff(110, 100).contains("10.000"));
    }

    #[test]
    fn percentage_diff_handles_negative_delta() {
        assert!(percentage_diff(90, 100).contains("-10.000"));
    }

    #[test]
    fn bench_stats_from_events_requires_all_metrics() {
        let events = vec![
            ("Ir".to_owned(), 1),
            ("I1mr".to_owned(), 2),
            ("ILmr".to_owned(), 3),
            ("Dr".to_owned(), 4),
            ("D1mr".to_owned(), 5),
            ("DLmr".to_owned(), 6),
            ("Dw".to_owned(), 7),
            ("D1mw".to_owned(), 8),
        ]
        .into_iter()
        .collect::<std::collections::HashMap<String, u64>>();

        assert!(BenchStats::from_events(&events).is_err());
    }
}

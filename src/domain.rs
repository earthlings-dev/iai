use std::{collections::HashMap, fmt, num::ParseIntError};

const REQUIRED_CACHEGRIND_EVENTS: &[&str] = &[
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

/// The two invocation modes recognized by the runner.
#[derive(Clone, Debug)]
pub(crate) enum InvocationMode {
    /// Spawned child invocation; runs only `benchmark_index` via `--iai-run`.
    Child { benchmark_index: isize },
    /// Primary harness invocation; drives calibration and all registered benches.
    Parent,
}

/// Validation failures that can occur while parsing CLI arguments.
#[derive(Debug)]
pub(crate) enum InvocationParseError {
    /// `--iai-run` was provided without a following index.
    MissingBenchmarkIndex,
    /// The provided index argument was present but not an integer.
    InvalidBenchmarkIndex(ParseIntError),
}

/// Parse process arguments into an invocation descriptor.
///
/// Expected shape:
/// - No special flags => parent mode
/// - `--iai-run <index>` => child mode with benchmark index dispatch
/// - Any additional tokens after a child index are ignored by construction.
pub(crate) fn parse_invocation<I>(mut args: I) -> Result<Invocation, InvocationParseError>
where
    I: Iterator<Item = String>,
{
    // Keep only explicit shape parsing here; all other users receive a fully typed
    // invocation and no longer need to inspect argument arrays.
    let executable = args.next().unwrap_or_default();

    match args.next() {
        Some(flag) if flag == "--iai-run" => {
            let index = args
                .next()
                .ok_or(InvocationParseError::MissingBenchmarkIndex)?;
            let benchmark_index = index
                .parse::<isize>()
                .map_err(InvocationParseError::InvalidBenchmarkIndex)?;

            Ok(Invocation {
                executable,
                mode: InvocationMode::Child { benchmark_index },
            })
        }
        _ => Ok(Invocation {
            executable,
            mode: InvocationMode::Parent,
        }),
    }
}

impl fmt::Display for InvocationParseError {
    /// Render parse errors as compact diagnostics consumed by harness error paths.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBenchmarkIndex => {
                write!(
                    f,
                    "Invalid --iai-run invocation: benchmark index is missing"
                )
            }
            Self::InvalidBenchmarkIndex(error) => {
                write!(
                    f,
                    "Invalid --iai-run invocation: benchmark index is invalid ({error})"
                )
            }
        }
    }
}

impl fmt::Display for Invocation {
    /// Display either `"<exe>"` or `"<exe> --iai-run <index>"`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.mode {
            InvocationMode::Parent => write!(f, "{}", self.executable),
            InvocationMode::Child { benchmark_index } => {
                write!(f, "{} --iai-run {}", self.executable, benchmark_index)
            }
        }
    }
}

/// Cachegrind counters required by the parser and reporting pipeline.
///
/// The fields map directly to the events requested from cachegrind and are kept
/// as raw counters until report-time normalization.
#[derive(Clone, Debug)]
pub(crate) struct CachegrindStats {
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

impl CachegrindStats {
    /// Build stats from parsed cachegrind event values.
    ///
    /// Returns an error if any required event is missing.
    pub(crate) fn from_events(events: &HashMap<String, u64>) -> Result<Self, String> {
        // Validate required counters up front so missing data fails early and
        // before arithmetic starts.
        let event_value = |key: &str| {
            events
                .get(key)
                .copied()
                .ok_or_else(|| format!("Missing required cachegrind event: {key}"))
        };

        for key in REQUIRED_CACHEGRIND_EVENTS {
            if !events.contains_key(*key) {
                return Err(format!("Missing required cachegrind event: {key}"));
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
    ///
    /// This is the sum of instruction/data LLC misses and write misses.
    pub(crate) fn ram_accesses(&self) -> u64 {
        self.instruction_cache_misses + self.data_cache_read_misses + self.data_cache_write_misses
    }

    /// Reduce raw counters into cache hierarchy groupings used by reports.
    pub(crate) fn summarize(&self) -> CachegrindSummary {
        let ram_hits = self.ram_accesses();
        let l3_accesses =
            self.instruction_l1_misses + self.data_l1_read_misses + self.data_l1_write_misses;
        let l3_hits = l3_accesses - ram_hits;

        let total_memory_rw = self.instruction_reads + self.data_reads + self.data_writes;
        let l1_hits = total_memory_rw - (ram_hits + l3_hits);

        CachegrindSummary {
            l1_hits,
            l3_hits,
            ram_hits,
        }
    }

    /// Subtract calibration counters with saturating arithmetic.
    ///
    /// Saturating subtraction intentionally avoids panic/underflow if a future
    /// counter format drifts below calibration values.
    pub(crate) fn subtract(&self, calibration: &CachegrindStats) -> CachegrindStats {
        CachegrindStats {
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
pub(crate) struct CachegrindSummary {
    pub(crate) l1_hits: u64,
    pub(crate) l3_hits: u64,
    pub(crate) ram_hits: u64,
}

impl CachegrindSummary {
    /// Estimate weighted memory-hierarchy cycles using the Cachegrind weights.
    ///
    /// The weighting intentionally follows a simple static model used by this
    /// project and is kept in one place to support later replacement.
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
    // Keep percentage strings compact and aligned while preserving sign.
    // Reduce decimal precision as magnitude grows to keep terminal output compact.
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
        let invocation =
            parse_invocation(args.into_iter()).expect("failed to parse parent invocation");

        assert_eq!(invocation.executable, "/path/to/test");
        assert!(matches!(invocation.mode, InvocationMode::Parent));
    }

    #[test]
    fn parse_invocation_child_mode_extracts_index() {
        let args = vec![
            "/path/to/test".to_owned(),
            "--iai-run".to_owned(),
            "2".to_owned(),
        ];
        let invocation =
            parse_invocation(args.into_iter()).expect("failed to parse child invocation");

        assert!(matches!(
            invocation.mode,
            InvocationMode::Child { benchmark_index: 2 }
        ));
    }

    #[test]
    fn parse_invocation_rejects_missing_child_index() {
        let args = vec!["/path/to/test".to_owned(), "--iai-run".to_owned()];

        assert!(matches!(
            parse_invocation(args.into_iter()),
            Err(InvocationParseError::MissingBenchmarkIndex)
        ));
    }

    #[test]
    fn parse_invocation_rejects_non_numeric_child_index() {
        let args = vec![
            "/path/to/test".to_owned(),
            "--iai-run".to_owned(),
            "not-a-number".to_owned(),
        ];

        assert!(matches!(
            parse_invocation(args.into_iter()),
            Err(InvocationParseError::InvalidBenchmarkIndex(_))
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
    fn cachegrind_from_events_requires_all_metrics() {
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

        assert!(CachegrindStats::from_events(&events).is_err());
    }
}

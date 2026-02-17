use std::{
    collections::HashMap,
    fmt,
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};

use crate::infrastructure::StandardSystem;
use crate::{
    domain::{
        CallgrindStats, CallgrindSummary, Invocation, InvocationMode, parse_invocation,
        percentage_diff,
    },
    ports::System,
};

/// Per-benchmark stats keyed by name, for both current and baseline runs.
type BenchStatsMap = HashMap<String, CallgrindStats>;

/// Execute the benchmark harness.
///
/// This is an internal application entrypoint used by the custom test harness.
/// It wires the domain/application boundary through a production `System` adapter.
pub(crate) fn runner(benches: &[&(&'static str, fn())]) {
    let system = StandardSystem::new();
    run_with_system(benches, &system, std::env::args())
}

/// Execute the benchmark flow with an explicit argument iterator and `System` adapter.
///
/// This function is intentionally generic over the argument source so command-line
/// parsing can be tested with deterministic inputs.
/// It performs:
/// - typed argument parsing
/// - invocation mode dispatch
/// - child-process execution path
/// - benchmark loop with baseline comparisons
fn run_with_system<I>(benches: &[&(&'static str, fn())], system: &impl System, args: I)
where
    I: IntoIterator<Item = String>,
{
    let Invocation { executable, mode } = parse_invocation(args.into_iter());

    match mode {
        InvocationMode::Child => {
            // Child mode: execute all benchmarks sequentially under Callgrind collection.
            run_child_benchmarks(benches);
            return;
        }
        InvocationMode::Parent => {}
    }

    // Parent mode: validate Callgrind, run all benchmarks in a single invocation,
    // and display reports with baseline comparisons when available.
    if !check_valgrind(system) {
        return;
    }

    let arch = get_arch();
    let allow_aslr = system.var_os("IAI_ALLOW_ASLR").is_some();

    let (stats_map, old_stats_map) =
        match run_benches(system, &arch, &executable, benches, allow_aslr) {
            Ok(result) => result,
            Err(error) => {
                eprintln!("Failed to run benchmarks: {}", error);
                return;
            }
        };

    for (name, _func) in benches.iter() {
        let stats = match stats_map.get(*name) {
            Some(s) => s,
            None => {
                eprintln!("No results found for benchmark '{}'", name);
                continue;
            }
        };
        let old_stats = old_stats_map.get(*name);

        let old_summary = old_stats.map(|s| s.summarize());
        let summary = stats.summarize();
        for line in format_benchmark_report(name, stats, old_stats, &summary, old_summary.as_ref())
        {
            println!("{}", line);
        }
    }
}

/// Build user-facing benchmark output from a benchmark and summary data.
///
/// The renderer is isolated in a helper to separate rendering concerns from
/// process execution and to make output formatting test-friendly.
fn format_benchmark_report(
    name: &str,
    stats: &CallgrindStats,
    old_stats: Option<&CallgrindStats>,
    summary: &CallgrindSummary,
    old_summary: Option<&CallgrindSummary>,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(6);
    lines.push(name.to_owned());
    lines.push(format!(
        "  Instructions:     {:>15}{}",
        stats.instruction_reads,
        match old_stats {
            Some(old) => percentage_diff(stats.instruction_reads, old.instruction_reads),
            None => "".to_owned(),
        }
    ));
    lines.push(format!(
        "  L1 Accesses:      {:>15}{}",
        summary.l1_hits,
        match old_summary {
            Some(old) => percentage_diff(summary.l1_hits, old.l1_hits),
            None => "".to_owned(),
        }
    ));
    lines.push(format!(
        "  L2 Accesses:      {:>15}{}",
        summary.l3_hits,
        match old_summary {
            Some(old) => percentage_diff(summary.l3_hits, old.l3_hits),
            None => "".to_owned(),
        }
    ));
    lines.push(format!(
        "  RAM Accesses:     {:>15}{}",
        summary.ram_hits,
        match old_summary {
            Some(old) => percentage_diff(summary.ram_hits, old.ram_hits),
            None => "".to_owned(),
        }
    ));
    lines.push(format!(
        "  Estimated Cycles: {:>15}{}",
        summary.cycles(),
        match old_summary {
            Some(old) => percentage_diff(summary.cycles(), old.cycles()),
            None => "".to_owned(),
        }
    ));
    lines.push(String::new());
    lines
}

/// Execute all benchmarks when invoked in child mode.
///
/// This path is used for `--iai-run` re-entry via Callgrind. All benchmarks
/// run sequentially; Callgrind's `--toggle-collect` isolates per-function counters.
fn run_child_benchmarks(benches: &[&(&'static str, fn())]) {
    for (_name, func) in benches.iter() {
        func();
    }
}

/// Probe `valgrind` readiness before running parent-mode benchmarks.
///
/// Returns `true` only when `valgrind --tool=callgrind --version` exits
/// successfully; otherwise prints a diagnostic and returns `false`.
fn check_valgrind(system: &impl System) -> bool {
    let mut command = Command::new("valgrind");
    command
        .arg("--tool=callgrind")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match system.output(&mut command) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "Failed to launch valgrind: {}. Please ensure that valgrind is installed and on the $PATH.",
                output.status
            );
            false
        }
        Err(error) => {
            eprintln!("Unexpected error while launching valgrind: {}", error);
            false
        }
    }
}

/// Resolve target architecture identifier without invoking external commands.
fn get_arch() -> String {
    std::env::consts::ARCH.to_owned()
}

/// Construct the base command for a normal `valgrind` invocation.
fn basic_valgrind() -> Command {
    Command::new("valgrind")
}

#[cfg(target_os = "linux")]
/// Linux disables ASLR via `setarch` when requested.
fn valgrind_without_aslr(arch: &str) -> Command {
    let mut command = Command::new("setarch");
    command.arg(arch).arg("-R").arg("valgrind");
    command
}

#[cfg(target_os = "freebsd")]
/// FreeBSD disables ASLR via `proccontrol` when requested.
fn valgrind_without_aslr(_arch: &str) -> Command {
    let mut command = Command::new("proccontrol");
    command.arg("-m").arg("aslr").arg("-s").arg("disable");
    command
}

#[cfg(all(not(target_os = "linux"), not(target_os = "freebsd")))]
/// Fallback command when no platform-specific ASLR wrapper exists.
fn valgrind_without_aslr(_arch: &str) -> Command {
    basic_valgrind()
}

/// Execute all benchmarks in a single Callgrind invocation and parse results.
///
/// Returns a tuple of (current stats, old baseline stats) as HashMaps keyed
/// by benchmark name. The old stats map is empty if no prior baseline exists.
fn run_benches(
    system: &impl System,
    arch: &str,
    executable: &str,
    benches: &[&(&'static str, fn())],
    allow_aslr: bool,
) -> Result<(BenchStatsMap, BenchStatsMap), RunnerError> {
    let output_file = PathBuf::from("target/iai/callgrind.out");
    let old_output_file = output_file.with_file_name("callgrind.out.old");

    let output_dir = output_file
        .parent()
        .ok_or_else(|| RunnerError::InvalidOutputPath(output_file.clone()))?;

    system.create_dir_all(output_dir).map_err(RunnerError::Io)?;

    if system.file_exists(&output_file) {
        system
            .copy_file(&output_file, &old_output_file)
            .map_err(RunnerError::Io)?;
    }

    let mut command = if allow_aslr {
        basic_valgrind()
    } else {
        valgrind_without_aslr(arch)
    };

    command
        .arg("--tool=callgrind")
        .arg("--cache-sim=yes")
        .arg("--I1=32768,8,64")
        .arg("--D1=32768,8,64")
        .arg("--LL=8388608,16,64")
        .arg(format!("--callgrind-out-file={}", output_file.display()))
        .arg("--compress-strings=no")
        .arg("--compress-pos=no")
        .arg("--collect-atstart=no");

    for (name, _func) in benches.iter() {
        command.arg(format!("--toggle-collect=__iai_bench_{name}"));
    }

    command
        .arg(executable)
        .arg("--iai-run")
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let status = system.status(&mut command).map_err(RunnerError::Io)?;
    if !status.success() {
        return Err(RunnerError::CommandFailed(status));
    }

    let new_stats = parse_callgrind_output(system, &output_file)?;
    let old_stats = if system.file_exists(&old_output_file) {
        parse_callgrind_output(system, &old_output_file)?
    } else {
        HashMap::new()
    };

    Ok((new_stats, old_stats))
}

/// Parse Callgrind output from `path` into per-benchmark typed counters.
///
/// The parser scans for:
/// - An `events:` line defining the event names
/// - `cfn=__iai_bench_<name>` lines identifying benchmark function sections
/// - For each such section, reads the `calls=` line and the inclusive-cost data line
///
/// Callgrind format notes:
/// - Trailing zero values on data lines are omitted; missing positions default to 0.
/// - The data line format is `<position> <values...>` where position is skipped.
///
/// Returns a HashMap mapping benchmark names to their CallgrindStats.
fn parse_callgrind_output(
    system: &impl System,
    path: &Path,
) -> Result<HashMap<String, CallgrindStats>, RunnerError> {
    let file = system.open_file(path).map_err(RunnerError::Io)?;
    let mut events_tokens: Option<Vec<String>> = None;
    let mut results = HashMap::new();

    let mut lines = BufReader::new(file).lines();

    while let Some(line) = lines.next() {
        let line = line.map_err(RunnerError::Io)?;

        if let Some(values) = line.strip_prefix("events: ") {
            events_tokens = Some(
                values
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            );
            continue;
        }

        if let Some(name) = line.strip_prefix("cfn=__iai_bench_") {
            let events = events_tokens.as_ref().ok_or_else(|| {
                RunnerError::ParseError(format!(
                    "Unable to parse callgrind output file {}: events line must appear before function data",
                    path.display(),
                ))
            })?;

            // Skip the calls line
            let _calls = lines.next().ok_or_else(|| {
                RunnerError::ParseError(format!(
                    "Unable to parse callgrind output file {}: unexpected end of file after cfn=__iai_bench_{}",
                    path.display(),
                    name,
                ))
            })?.map_err(RunnerError::Io)?;

            // Read the data line: "<position> <values...>"
            let data_line = lines.next().ok_or_else(|| {
                RunnerError::ParseError(format!(
                    "Unable to parse callgrind output file {}: unexpected end of file after calls line for {}",
                    path.display(),
                    name,
                ))
            })?.map_err(RunnerError::Io)?;

            let event_map = parse_data_line(events, &data_line, path)?;

            let stats = CallgrindStats::from_events(&event_map).map_err(RunnerError::ParseError)?;
            results.insert(name.to_owned(), stats);
        }
    }

    Ok(results)
}

/// Parse a single Callgrind data line into an event-value map.
///
/// The line format is `<position> <value>...` where the first token is a source
/// position (skipped) and subsequent tokens map positionally to the events list.
/// Callgrind omits trailing zero values, so any events beyond the provided values
/// default to 0.
fn parse_data_line(
    events: &[String],
    data_line: &str,
    path: &Path,
) -> Result<HashMap<String, u64>, RunnerError> {
    let mut tokens = data_line.split_whitespace();
    let _position = tokens.next(); // skip source position

    let mut event_map: HashMap<String, u64> = HashMap::with_capacity(events.len());
    for event_name in events {
        let value = match tokens.next() {
            Some(value_str) => value_str.parse::<u64>().map_err(|error| {
                RunnerError::ParseError(format!(
                    "Unable to parse callgrind output file {}: value '{}' for event '{}' is invalid ({})",
                    path.display(),
                    value_str,
                    event_name,
                    error,
                ))
            })?,
            // Callgrind omits trailing zeros on data lines.
            None => 0,
        };
        event_map.insert(event_name.clone(), value);
    }

    Ok(event_map)
}

/// Internal runner execution errors.
///
/// These cover I/O boundaries, command failures, and callgrind parse failures.
#[derive(Debug)]
enum RunnerError {
    /// Error while creating directories, copying files, or reading outputs.
    Io(io::Error),
    /// Process returned a non-zero exit code.
    CommandFailed(ExitStatus),
    /// Output path construction failed before command execution.
    InvalidOutputPath(PathBuf),
    /// Parser validation failure with a descriptive message.
    ParseError(String),
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {}", error),
            Self::CommandFailed(status) => write!(f, "Command failed: {}", status),
            Self::InvalidOutputPath(path) => write!(f, "Invalid output path: {}", path.display()),
            Self::ParseError(message) => write!(f, "{}", message),
        }
    }
}

impl From<io::Error> for RunnerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{EnvironmentPort, FilePort, ProcessPort};
    use std::cell::Cell;
    use std::fs;
    use std::process::Output;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Process-local call counters used by helper benchmarks to assert dispatch.
    static BENCH_ONE_CALLS: AtomicUsize = AtomicUsize::new(0);
    /// Process-local call counters used by helper benchmarks to assert dispatch.
    static BENCH_TWO_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// Benchmark stub for dispatch tests.
    fn bench_one() {
        BENCH_ONE_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    /// Benchmark stub for dispatch tests.
    fn bench_two() {
        BENCH_TWO_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    /// Reset both benchmark call counters to a known baseline.
    fn reset_bench_counts() {
        BENCH_ONE_CALLS.store(0, Ordering::SeqCst);
        BENCH_TWO_CALLS.store(0, Ordering::SeqCst);
    }

    /// Return how many times `bench_one` was executed.
    fn bench_one_calls() -> usize {
        BENCH_ONE_CALLS.load(Ordering::SeqCst)
    }

    /// Return how many times `bench_two` was executed.
    fn bench_two_calls() -> usize {
        BENCH_TWO_CALLS.load(Ordering::SeqCst)
    }

    /// Create a temporary Callgrind-style output fixture and return its path.
    fn with_tmp_file(contents: &str) -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "iai-callgrind-test-{}-{}.out",
            std::process::id(),
            id
        ));
        fs::write(&path, contents).expect("failed to write temporary callgrind output");
        path
    }

    /// Parse callgrind output from a temp path and remove the fixture afterward.
    fn read_and_cleanup(path: PathBuf) -> Result<BenchStatsMap, RunnerError> {
        let result = {
            let system = StandardSystem::new();
            parse_callgrind_output(&system, &path)
        };
        let _ = fs::remove_file(path);
        result
    }

    #[test]
    fn parse_callgrind_output_parses_valid_output() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 10 1 2 3 4 5 6 7 8\n",
        );

        let parsed = read_and_cleanup(path).expect("expected callgrind output to parse");
        let stats = parsed
            .get("my_bench")
            .expect("expected my_bench in results");

        assert_eq!(stats.instruction_reads, 10);
        assert_eq!(stats.instruction_l1_misses, 1);
        assert_eq!(stats.instruction_cache_misses, 2);
        assert_eq!(stats.data_reads, 3);
        assert_eq!(stats.data_l1_read_misses, 4);
        assert_eq!(stats.data_cache_read_misses, 5);
        assert_eq!(stats.data_writes, 6);
        assert_eq!(stats.data_l1_write_misses, 7);
        assert_eq!(stats.data_cache_write_misses, 8);
    }

    #[test]
    fn parse_callgrind_output_parses_multiple_benchmarks() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_bench_a\n\
             calls=1 0\n\
             0 10 1 2 3 4 5 6 7 8\n\
             cfn=__iai_bench_bench_b\n\
             calls=1 0\n\
             0 20 2 4 6 8 10 12 14 16\n",
        );

        let parsed = read_and_cleanup(path).expect("expected callgrind output to parse");
        assert_eq!(parsed.len(), 2);

        let a = parsed.get("bench_a").expect("expected bench_a");
        assert_eq!(a.instruction_reads, 10);

        let b = parsed.get("bench_b").expect("expected bench_b");
        assert_eq!(b.instruction_reads, 20);
    }

    #[test]
    fn parse_callgrind_output_missing_events_line() {
        let path = with_tmp_file(
            "cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 10 1 2 3 4 5 6 7 8\n",
        );

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("events line must appear before function data"));
    }

    #[test]
    fn parse_callgrind_output_rejects_non_numeric_data() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 10 1 a 3 4 5 6 7 8\n",
        );

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("value 'a'"));
    }

    #[test]
    fn parse_callgrind_output_handles_trailing_zeros_omitted() {
        // Callgrind omits trailing zero values on data lines.
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 42 3 1\n",
        );

        let parsed = read_and_cleanup(path).expect("expected callgrind output to parse");
        let stats = parsed
            .get("my_bench")
            .expect("expected my_bench in results");

        assert_eq!(stats.instruction_reads, 42);
        assert_eq!(stats.instruction_l1_misses, 3);
        assert_eq!(stats.instruction_cache_misses, 1);
        // Remaining events default to 0
        assert_eq!(stats.data_reads, 0);
        assert_eq!(stats.data_l1_read_misses, 0);
        assert_eq!(stats.data_cache_read_misses, 0);
        assert_eq!(stats.data_writes, 0);
        assert_eq!(stats.data_l1_write_misses, 0);
        assert_eq!(stats.data_cache_write_misses, 0);
    }

    #[test]
    fn parse_callgrind_output_returns_empty_for_no_benchmarks() {
        let path = with_tmp_file("events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n");

        let parsed = read_and_cleanup(path).expect("expected callgrind output to parse");
        assert!(parsed.is_empty());
    }

    #[test]
    fn runner_child_mode_dispatches_all_benchmarks() {
        reset_bench_counts();
        let benches: &[&(&str, fn())] = &[&("bench_one", bench_one), &("bench_two", bench_two)];
        let system = FakeSystem::default();

        run_with_system(
            benches,
            &system,
            vec!["iai-binary".to_owned(), "--iai-run".to_owned()],
        );

        assert_eq!(bench_one_calls(), 1);
        assert_eq!(bench_two_calls(), 1);
        assert_eq!(system.output_calls.get(), 0);
    }

    #[test]
    fn runner_parent_mode_does_not_dispatch_benchmarks_if_valgrind_missing() {
        reset_bench_counts();
        let benches: &[&(&str, fn())] = &[&("bench_one", bench_one), &("bench_two", bench_two)];
        let system = FakeSystem::default();

        run_with_system(benches, &system, vec!["iai-binary".to_owned()]);

        assert_eq!(bench_one_calls(), 0);
        assert_eq!(bench_two_calls(), 0);
        assert_eq!(system.output_calls.get(), 1);
    }

    /// Fake system used to validate runner dispatch and probe behavior without
    /// invoking external processes.
    struct FakeSystem {
        output_calls: Cell<usize>,
    }

    impl Default for FakeSystem {
        fn default() -> Self {
            Self {
                output_calls: Cell::new(0),
            }
        }
    }

    impl FilePort for FakeSystem {
        /// File creation is not needed for the adapter-focused negative-path tests.
        fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }

        /// File copy is intentionally a no-op in this fake.
        fn copy_file(&self, _from: &Path, _to: &Path) -> io::Result<u64> {
            Ok(0)
        }

        /// The fake reports no callgrind outputs to force parent-mode early failure paths.
        fn file_exists(&self, _path: &Path) -> bool {
            false
        }

        /// Reading files from the fake is unsupported and returns a deterministic "not found" error.
        fn open_file(&self, _path: &Path) -> io::Result<fs::File> {
            Err(io::Error::new(io::ErrorKind::NotFound, "not found"))
        }
    }

    impl ProcessPort for FakeSystem {
        /// Status execution is unused by the selected fake-system tests; keep it unused and explicit.
        fn status(&self, _command: &mut Command) -> io::Result<ExitStatus> {
            Err(io::Error::other("not used in this test"))
        }

        /// Count every output probe to assert the runner's parent-mode readiness check behavior.
        fn output(&self, _command: &mut Command) -> io::Result<Output> {
            self.output_calls.set(self.output_calls.get() + 1);
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "valgrind not available",
            ))
        }
    }

    impl EnvironmentPort for FakeSystem {
        /// Return no environment overrides unless explicitly required by the test setup.
        fn var_os(&self, _key: &str) -> Option<std::ffi::OsString> {
            None
        }
    }
}

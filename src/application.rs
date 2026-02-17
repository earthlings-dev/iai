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
        CachegrindStats, CachegrindSummary, Invocation, InvocationMode, parse_invocation,
        percentage_diff,
    },
    ports::System,
};

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
/// - benchmark loop with calibration and baseline comparisons
fn run_with_system<I>(benches: &[&(&'static str, fn())], system: &impl System, args: I)
where
    I: IntoIterator<Item = String>,
{
    // Parse the process invocation first so parent and child modes share a single
    // validated entry point.
    let Invocation { executable, mode } = match parse_invocation(args.into_iter()) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("{error}");
            return;
        }
    };

    match mode {
        InvocationMode::Child { benchmark_index } => {
            // Child mode is intentionally single-shot: the selected benchmark is
            // executed and this process then exits the runner loop.
            run_child_benchmark(benches, benchmark_index);
            return;
        }
        InvocationMode::Parent => {}
    }

    // Parent mode performs: valgrind validation, one calibration run, and then
    // the full benchmark table (with baseline subtraction applied when present).
    if !check_valgrind(system) {
        return;
    }

    let arch = get_arch();
    let allow_aslr = system.var_os("IAI_ALLOW_ASLR").is_some();

    let (calibration, old_calibration) = match run_bench(
        system,
        &arch,
        &executable,
        -1,
        "iai_calibration",
        allow_aslr,
    ) {
        Ok((new_stats, old_stats)) => (new_stats, old_stats),
        Err(error) => {
            eprintln!("Failed to run calibration benchmark: {}", error);
            return;
        }
    };

    for (index, (name, _func)) in benches.iter().enumerate() {
        let (stats, old_stats) =
            match run_bench(system, &arch, &executable, index as isize, name, allow_aslr) {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("Failed to run benchmark '{}': {}", name, error);
                    continue;
                }
            };

        let stats = stats.subtract(&calibration);
        let old_stats = match (&old_stats, &old_calibration) {
            (Some(old_stats), Some(old_calibration)) => Some(old_stats.subtract(old_calibration)),
            _ => None,
        };

        let old_summary = old_stats.as_ref().map(|stats| stats.summarize());
        let summary = stats.summarize();
        for line in format_benchmark_report(
            name,
            &stats,
            old_stats.as_ref(),
            &summary,
            old_summary.as_ref(),
        ) {
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
    stats: &CachegrindStats,
    old_stats: Option<&CachegrindStats>,
    summary: &CachegrindSummary,
    old_summary: Option<&CachegrindSummary>,
) -> Vec<String> {
    // Build lines only; keep I/O out of the runner core for easier testing.
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

/// Execute a single benchmark when invoked in child mode.
///
/// Returns `true` if a benchmark was dispatched, `false` otherwise.
/// Returns `true` when the selected benchmark was dispatched.
/// This path is only used for `--iai-run N` re-entry via `valgrind`.
fn run_child_benchmark(benches: &[&(&'static str, fn())], benchmark_index: isize) -> bool {
    if benchmark_index < 0 {
        // Keep behavior defensive and deterministic for malformed invocations.
        return false;
    }

    match usize::try_from(benchmark_index) {
        Err(_) => {
            eprintln!("Invalid benchmark index: {}", benchmark_index);
            false
        }
        Ok(index) => {
            if index >= benches.len() {
                eprintln!(
                    "Invalid benchmark index: {}. This index is out of range.",
                    index
                );
                return false;
            }

            (benches[index].1)();
            true
        }
    }
}

/// Probe `valgrind` readiness before running parent-mode benchmarks.
///
/// Returns `true` only when `valgrind --tool=cachegrind --version` exits
/// successfully; otherwise prints a diagnostic and returns `false`.
fn check_valgrind(system: &impl System) -> bool {
    // Call `valgrind --tool=cachegrind --version` with suppressed output to avoid
    // polluting benchmark output before actual runs.
    let mut command = Command::new("valgrind");
    command
        .arg("--tool=cachegrind")
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
    // Prefer the compiler-reported target architecture instead of shelling out.
    std::env::consts::ARCH.to_owned()
}

/// Construct the base command for a normal `valgrind` invocation.
/// This intentionally omits ASLR wrappers and keeps command-shape concerns local.
fn basic_valgrind() -> Command {
    Command::new("valgrind")
}

#[cfg(target_os = "linux")]
/// Linux disables ASLR via `setarch` when requested.
fn valgrind_without_aslr(arch: &str) -> Command {
    // Keep path selection centralized so testability and OS-specific behavior are explicit.
    let mut command = Command::new("setarch");
    command.arg(arch).arg("-R").arg("valgrind");
    command
}

#[cfg(target_os = "freebsd")]
/// FreeBSD disables ASLR via `proccontrol` when requested.
fn valgrind_without_aslr(_arch: &str) -> Command {
    // Keep the disablement helper explicit to avoid silently skipping the ASLR
    // behavior on supported BSD hosts.
    let mut command = Command::new("proccontrol");
    command.arg("-m").arg("aslr").arg("-s").arg("disable");
    command
}

#[cfg(all(not(target_os = "linux"), not(target_os = "freebsd")))]
/// Fallback command when no platform-specific ASLR wrapper exists.
fn valgrind_without_aslr(_arch: &str) -> Command {
    // Fallback path for platforms without a dedicated ASLR wrapper command.
    basic_valgrind()
}

/// Execute one benchmark invocation and parse current and optional prior cachegrind output.
fn run_bench(
    system: &impl System,
    arch: &str,
    executable: &str,
    index: isize,
    benchmark_name: &str,
    allow_aslr: bool,
) -> Result<(CachegrindStats, Option<CachegrindStats>), RunnerError> {
    // Output path is fixed under target/iai so multiple invocations can rotate and
    // compare against a prior snapshot.
    let output_file = PathBuf::from(format!("target/iai/cachegrind.out.{benchmark_name}"));
    let old_output_file =
        output_file.with_file_name(format!("cachegrind.out.{benchmark_name}.old"));

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
        .arg("--tool=cachegrind")
        .arg("--cache-sim=yes")
        .arg("--I1=32768,8,64")
        .arg("--D1=32768,8,64")
        .arg("--LL=8388608,16,64")
        .arg(format!("--cachegrind-out-file={}", output_file.display()))
        .arg(executable)
        .arg("--iai-run")
        .arg(index.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let status = system.status(&mut command).map_err(RunnerError::Io)?;
    if !status.success() {
        return Err(RunnerError::CommandFailed(status));
    }

    let new_stats = parse_cachegrind_output(system, &output_file)?;
    let old_stats = if system.file_exists(&old_output_file) {
        Some(parse_cachegrind_output(system, &old_output_file)?)
    } else {
        None
    };

    Ok((new_stats, old_stats))
}

/// Parse Cachegrind output from `path` into typed benchmark counters.
///
/// The parser requires matching `events:` and `summary:` records and all mandatory
/// event counters required by `CachegrindStats::from_events`.
/// It:
/// - captures event names and tokenized summary values from the relevant lines
/// - enforces same token count for both lines
/// - fails fast with contextual `RunnerError::ParseError` on malformed data
fn parse_cachegrind_output(
    system: &impl System,
    path: &Path,
) -> Result<CachegrindStats, RunnerError> {
    // Parse side-effect-free and line-oriented so callers can depend on stable,
    // deterministic errors for file shape drift.
    let file = system.open_file(path).map_err(RunnerError::Io)?;
    let mut events_tokens = None;
    let mut summary_tokens = None;

    for line in BufReader::new(file).lines() {
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

        if let Some(values) = line.strip_prefix("summary: ") {
            summary_tokens = Some(
                values
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            );
        }
    }

    let events_tokens = events_tokens.ok_or_else(|| {
        RunnerError::ParseError(format!(
            "Unable to parse cachegrind output file {}: missing events line",
            path.display(),
        ))
    })?;

    let summary_tokens = summary_tokens.ok_or_else(|| {
        RunnerError::ParseError(format!(
            "Unable to parse cachegrind output file {}: missing summary line",
            path.display(),
        ))
    })?;

    let mut events: HashMap<String, u64> = HashMap::with_capacity(events_tokens.len());

    if events_tokens.len() != summary_tokens.len() {
        return Err(RunnerError::ParseError(format!(
            "Unable to parse cachegrind output file {}: events and summary lengths do not match",
            path.display(),
        )));
    }

    for (event, value) in events_tokens.into_iter().zip(summary_tokens.into_iter()) {
        let value = value.parse::<u64>().map_err(|error| {
            RunnerError::ParseError(format!(
                "Unable to parse cachegrind output file {}: value '{}' for event '{}' is invalid ({})",
                path.display(),
                value,
                event,
                error
            ))
        })?;

        events.insert(event.to_owned(), value);
    }

    CachegrindStats::from_events(&events).map_err(RunnerError::ParseError)
}

/// Internal runner execution errors.
///
/// These cover I/O boundaries, command failures, and cachegrind parse failures.
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

    /// Benchmark stub for index-selection tests.
    fn bench_one() {
        BENCH_ONE_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    /// Benchmark stub for index-selection tests.
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

    /// Create a temporary Cachegrind-style output fixture and return its path.
    ///
    /// The file is given a unique name per test process and monotonic id to
    /// avoid collisions across test invocations.
    fn with_tmp_file(contents: &str) -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "iai-cachegrind-test-{}-{}.out",
            std::process::id(),
            id
        ));
        fs::write(&path, contents).expect("failed to write temporary cachegrind output");
        path
    }

    /// Parse cachegrind output from a temp path and remove the fixture afterward.
    ///
    /// Cleanup is performed regardless of parse outcome to keep tests independent
    /// and avoid leaking per-run artifacts into shared temp directories.
    fn read_and_cleanup(path: PathBuf) -> Result<CachegrindStats, RunnerError> {
        let result = {
            let system = StandardSystem::new();
            parse_cachegrind_output(&system, &path)
        };
        let _ = fs::remove_file(path);
        result
    }

    #[test]
    fn parse_cachegrind_output_parses_valid_output() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\nsummary: 10 1 2 3 4 5 6 7 8\n",
        );

        let parsed = read_and_cleanup(path).expect("expected cachegrind output to parse");

        assert_eq!(parsed.instruction_reads, 10);
        assert_eq!(parsed.instruction_l1_misses, 1);
        assert_eq!(parsed.instruction_cache_misses, 2);
        assert_eq!(parsed.data_reads, 3);
        assert_eq!(parsed.data_l1_read_misses, 4);
        assert_eq!(parsed.data_cache_read_misses, 5);
        assert_eq!(parsed.data_writes, 6);
        assert_eq!(parsed.data_l1_write_misses, 7);
        assert_eq!(parsed.data_cache_write_misses, 8);
    }

    #[test]
    fn parse_cachegrind_output_missing_events_line() {
        let path = with_tmp_file("summary: 10 1 2 3 4 5 6 7 8\n");

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("missing events line"));
    }

    #[test]
    fn parse_cachegrind_output_missing_summary_line() {
        let path = with_tmp_file("events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n");

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("missing summary line"));
    }

    #[test]
    fn parse_cachegrind_output_requires_matching_token_lengths() {
        let path = with_tmp_file("events: Ir I1mr ILmr\nsummary: 10 1 2 3\n");

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("events and summary lengths do not match"));
    }

    #[test]
    fn parse_cachegrind_output_rejects_non_numeric_data() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\nsummary: 10 1 a 3 4 5 6 7 8\n",
        );

        let parsed = read_and_cleanup(path).expect_err("expected parse failure");
        let message = match parsed {
            RunnerError::ParseError(message) => message,
            _ => panic!("expected parse error"),
        };

        assert!(message.contains("value 'a'"));
    }

    #[test]
    fn runner_child_mode_dispatches_only_selected_benchmark() {
        reset_bench_counts();
        let benches: &[&(&str, fn())] = &[&("bench_one", bench_one), &("bench_two", bench_two)];
        let system = FakeSystem::default();

        run_with_system(
            benches,
            &system,
            vec![
                "iai-binary".to_owned(),
                "--iai-run".to_owned(),
                "1".to_owned(),
            ],
        );

        assert_eq!(bench_one_calls(), 0);
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

        /// The fake reports no cachegrind outputs to force parent-mode early failure paths.
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

        /// Count every output probe to assert the runner’s parent-mode readiness check behavior.
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

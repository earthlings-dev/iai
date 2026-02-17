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
        BenchStats, BenchSummary, Invocation, InvocationMode, parse_invocation, percentage_diff,
    },
    ports::System,
};

/// Per-benchmark stats keyed by name.
#[cfg(feature = "callgrind")]
type BenchStatsMap = HashMap<String, BenchStats>;

// ──────────────────────────────────────────────────────────────
// Public entry point
// ──────────────────────────────────────────────────────────────

/// Execute the benchmark harness.
pub(crate) fn runner(benches: &[&(&'static str, fn())]) {
    let system = StandardSystem::new();
    run_with_system(benches, &system, std::env::args())
}

// ──────────────────────────────────────────────────────────────
// Runner orchestration
// ──────────────────────────────────────────────────────────────

fn run_with_system<I>(benches: &[&(&'static str, fn())], system: &impl System, args: I)
where
    I: IntoIterator<Item = String>,
{
    let Invocation { executable, mode } = parse_invocation(args.into_iter());

    match mode {
        InvocationMode::ChildIndexed { benchmark_index } => {
            #[cfg(feature = "cachegrind")]
            run_child_benchmark(benches, benchmark_index);
            #[cfg(not(feature = "cachegrind"))]
            {
                let _ = benchmark_index;
                eprintln!("Indexed child mode requires the 'cachegrind' feature");
            }
            return;
        }
        InvocationMode::Child => {
            #[cfg(feature = "callgrind")]
            run_child_benchmarks(benches);
            #[cfg(not(feature = "callgrind"))]
            eprintln!("Child mode requires the 'callgrind' feature");
            return;
        }
        InvocationMode::Parent => {}
    }

    let arch = get_arch();
    let allow_aslr = system.var_os("IAI_ALLOW_ASLR").is_some();
    let both_active = cfg!(feature = "cachegrind") && cfg!(feature = "callgrind");
    let group_by_benchmark = both_active && system.var_os("IAI_GROUP_BY_BENCHMARK").is_some();

    // ── Cachegrind ──────────────────────────────────────────
    #[cfg(feature = "cachegrind")]
    let cachegrind_results: Option<Vec<(String, BenchStats, Option<BenchStats>)>> = {
        if !check_valgrind(system, "cachegrind") {
            return;
        }
        match run_cachegrind(system, &arch, &executable, benches, allow_aslr) {
            Ok(results) => Some(results),
            Err(error) => {
                eprintln!("Cachegrind failed: {}", error);
                None
            }
        }
    };

    // ── Callgrind ───────────────────────────────────────────
    #[cfg(feature = "callgrind")]
    let callgrind_results: Option<(BenchStatsMap, BenchStatsMap)> = {
        if !check_valgrind(system, "callgrind") {
            return;
        }
        match run_benches(system, &arch, &executable, benches, allow_aslr) {
            Ok(results) => Some(results),
            Err(error) => {
                eprintln!("Callgrind failed: {}", error);
                None
            }
        }
    };

    // ── Display ─────────────────────────────────────────────
    if group_by_benchmark {
        display_grouped_by_benchmark(
            benches,
            #[cfg(feature = "cachegrind")]
            &cachegrind_results,
            #[cfg(feature = "callgrind")]
            &callgrind_results,
        );
    } else if both_active {
        display_grouped_by_tool(
            benches,
            #[cfg(feature = "cachegrind")]
            &cachegrind_results,
            #[cfg(feature = "callgrind")]
            &callgrind_results,
        );
    } else {
        display_single_tool(
            benches,
            #[cfg(feature = "cachegrind")]
            &cachegrind_results,
            #[cfg(feature = "callgrind")]
            &callgrind_results,
        );
    }
}

// ──────────────────────────────────────────────────────────────
// Display helpers
// ──────────────────────────────────────────────────────────────

fn display_grouped_by_tool(
    benches: &[&(&'static str, fn())],
    #[cfg(feature = "cachegrind")] cachegrind: &Option<
        Vec<(String, BenchStats, Option<BenchStats>)>,
    >,
    #[cfg(feature = "callgrind")] callgrind: &Option<(BenchStatsMap, BenchStatsMap)>,
) {
    let _ = benches;

    #[cfg(feature = "cachegrind")]
    if let Some(results) = cachegrind {
        println!("\n=== cachegrind ===");
        for (name, stats, old_stats) in results {
            print_bench_report(name, stats, old_stats.as_ref());
        }
    }

    #[cfg(feature = "callgrind")]
    if let Some((stats_map, old_stats_map)) = callgrind {
        println!("\n=== callgrind ===");
        for (name, _func) in benches.iter() {
            if let Some(stats) = stats_map.get(*name) {
                print_bench_report(name, stats, old_stats_map.get(*name));
            }
        }
    }
}

fn display_grouped_by_benchmark(
    benches: &[&(&'static str, fn())],
    #[cfg(feature = "cachegrind")] cachegrind: &Option<
        Vec<(String, BenchStats, Option<BenchStats>)>,
    >,
    #[cfg(feature = "callgrind")] callgrind: &Option<(BenchStatsMap, BenchStatsMap)>,
) {
    for (name, _func) in benches.iter() {
        #[cfg(feature = "cachegrind")]
        if let Some(results) = cachegrind
            && let Some((_n, stats, old_stats)) = results.iter().find(|(n, _, _)| n == *name)
        {
            print_bench_report_labeled(name, "cachegrind", stats, old_stats.as_ref());
        }

        #[cfg(feature = "callgrind")]
        if let Some((stats_map, old_stats_map)) = callgrind
            && let Some(stats) = stats_map.get(*name)
        {
            print_bench_report_labeled(name, "callgrind", stats, old_stats_map.get(*name));
        }
    }
}

fn display_single_tool(
    benches: &[&(&'static str, fn())],
    #[cfg(feature = "cachegrind")] cachegrind: &Option<
        Vec<(String, BenchStats, Option<BenchStats>)>,
    >,
    #[cfg(feature = "callgrind")] callgrind: &Option<(BenchStatsMap, BenchStatsMap)>,
) {
    let _ = benches;

    #[cfg(feature = "cachegrind")]
    if let Some(results) = cachegrind {
        for (name, stats, old_stats) in results {
            print_bench_report(name, stats, old_stats.as_ref());
        }
    }

    #[cfg(feature = "callgrind")]
    if let Some((stats_map, old_stats_map)) = callgrind {
        for (name, _func) in benches.iter() {
            if let Some(stats) = stats_map.get(*name) {
                print_bench_report(name, stats, old_stats_map.get(*name));
            }
        }
    }
}

fn print_bench_report(name: &str, stats: &BenchStats, old_stats: Option<&BenchStats>) {
    let old_summary = old_stats.map(|s| s.summarize());
    let summary = stats.summarize();
    for line in format_benchmark_report(name, stats, old_stats, &summary, old_summary.as_ref()) {
        println!("{}", line);
    }
}

fn print_bench_report_labeled(
    name: &str,
    tool: &str,
    stats: &BenchStats,
    old_stats: Option<&BenchStats>,
) {
    let labeled_name = format!("{name} [{tool}]");
    let old_summary = old_stats.map(|s| s.summarize());
    let summary = stats.summarize();
    for line in format_benchmark_report(
        &labeled_name,
        stats,
        old_stats,
        &summary,
        old_summary.as_ref(),
    ) {
        println!("{}", line);
    }
}

fn format_benchmark_report(
    name: &str,
    stats: &BenchStats,
    old_stats: Option<&BenchStats>,
    summary: &BenchSummary,
    old_summary: Option<&BenchSummary>,
) -> Vec<String> {
    let mut lines = Vec::with_capacity(7);
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

// ──────────────────────────────────────────────────────────────
// Shared infrastructure
// ──────────────────────────────────────────────────────────────

fn check_valgrind(system: &impl System, tool: &str) -> bool {
    let mut command = Command::new("valgrind");
    command
        .arg(format!("--tool={tool}"))
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    match system.output(&mut command) {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "Failed to launch valgrind (--tool={}): {}. Please ensure that valgrind is installed and on the $PATH.",
                tool, output.status
            );
            false
        }
        Err(error) => {
            eprintln!("Unexpected error while launching valgrind: {}", error);
            false
        }
    }
}

fn get_arch() -> String {
    std::env::consts::ARCH.to_owned()
}

fn basic_valgrind() -> Command {
    Command::new("valgrind")
}

#[cfg(target_os = "linux")]
fn valgrind_without_aslr(arch: &str) -> Command {
    let mut command = Command::new("setarch");
    command.arg(arch).arg("-R").arg("valgrind");
    command
}

#[cfg(target_os = "freebsd")]
fn valgrind_without_aslr(_arch: &str) -> Command {
    let mut command = Command::new("proccontrol");
    command.arg("-m").arg("aslr").arg("-s").arg("disable");
    command
}

#[cfg(all(not(target_os = "linux"), not(target_os = "freebsd")))]
fn valgrind_without_aslr(_arch: &str) -> Command {
    basic_valgrind()
}

// ──────────────────────────────────────────────────────────────
// Cachegrind backend
// ──────────────────────────────────────────────────────────────

#[cfg(feature = "cachegrind")]
fn run_child_benchmark(benches: &[&(&'static str, fn())], benchmark_index: isize) {
    if benchmark_index < 0 {
        return;
    }
    if let Ok(index) = usize::try_from(benchmark_index)
        && index < benches.len()
    {
        (benches[index].1)();
    }
}

#[cfg(feature = "cachegrind")]
fn run_cachegrind(
    system: &impl System,
    arch: &str,
    executable: &str,
    benches: &[&(&'static str, fn())],
    allow_aslr: bool,
) -> Result<Vec<(String, BenchStats, Option<BenchStats>)>, RunnerError> {
    let (calibration, old_calibration) =
        run_bench(system, arch, executable, -1, "iai_calibration", allow_aslr)?;

    let mut results = Vec::with_capacity(benches.len());
    for (index, (name, _func)) in benches.iter().enumerate() {
        let (stats, old_stats) =
            run_bench(system, arch, executable, index as isize, name, allow_aslr)?;

        let stats = stats.subtract(&calibration);
        let old_stats = match (&old_stats, &old_calibration) {
            (Some(old_stats), Some(old_calibration)) => Some(old_stats.subtract(old_calibration)),
            _ => None,
        };

        results.push((name.to_string(), stats, old_stats));
    }

    Ok(results)
}

#[cfg(feature = "cachegrind")]
fn run_bench(
    system: &impl System,
    arch: &str,
    executable: &str,
    index: isize,
    benchmark_name: &str,
    allow_aslr: bool,
) -> Result<(BenchStats, Option<BenchStats>), RunnerError> {
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

#[cfg(feature = "cachegrind")]
fn parse_cachegrind_output(system: &impl System, path: &Path) -> Result<BenchStats, RunnerError> {
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

    if events_tokens.len() != summary_tokens.len() {
        return Err(RunnerError::ParseError(format!(
            "Unable to parse cachegrind output file {}: events and summary lengths do not match",
            path.display(),
        )));
    }

    let mut events: HashMap<String, u64> = HashMap::with_capacity(events_tokens.len());
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
        events.insert(event, value);
    }

    BenchStats::from_events(&events).map_err(RunnerError::ParseError)
}

// ──────────────────────────────────────────────────────────────
// Callgrind backend
// ──────────────────────────────────────────────────────────────

#[cfg(feature = "callgrind")]
fn run_child_benchmarks(benches: &[&(&'static str, fn())]) {
    for (_name, func) in benches.iter() {
        func();
    }
}

#[cfg(feature = "callgrind")]
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

#[cfg(feature = "callgrind")]
fn parse_callgrind_output(system: &impl System, path: &Path) -> Result<BenchStatsMap, RunnerError> {
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
            let _calls = lines
                .next()
                .ok_or_else(|| {
                    RunnerError::ParseError(format!(
                        "Unable to parse callgrind output file {}: unexpected end of file after cfn=__iai_bench_{}",
                        path.display(), name,
                    ))
                })?
                .map_err(RunnerError::Io)?;

            // Read the data line: "<position> <values...>"
            let data_line = lines
                .next()
                .ok_or_else(|| {
                    RunnerError::ParseError(format!(
                        "Unable to parse callgrind output file {}: unexpected end of file after calls line for {}",
                        path.display(), name,
                    ))
                })?
                .map_err(RunnerError::Io)?;

            let event_map = parse_data_line(events, &data_line, path)?;
            let stats = BenchStats::from_events(&event_map).map_err(RunnerError::ParseError)?;
            results.insert(name.to_owned(), stats);
        }
    }

    Ok(results)
}

/// Parse a single Callgrind data line into an event-value map.
///
/// Callgrind omits trailing zero values, so any events beyond the provided values
/// default to 0.
#[cfg(feature = "callgrind")]
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
                    path.display(), value_str, event_name, error,
                ))
            })?,
            None => 0,
        };
        event_map.insert(event_name.clone(), value);
    }

    Ok(event_map)
}

// ──────────────────────────────────────────────────────────────
// Error types
// ──────────────────────────────────────────────────────────────

#[derive(Debug)]
enum RunnerError {
    Io(io::Error),
    CommandFailed(ExitStatus),
    InvalidOutputPath(PathBuf),
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

// ──────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{EnvironmentPort, FilePort, ProcessPort};
    use std::cell::Cell;
    use std::fs;
    use std::process::Output;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static BENCH_ONE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static BENCH_TWO_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn bench_one() {
        BENCH_ONE_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn bench_two() {
        BENCH_TWO_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    fn reset_bench_counts() {
        BENCH_ONE_CALLS.store(0, Ordering::SeqCst);
        BENCH_TWO_CALLS.store(0, Ordering::SeqCst);
    }

    fn bench_one_calls() -> usize {
        BENCH_ONE_CALLS.load(Ordering::SeqCst)
    }

    fn bench_two_calls() -> usize {
        BENCH_TWO_CALLS.load(Ordering::SeqCst)
    }

    fn with_tmp_file(contents: &str) -> PathBuf {
        static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        let mut path = std::env::temp_dir();
        path.push(format!("iai-test-{}-{}.out", std::process::id(), id));
        fs::write(&path, contents).expect("failed to write temporary output");
        path
    }

    // ── Cachegrind parse tests ──────────────────────────────

    #[cfg(feature = "cachegrind")]
    fn read_and_cleanup_cachegrind(path: PathBuf) -> Result<BenchStats, RunnerError> {
        let result = {
            let system = StandardSystem::new();
            parse_cachegrind_output(&system, &path)
        };
        let _ = fs::remove_file(path);
        result
    }

    #[cfg(feature = "cachegrind")]
    #[test]
    fn parse_cachegrind_output_parses_valid_output() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\nsummary: 10 1 2 3 4 5 6 7 8\n",
        );
        let parsed = read_and_cleanup_cachegrind(path).expect("expected parse success");

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

    #[cfg(feature = "cachegrind")]
    #[test]
    fn parse_cachegrind_output_missing_events_line() {
        let path = with_tmp_file("summary: 10 1 2 3 4 5 6 7 8\n");
        let err = read_and_cleanup_cachegrind(path).expect_err("expected parse failure");
        assert!(matches!(err, RunnerError::ParseError(m) if m.contains("missing events line")));
    }

    #[cfg(feature = "cachegrind")]
    #[test]
    fn parse_cachegrind_output_missing_summary_line() {
        let path = with_tmp_file("events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n");
        let err = read_and_cleanup_cachegrind(path).expect_err("expected parse failure");
        assert!(matches!(err, RunnerError::ParseError(m) if m.contains("missing summary line")));
    }

    #[cfg(feature = "cachegrind")]
    #[test]
    fn parse_cachegrind_output_requires_matching_token_lengths() {
        let path = with_tmp_file("events: Ir I1mr ILmr\nsummary: 10 1 2 3\n");
        let err = read_and_cleanup_cachegrind(path).expect_err("expected parse failure");
        assert!(matches!(err, RunnerError::ParseError(m) if m.contains("lengths do not match")));
    }

    #[cfg(feature = "cachegrind")]
    #[test]
    fn parse_cachegrind_output_rejects_non_numeric_data() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\nsummary: 10 1 a 3 4 5 6 7 8\n",
        );
        let err = read_and_cleanup_cachegrind(path).expect_err("expected parse failure");
        assert!(matches!(err, RunnerError::ParseError(m) if m.contains("value 'a'")));
    }

    // ── Callgrind parse tests ───────────────────────────────

    #[cfg(feature = "callgrind")]
    fn read_and_cleanup_callgrind(path: PathBuf) -> Result<BenchStatsMap, RunnerError> {
        let result = {
            let system = StandardSystem::new();
            parse_callgrind_output(&system, &path)
        };
        let _ = fs::remove_file(path);
        result
    }

    #[cfg(feature = "callgrind")]
    #[test]
    fn parse_callgrind_output_parses_valid_output() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 10 1 2 3 4 5 6 7 8\n",
        );
        let parsed = read_and_cleanup_callgrind(path).expect("expected parse success");
        let stats = parsed.get("my_bench").expect("expected my_bench");

        assert_eq!(stats.instruction_reads, 10);
        assert_eq!(stats.instruction_cache_misses, 2);
        assert_eq!(stats.data_cache_write_misses, 8);
    }

    #[cfg(feature = "callgrind")]
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
        let parsed = read_and_cleanup_callgrind(path).expect("expected parse success");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("bench_a").unwrap().instruction_reads, 10);
        assert_eq!(parsed.get("bench_b").unwrap().instruction_reads, 20);
    }

    #[cfg(feature = "callgrind")]
    #[test]
    fn parse_callgrind_output_handles_trailing_zeros_omitted() {
        let path = with_tmp_file(
            "events: Ir I1mr ILmr Dr D1mr DLmr Dw D1mw DLmw\n\
             cfn=__iai_bench_my_bench\n\
             calls=1 0\n\
             0 42 3 1\n",
        );
        let parsed = read_and_cleanup_callgrind(path).expect("expected parse success");
        let stats = parsed.get("my_bench").unwrap();
        assert_eq!(stats.instruction_reads, 42);
        assert_eq!(stats.data_reads, 0);
    }

    #[cfg(feature = "callgrind")]
    #[test]
    fn parse_callgrind_output_missing_events_line() {
        let path = with_tmp_file("cfn=__iai_bench_my_bench\ncalls=1 0\n0 10 1 2 3 4 5 6 7 8\n");
        let err = read_and_cleanup_callgrind(path).expect_err("expected parse failure");
        assert!(matches!(err, RunnerError::ParseError(m) if m.contains("events line")));
    }

    // ── Runner dispatch tests ───────────────────────────────

    #[cfg(feature = "cachegrind")]
    #[test]
    fn runner_child_indexed_mode_dispatches_selected_benchmark() {
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

    #[cfg(feature = "callgrind")]
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
        // The runner returns early after the first valgrind probe failure,
        // so only 1 output call is made regardless of how many tools are enabled.
        let has_any_tool = cfg!(feature = "cachegrind") || cfg!(feature = "callgrind");
        let expected_output_calls = if has_any_tool { 1 } else { 0 };
        assert_eq!(system.output_calls.get(), expected_output_calls);
    }

    // ── Fake system ─────────────────────────────────────────

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
        fn create_dir_all(&self, _path: &Path) -> io::Result<()> {
            Ok(())
        }
        fn copy_file(&self, _from: &Path, _to: &Path) -> io::Result<u64> {
            Ok(0)
        }
        fn file_exists(&self, _path: &Path) -> bool {
            false
        }
        fn open_file(&self, _path: &Path) -> io::Result<fs::File> {
            Err(io::Error::new(io::ErrorKind::NotFound, "not found"))
        }
    }

    impl ProcessPort for FakeSystem {
        fn status(&self, _command: &mut Command) -> io::Result<ExitStatus> {
            Err(io::Error::other("not used in this test"))
        }
        fn output(&self, _command: &mut Command) -> io::Result<Output> {
            self.output_calls.set(self.output_calls.get() + 1);
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "valgrind not available",
            ))
        }
    }

    impl EnvironmentPort for FakeSystem {
        fn var_os(&self, _key: &str) -> Option<std::ffi::OsString> {
            None
        }
    }
}

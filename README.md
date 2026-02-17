<h1 align="center">Iai</h1>

<div align="center">Experimental One-shot Benchmark Framework in Rust</div>

<div align="center">
    <img src="https://github.com/bheisler/iai/workflows/Continuous%20integration/badge.svg" alt="Continuous integration">
</div>

<div align="center">
	<a href="https://bheisler.github.io/criterion.rs/book/iai/getting_started.html">Getting Started</a>
    |
    <a href="https://bheisler.github.io/criterion.rs/book/iai/iai.html">User Guide</a>
    |
    <a href="https://docs.rs/crate/iai/">Released API Docs</a>
    |
    <a href="https://github.com/bheisler/iai/blob/master/CHANGELOG.md">Changelog</a>
</div>

Iai is an experimental benchmarking harness that uses Valgrind's cache-simulation tools to perform
extremely precise single-shot measurements of Rust code.

By default Iai uses **Cachegrind**. An optional **Callgrind** backend is available via feature flag,
and both tools can run side-by-side when both features are enabled.

## Table of Contents
- [Table of Contents](#table-of-contents)
  - [Features](#features)
  - [Quickstart](#quickstart)
  - [Profiling Backends](#profiling-backends)
  - [Fork Motivation and Architecture](#fork-motivation-and-architecture)
  - [Goals](#goals)
  - [Comparison with Criterion-rs](#comparison-with-criterion-rs)
  - [Contributing](#contributing)
  - [Compatibility Policy](#compatibility-policy)
  - [Acknowledgments](#acknowledgments)
  - [License](#license)

### Features

- __Precision__: High-precision measurements allow you to reliably detect very small optimizations to your code
- __Consistency__: Iai can take accurate measurements even in virtualized CI environments
- __Performance__: Since Iai only executes a benchmark once, it is typically faster to run than statistical benchmarks
- __Profiling__: Iai generates a Cachegrind (or Callgrind) profile of your code while benchmarking, so you can use compatible tools to analyze the results in detail
- __Stable-compatible__: Benchmark your code without installing nightly Rust
- __Dual backends__: Choose Cachegrind, Callgrind, or both via Cargo feature flags

### Quickstart

In order to use Iai, you must have [Valgrind] installed. This means that Iai cannot be used on
platforms that are not supported by Valgrind.

Valgrind versions that default cache simulation to `--cache-sim=no` are supported because
Iai passes `--cache-sim=yes` when running benchmarks. This ensures output always includes the
cache hierarchy counters required by the parser (`Ir`, `I1mr`, `ILmr`, `Dr`, `D1mr`, `DLmr`,
`Dw`, `D1mw`, `DLmw`).

[Valgrind]: https://www.valgrind.org

To start with Iai, add the following to your `Cargo.toml` file:

```toml
[dev-dependencies]
iai = "0.2"

[[bench]]
name = "my_benchmark"
harness = false
```

Next, define a benchmark by creating a file at `$PROJECT/benches/my_benchmark.rs` with the following contents:

```rust
use iai::black_box;

fn fibonacci(n: u64) -> u64 {
    match n {
        0 => 1,
        1 => 1,
        n => fibonacci(n-1) + fibonacci(n-2),
    }
}

fn iai_benchmark_short() -> u64 {
    fibonacci(black_box(10))
}

fn iai_benchmark_long() -> u64 {
    fibonacci(black_box(30))
}


iai::main!(iai_benchmark_short, iai_benchmark_long);
```

Finally, run this benchmark with `cargo bench`. You should see output similar to the following:

```
     Running target/release/deps/test_regular_bench-8b173c29ce041afa

bench_fibonacci_short
  Instructions:                1735
  L1 Accesses:                 2364
  L2 Accesses:                    1
  RAM Accesses:                   1
  Estimated Cycles:            2404

bench_fibonacci_long
  Instructions:            26214735
  L1 Accesses:             35638623
  L2 Accesses:                    2
  RAM Accesses:                   1
  Estimated Cycles:        35638668
```

### Profiling Backends

Iai supports two Valgrind-based profiling backends controlled by Cargo feature flags:

| Configuration | Behavior |
|---|---|
| Default (no flags) | Cachegrind only |
| `default-features = false, features = ["callgrind"]` | Callgrind only |
| `features = ["callgrind"]` (with defaults) | Both tools run |

**Cachegrind** (default) runs each benchmark in a separate Valgrind invocation with a calibration
pass to subtract harness overhead. Output files are written to `target/iai/cachegrind.out.<name>`.

**Callgrind** runs all benchmarks in a single Valgrind invocation using `--toggle-collect` to
measure each benchmark function individually. Output is written to `target/iai/callgrind.out`.

To use Callgrind instead of Cachegrind:

```toml
[dev-dependencies]
iai = { version = "0.2", default-features = false, features = ["callgrind"] }
```

To run both tools:

```toml
[dev-dependencies]
iai = { version = "0.2", features = ["callgrind"] }
```

When both tools are enabled, results are grouped by tool:

```
=== cachegrind ===
bench_fibonacci_short
  Instructions:                1735
  ...

=== callgrind ===
bench_fibonacci_short
  Instructions:                1723
  ...
```

Set the `IAI_GROUP_BY_BENCHMARK` environment variable to group by benchmark instead:

```
IAI_GROUP_BY_BENCHMARK=1 cargo bench
```

```
bench_fibonacci_short [cachegrind]
  Instructions:                1735
  ...
bench_fibonacci_short [callgrind]
  Instructions:                1723
  ...
```

#### Cachegrind vs Callgrind

Both tools produce the same 9 cache-simulation counters and yield nearly identical instruction
counts for the same benchmark code. The key differences are in how they invoke Valgrind and
handle harness overhead:

| | Cachegrind | Callgrind |
|---|---|---|
| Valgrind invocations | N+1 (1 calibration + 1 per benchmark) | 1 (all benchmarks in a single run) |
| Overhead removal | Calibration subtraction | `--toggle-collect` (measures only benchmark functions) |
| Output files | One per benchmark (`cachegrind.out.<name>`) | One shared file (`callgrind.out`) |
| Scales with N benchmarks | Linearly (more process spawns) | Constant (single invocation) |
| Profile tooling | `cg_annotate` | `callgrind_annotate`, KCachegrind |

Instruction counts agree to within a few instructions. The small differences come from
calibration arithmetic (cachegrind) vs toggle-collect boundaries (callgrind).

Callgrind runs all benchmarks in a single Valgrind process, so harness wall time stays roughly
constant regardless of benchmark count. Cachegrind spawns N+1 processes, so wall time grows
linearly. For most use cases the choice comes down to: callgrind for faster runs and richer
profiling tools, cachegrind for compatibility with the original Iai ecosystem.

#### Performance vs madsmtm/iai

The callgrind backend in this fork produces identical instruction counts to
[madsmtm/iai](https://github.com/madsmtm/iai/tree/callgrind). Wall-clock harness overhead
was measured by running each binary directly (bypassing `cargo bench`) in 15 alternating
iterations:

**iai built-in benchmarks** (3 benchmarks including `fibonacci(30)`):

|                       | Min   | Mean  | Median | Max   |
|-----------------------|-------|-------|--------|-------|
| This fork (callgrind) | 507ms | 616ms | 609ms  | 756ms |
| madsmtm callgrind     | 507ms | 680ms | 704ms  | 779ms |

**ICU4X zerovec benchmarks** (8 benchmarks):

|                       | Min   | Mean  | Median | Max   |
|-----------------------|-------|-------|--------|-------|
| This fork (callgrind) | 227ms | 282ms | 289ms  | 324ms |
| madsmtm callgrind     | 216ms | 284ms | 296ms  | 366ms |

The modular architecture introduces no measurable overhead. Two targeted changes reduce
harness-side work:

1. **Eliminated `uname -m` subprocess**: madsmtm spawns `uname -m` via `Command::new("uname")`
   to detect CPU architecture, requiring `pipe2` × 2, `clone3`, `execve`, and `wait4` syscalls.
   This fork uses Rust's compile-time `std::env::consts::ARCH`, removing a full fork/exec cycle.

2. **Suppressed Valgrind diagnostic output**: Valgrind's stderr (copyright banner, event summary,
   cache statistics) is redirected to `/dev/null` via `Stdio::null()`, avoiding kernel-side write
   buffering for ~20 lines of output per invocation.

These changes reduce the parent process syscall count (298 vs 313 on zerovec; 270 vs 285 on
the built-in benchmarks) and eliminate one child process per run (2 vs 3).

The trait-based architecture (`System` trait for file/process/environment access) is fully
monomorphized in release builds — the compiler inlines all trait method calls, producing
identical machine code to direct `std` calls.

### Fork Motivation and Architecture

This fork of [bheisler/iai](https://github.com/bheisler/iai) was created to address several issues
with the upstream project:

1. **Valgrind compatibility**: Newer versions of Valgrind default to `--cache-sim=no`, which causes
   the original upstream to panic. This fork explicitly passes `--cache-sim=yes`.
2. **Callgrind support**: The callgrind backend from
   [madsmtm/iai](https://github.com/madsmtm/iai/tree/callgrind) provides faster benchmarking
   through single-invocation profiling and enables richer analysis via KCachegrind.
3. **Dual-tool mode**: Users can now run both cachegrind and callgrind in a single `cargo bench`
   invocation, comparing results from both tools side-by-side.
4. **Rust edition 2024**: The codebase has been migrated to edition 2024 with MSRV 1.93.

The internal architecture has been decomposed from a single `lib.rs` into focused modules:

- **`domain`**: Pure types and logic (stats, invocation parsing, formatting) with no I/O
- **`application`**: Orchestration of benchmark runs, Valgrind invocation, and output display
- **`ports`**: Trait boundaries for file system, process, and environment access
- **`infrastructure`**: Production implementations of the port traits

This separation makes the profiling backends independently testable via trait-based dependency
injection and allows feature-gating each backend without tangling shared infrastructure.

### Goals

The primary goal of Iai is to provide a simple and precise tool for reliably detecting very small changes to the performance of code. Additionally, it should be as programmer-friendly as possible and make it easy to create reliable, useful benchmarks.

### Comparison with Criterion-rs

I intend Iai to be a complement to Criterion-rs, not a competitor. The two projects measure different
things in different ways and have different pros, cons, and limitations, so for most projects the
best approach is to use both.

Here's an overview of the important differences:
- Temporary Con: Right now, Iai is lacking many features of Criterion-rs, including reports and configuration of any kind.
    - The current intent is to add support to [Cargo-criterion] for configuring and reporting on Iai benchmarks.
- Pro: Iai can reliably detect much smaller changes in performance than Criterion-rs can.
- Pro: Iai can work reliably in noisy CI environments or even cloud CI providers like GitHub Actions or Travis-CI, where Criterion-rs cannot.
- Pro: Iai also generates profile output (Cachegrind and/or Callgrind) from the benchmark without further effort.
- Pro: Although Valgrind adds considerable runtime overhead, running each benchmark exactly once is still usually faster than Criterion-rs' statistical measurements.
- Mixed: Because Iai can detect such small changes, it may report performance differences from changes to the order of functions in memory and other compiler details.
- Con: Iai's measurements merely correlate with wall-clock time (which is usually what you actually care about), where Criterion-rs measures it directly.
- Con: Iai cannot exclude setup code from the measurements, where Criterion-rs can.
- Con: Because Valgrind's cache simulators do not measure system calls, IO time is not accurately measured.
- Con: Because Iai runs the benchmark exactly once, it cannot measure variation in the performance such as might be caused by OS thread scheduling or hash-table randomization.
- Limitation: Iai can only be used on platforms supported by Valgrind. Notably, this does not include Windows.

For benchmarks that run in CI (especially if you're checking for performance regressions in pull 
requests on cloud CI) you should use Iai. For benchmarking on Windows or other platforms that
Valgrind doesn't support, you should use Criterion-rs. For other cases, I would advise using both.
Iai gives more precision and scales better to larger benchmarks, while Criterion-rs allows for
excluding setup time and gives you more information about the actual time your code takes and how
strongly that is affected by non-determinism like threading or hash-table randomization. If you
absolutely need to pick one or the other though, Iai is probably the one to go with.

[Cargo-criterion]: https://github.com/bheisler/cargo-criterion

### Contributing

First, thank you for contributing.

One great way to contribute to Iai is to use it for your own benchmarking needs and report your experiences, file and comment on issues, etc.

Code or documentation improvements in the form of pull requests are also welcome. If you're not
sure what to work on, try checking the 
[Beginner label](https://github.com/bheisler/iai/issues?q=is%3Aissue+is%3Aopen+label%3ABeginner).

If your issues or pull requests have no response after a few days, feel free to ping me (@bheisler).

For more details, see the [CONTRIBUTING.md file](https://github.com/bheisler/iai/blob/master/CONTRIBUTING.md).

### Compatibility Policy

Iai is developed and tested against current stable Rust in CI. Older versions may work, but are not guaranteed.

### Acknowledgments

Iai was originally written by Brook Heisler ([@bheisler](https://github.com/bheisler)).

The Callgrind backend is based on work by Mads Marquart
([@madsmtm](https://github.com/madsmtm)) from the
[callgrind branch](https://github.com/madsmtm/iai/tree/callgrind) of his Iai fork.

### License

Iai is dual licensed under the Apache 2.0 license and the MIT license.

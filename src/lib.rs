//! Experimental one-shot benchmarking framework using Valgrind cache simulation.
//!
//! By default Iai uses **Cachegrind**. Enable the `callgrind` feature for
//! Callgrind support, or enable both features to run both tools side-by-side.
//!
//! ## Feature flags
//!
//! | Feature | Default | Description |
//! |---|---|---|
//! | `cachegrind` | yes | Cachegrind profiling backend |
//! | `callgrind` | no | Callgrind profiling backend |
//! | `macro` | no | Procedural macro support (`#[iai]`) |
//! | `real_blackbox` | no | Use nightly `test::black_box` intrinsic |
//!
//! When both `cachegrind` and `callgrind` are enabled, results are grouped by
//! tool. Set `IAI_GROUP_BY_BENCHMARK=1` to group by benchmark instead.

#[cfg(feature = "macro")]
pub use iai_macro::iai;

mod application;
mod black_box;
mod domain;
mod infrastructure;
/// Legacy macro entrypoints for compatibility with custom-test-framework harness usage.
/// Test-framework macro entrypoint.
mod macros;
mod ports;

/// Inline black-box API re-export for benchmarked functions.
pub use black_box::black_box;

/// Custom-test-framework runner. Should not be called directly.
#[doc(hidden)]
/// The harness invokes this at startup so generated benchmarks can be launched
/// through either parent mode (scheduler) or child mode (`--iai-run N`).
pub fn runner(benches: &[&(&'static str, fn())]) {
    application::runner(benches);
}

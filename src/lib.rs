//! Public crate façade for IAI runner and optional benchmark macro re-export.
//!
//! The crate keeps runtime behavior split across:
//! - Domain types/functions (`src/domain.rs`)
//! - Application orchestration (`src/application.rs`)
//! - Port traits (`src/ports.rs`)
//! - Infrastructure adapters (`src/infrastructure.rs`)
//!
//! This top-level module only wires modules together and preserves the existing
//! public API.

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

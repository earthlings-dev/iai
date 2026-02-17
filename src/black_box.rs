//! Prevent compiler optimizations from eliding benchmark inputs and outputs.
//!
//! Historically, a `real_blackbox` feature existed to opt into the unstable
//! `test::black_box` implementation. In this refactor, `std::hint::black_box` is
//! used on all channels to avoid requiring nightly-only crate attributes and keep
//! the API stable across feature combinations.
pub fn black_box<T>(dummy: T) -> T {
    std::hint::black_box(dummy)
}

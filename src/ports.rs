use std::{
    ffi::OsString,
    fs::File,
    io,
    path::Path,
    process::{Command, ExitStatus, Output},
};

/// File-system boundary used by the benchmark runner and parsing logic.
///
/// These methods are intentionally narrow so `application` and `domain` logic can be
/// exercised with pure fakes in tests.
pub(crate) trait FilePort {
    /// Ensure a directory hierarchy exists.
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    /// Copy a file, returning the number of bytes copied on success.
    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<u64>;
    /// Check whether a path exists.
    fn file_exists(&self, path: &Path) -> bool;
    /// Open a file for reading.
    fn open_file(&self, path: &Path) -> io::Result<File>;
}

/// Process-execution boundary for command invocation.
///
/// The port separates process lifecycle control (`status`) from captured command
/// output (`output`), matching the two distinct usage points in the runner.
pub(crate) trait ProcessPort {
    /// Execute a command and wait for status.
    fn status(&self, command: &mut Command) -> io::Result<ExitStatus>;
    /// Execute a command and capture all output.
    fn output(&self, command: &mut Command) -> io::Result<Output>;
}

/// Environment boundary for reading variables used by runner configuration.
///
/// Environment reads are isolated behind a trait to keep invocation behavior
/// deterministic in tests and to avoid cross-cutting side effects in core code.
pub(crate) trait EnvironmentPort {
    /// Lookup an environment variable by key.
    fn var_os(&self, key: &str) -> Option<OsString>;
}

/// Aggregate trait representing the runtime dependencies needed by the runner.
///
/// `System` composes the three infrastructure concerns required by the
/// application layer:
/// file operations, process execution, and environment lookup.
pub(crate) trait System: FilePort + ProcessPort + EnvironmentPort {}

impl<T: FilePort + ProcessPort + EnvironmentPort> System for T {}

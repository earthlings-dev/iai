use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    path::Path,
    process::{Command, ExitStatus, Output},
};

use crate::ports::{EnvironmentPort, FilePort, ProcessPort};

/// Standard production adapter for all side-effect boundaries.
///
/// This type delegates directly to the Rust standard library and `std::env`.
pub(crate) struct StandardSystem;

impl StandardSystem {
    /// Create a fresh handle to the process/environment adapter.
    ///
    /// The value has no interior state; this is a thin constructor for symmetry
    /// with test doubles used by the application layer.
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Default for StandardSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl FilePort for StandardSystem {
    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        // Uses std::fs to preserve parent-directory and permission behavior
        // exactly as the production environment expects.
        fs::create_dir_all(path)
    }

    fn copy_file(&self, from: &Path, to: &Path) -> io::Result<u64> {
        // Returns copied byte count for parity with std::fs::copy.
        fs::copy(from, to)
    }

    fn file_exists(&self, path: &Path) -> bool {
        // Existence checks are used to detect prior cachegrind output for
        // baseline comparisons.
        path.exists()
    }

    fn open_file(&self, path: &Path) -> io::Result<File> {
        File::open(path)
    }
}

impl ProcessPort for StandardSystem {
    fn status(&self, command: &mut Command) -> io::Result<ExitStatus> {
        // Execute the prepared command and wait, preserving the caller's stdout/
        // stderr configuration.
        command.status()
    }

    fn output(&self, command: &mut Command) -> io::Result<Output> {
        // Capture all output only for capability probes (valgrind availability).
        command.output()
    }
}

impl EnvironmentPort for StandardSystem {
    /// Read an OS environment variable.
    fn var_os(&self, key: &str) -> Option<OsString> {
        // Delegates to process environment; absence means default benchmark flow.
        std::env::var_os(key)
    }
}

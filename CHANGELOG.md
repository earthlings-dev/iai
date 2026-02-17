# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](http://keepachangelog.com/en/1.0.0/)
and this project adheres to [Semantic Versioning](http://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0]
### Added
- Callgrind profiling backend, available via `features = ["callgrind"]`. Based on
  [madsmtm/iai](https://github.com/madsmtm/iai/tree/callgrind)'s callgrind branch.
- Dual-tool mode: enable both `cachegrind` (default) and `callgrind` features to
  run both profilers side-by-side.
- `IAI_GROUP_BY_BENCHMARK` environment variable to switch dual-tool output from
  group-by-tool (default) to group-by-benchmark.

### Fixed
- Pass `--cache-sim=yes` explicitly when invoking Valgrind, fixing a panic on
  newer Valgrind versions that default to `--cache-sim=no`.

### Changed
- Internal architecture decomposed into domain, application, ports, and
  infrastructure modules for independent testability of each profiling backend.
- Migrated to Rust edition 2024 (MSRV 1.93).

## [0.1.1]
### Added
- Initial implementation.


[Unreleased]: https://github.com/earthlings-dev/iai/compare/0.2.0...HEAD
[0.2.0]: https://github.com/earthlings-dev/iai/compare/0.1.1...0.2.0
[0.1.1]: https://github.com/bheisler/iai/compare/...0.1.1
# Changelog

All notable changes to Litz. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [semantic versioning](https://semver.org/spec/v2.0.0.html) with the `0.x` caveat that a minor release may break compatibility.

Every crate in the workspace and the npm package carry the same version. Releases before `0.2.0` were published to crates.io as `0.1.x` without tags or notes.

## [Unreleased]

## [0.4.1] - 2026-11-23

### Added

- Every workspace crate carries the same version as the npm package and the GitHub release, and internal path dependencies moved to `[workspace.dependencies]`, so a release edits one file. `tools/set-version.sh` writes the version to `Cargo.toml` and `js/package.json`.
- The publish workflow releases the crates with crates.io trusted publishing, in dependency order, skipping versions already on the registry. It refuses a tag whose version disagrees with `Cargo.toml` or `js/package.json`.
- `CHANGELOG.md`, `CONTRIBUTING.md` and `SECURITY.md`.

### Changed

- The docs link the homepage and the WIT contracts at their GitHub paths, and state the current npm version.

## [0.4.0] - 2026-11-23

### Added

- CircuitPython boots correctly in a multi-board fleet: the runner reads each board's image before choosing console options, so the interpreter gets the Bluetooth controller it initializes on boot.
- MicroPython can write its filesystem on the RP2040: the bootrom flash range-erase and program helpers drive a model of the control block instead of returning immediately.

### Changed

- The npm package is versioned `0.4.0`, matching the GitHub release.
- The JavaScript reference documents fleet sessions across several boards.

### Fixed

- The debugger reads breakpoint words without assuming a slice length.

## [0.3.0] - 2026-11-23

### Added

- Multi-board sessions in the JavaScript package, and a RISC-V debugger.
- A chip conformance suite and a guide for writing a wasm MCU.
- A docsify documentation site under `docs/`, served on its own origin.

### Changed

- The README lists what the simulator covers as a feature table.
- Every crate that builds against a WIT contract ships that contract.
- The RP2040 documentation states what the core models and what it leaves out.

### Fixed

- Path dependencies carry version requirements, so the published crates resolve from the registry.
- The AVR and RP2040 fault paths have tests.

## [0.2.1] - 2026-11-22

### Added

- Voltage and current control on the bench power supply.
- The Rust reference documents the embedding API.

### Changed

- The project is licensed under Apache-2.0, and every crate and the npm package ship the license text.
- The README describes the editor and project crates.

### Fixed

- The publish workflow skips an npm version that is already on the registry.
- The probe channel description matches the instrument.

## [0.2.0] - 2026-11-22

### Added

- The `@litz-sim/litz` npm package, with the full host bridge for parts, chips, buses, analog and radios.
- A publish workflow that uses npm trusted publishing.

### Fixed

- The RP2040 USB controller completes its control handshake.
- Example firmware is committed, so the CLI tests boot real images.

[Unreleased]: https://github.com/Litz-Sim/litz/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/Litz-Sim/litz/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/Litz-Sim/litz/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/Litz-Sim/litz/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/Litz-Sim/litz/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Litz-Sim/litz/releases/tag/v0.2.0

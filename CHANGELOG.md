# Changelog

All notable changes to Litz. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [semantic versioning](https://semver.org/spec/v2.0.0.html) with the `0.x` caveat that a minor release may break compatibility.

Every crate in the workspace and the npm package carry the same version. Releases before `0.2.0` were published to crates.io as `0.1.x` without tags or notes.

## [Unreleased]

## [0.8.0] - 2026-11-28

### Added

- Zener breakdown in the analog solver: `DiodeModel::breakdown_volts` (and `DiodeModel::zener`) adds a reverse junction whose `Vz` is quoted at a 5 mA test current. `litz-zener` now reaches the netlist with its `vz` instead of blocking like a rectifier, so a zener reference or clamp on a diagram solves to the voltage a hand calculation predicts.
- `litz-voltage-regulator` is modelled in the netlist: a source from `VO` to `GND` at 5 V (7805), 12 V (7812), −5 V (7905) or the `voltage` attribute (LM317, also movable by a host while the board runs). A regulator whose input is within its dropout folds to `VI` minus 2 V (1.5 V for an LM317), so a 7805 on a 6 V supply reports about 4 V rather than an output its pass element cannot make.
- A DAC on the bus drives the analog netlist: `Board::analog_sources` reads the MCP4725's latched code and stamps `code/4095 × vref` on `VOUT`, with `vref` taken from the rail on `VCC` when there is one. An I2C write to the DAC drops the cached operating point.
- An ADC on the bus reads the netlist: ADS1115/ADS1015 (`A0..A3`, newly added pins) and MCP3008 (`CH0..CH7`) conversions take the solved voltage on the channel pin before the transaction; a channel whose pin is unwired keeps its manifest attribute. `Ads111x::set_inputs` and `Mcp3008::set_channels` are the entry points, and both devices now support mutable downcasting.

## [0.6.3] - 2026-11-25

### Added

- `litz-multimeter`: a two-lead meter on the analog shelf. `mode = "voltage"` stands it across pins `+`/`-` behind 10 MΩ, `mode = "current"` puts it in series behind a 0.01 Ω shunt, and the reading is the solve's voltage across those pins or current through that element.
- `Session#partVoltage(id, pin)` and `Session#partCurrent(id)` expose that reading, backed by the new `litz_part_voltage` / `litz_part_current` exports and `Board::node_current`. A floating node returns `null` rather than 0 V.

## [0.6.2] - 2026-11-25

### Added

- A host can put files on a storage part without a project directory: `createSession({ storage })` takes `{ [partId]: [{ path, data }] }`, formats the same deterministic FAT volume the `files` attribute builds, and the new `Session#storageFiles(part)` reads the volume back as the guest left it. This is the browser path for a microSD card or a W25Q flash; the attribute names a host directory, which a wasm build cannot read.
- `Session#flashImage(board?)` returns the MCU's own flash as the run left it, the JavaScript counterpart of `litz run --flash-export`.

### Fixed

- `Session#setNamedControl` accepts a board id. A board is not a leaf part, so the entry refused `setNamedControl("esp", "power", "off")` with `no part "esp" in the diagram` before the board's own power switch could answer. Boards are now tried first, and an unknown id still reports the old message.

## [0.6.1] - 2026-11-24

### Fixed

- `litz-mcu-wasm` and `litz-chip-rv32` bind the contract from a copy inside their own package instead of a path into `litz-mcu-wit`, which does not exist for a crate taken from the registry. `cargo install litz-cli@0.6.0` failed to compile `litz-mcu-wasm`; 0.6.1 installs. `crates/litz-mcu-wit/tests/mirrors.rs` fails when any copy stops being byte-identical to `crates/litz-mcu-wit/wit/litz-mcu.wit`.
- `docs/chips.md` and `docs/architecture.md` describe the one 0.2 contract: the 0.1 ABI is retired and refused by name rather than linked alongside 0.2.

## [0.6.0] - 2026-11-24

### Added

- One chip debug surface, answered by every backend: `debug_pc`, `debug_register_file`, `debug_read_memory`, `debug_disassemble`, `debug_set_breakpoints` and `debug_step` on `litz_board::Chip`, with `ChipDebug` for a backend that keeps debugging state. The ESP32 family, the RP2040 and custom wasm chips answer the same calls, and the JavaScript session reaches them through the same ABI.
- The RP2040 is a full backend: an ARMv6-M core with its clocks, timer, UART and USB peripherals, a `Machine` that owns them, its flash, and a checked-in thumb disassembler behind the debug calls.
- `Chip::flash_image` returns the MCU's own flash as the run left it, and `litz run --flash-export PATH` writes it. The ESP32 backends expose it through the new `SocBus::flash_image` (litz-esp-soc 0.1.2).
- SD cards and W25Q flash take a `size` attribute and model a FAT volume: files the host places are on the card the firmware mounts, and files the firmware writes come back with the image.
- The logic analyzer probes any header pin or part pin with no channel limit, and a probe on an unwired pin reads a flat high-impedance channel. `litz run --sr FILE [--sr-rate HZ]` writes the capture as a sigrok session for PulseView, alongside the existing CSV.
- Boards have a power switch: a `power` boolean attribute (default `true`) sets whether a board starts powered, `Board::set_board_power` flips it during a run, `set_named_control(board, "power", ...)` or a scenario's `set-control: {part-id: bridge, name: power, value: on}` does the same, and a board wired to another board's 5 V header is powered while that supply is on. `Board::board_powered`, `board_power_switch` and `release_chip_outputs` report and act on the state.

### Changed

- Losing power resets a board: its core stops, its pads are released and its rails read 0 V; power coming back boots the firmware from its reset vector with the flash it had, and a board that is off still advances the clock so it does not stall the one that is on. An unpowered chip's pads read a weak low rather than `HIGH_Z`, so a resistor that was mirroring the pad's last level cannot keep a net alive.
- Each board's `3V3`, `5V` and `VIN` are that board's own nets (`Endpoint::BoardRail`) and ground stays shared, so wiring two boards' `3V3` pins together joins their regulators instead of naming one global supply.
- `litz-esp-soc` is 0.1.2 and the SoC crates are 0.1.1; `litz-esp` requires those registry versions.

### Removed

- The extra WIT copies: `crates/litz-mcu-wit/wit/litz-mcu.wit` is the one chip contract, and the scaffolder copies it into a new board's `wit/` instead of shipping its own `wit-v2/`.

## [0.5.1] - 2026-11-23

### Fixed

- `litz-esp-soc` is 0.1.1 and carries the `Machine::set_breakpoints` and `Machine::step_one` methods that `litz-esp` 0.5.0 calls. 0.5.0 shipped those calls while the registry copy of the vendored crate was still the one without them, so `cargo install --locked litz-cli@0.5.0` failed to compile `litz-esp`; `litz-esp` now requires 0.1.1.
- The publish workflow runs `tools/install-smoke.sh` against the version it just published, before the GitHub release is created. A registry graph that disagrees with the path graph now fails the release instead of waiting for the scheduled install smoke.

## [0.5.0] - 2026-11-23

### Added

- Each display part's own framebuffer is reachable from the host: `Session#partDisplay(id)` in the JavaScript binding, backed by `litz_part_display_info`, `litz_part_display_version` and `litz_part_display_rgba`. It covers the SSD1306, SH1106, ILI9341, ST7735, e-paper, MAX7219 chain and Nokia 5110, so a host can show an e-paper panel the board-level `display()` never reported.
- `Board::part_framebuffer_version` reports the panel's change counter, so a host re-reads pixels only when the glass moved.

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

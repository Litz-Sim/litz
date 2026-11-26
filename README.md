# Litz

[![CI](https://github.com/Litz-Sim/litz/actions/workflows/ci.yml/badge.svg)](https://github.com/Litz-Sim/litz/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/litz.svg)](https://crates.io/crates/litz)
[![docs.rs](https://docs.rs/litz/badge.svg)](https://docs.rs/litz)
[![npm](https://img.shields.io/npm/v/@litz-sim/litz.svg)](https://www.npmjs.com/package/@litz-sim/litz)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](#license)

Litz is an open-source electronics and firmware simulator. A `diagram.json` file describes the parts and wiring, a manifest names the firmware image, and Litz boots that image on an emulated microcontroller, resolves pins, buses and analog nets against the parts on the board, and advances only when asked. Tests assert on serial output, part state and recorded waveforms. The core is Rust, runs natively and compiles to WebAssembly, loads parts from TOML manifests, and solves analog nets with an MNA solver rather than lookup tables.

## Feature list

| Area | What it does |
|---|---|
| Circuit | Parses and validates `diagram.json`, then flattens a development board into parts and nets. |
| Chips | Eight MCUs behind one `Chip` interface: ESP32, ESP32-S3, ESP32-C3, ESP32-C6, ESP32-P4, ESP32-S31, ATmega328P and RP2040. |
| Parts | Builtin catalog from TOML manifests: LEDs, displays, buttons, switches, sensors, EEPROM and flash, I/O expanders, real-time clocks, motor drivers, relays, IR parts and passives. |
| Buses | I2C by 7-bit address, SPI by chip select and UART by port, as transfers rather than pin wiggles, with device-side implementations in the parts. |
| Instruments | Probes on `<board>:<GPIO>` or `<part>:<pin>`, edges, frequency, duty, pulse widths and voltage range; `--scope` CSV, `--vcd` traces and `--decode` for UART, I2C and SPI. |
| Analog | MNA co-simulation with `--analog` or `[sim] analog = true`; an ADC reads the solved node voltage. |
| Wireless | A Wi-Fi router (DNS, TCP, UDP, TLS, WebSocket, MQTT), BLE beacons with optional GATT tables, nRF24L01/SX1276/AT86RF233 peers and IEEE 802.15.4 decoding. |
| Multi-board | Several boards per diagram, each with its own firmware, pins and buses; a wire between boards is one net. |
| CLI | `run`, `test`, `check`/`lint`, `build`, `serve`, `catalog`, `heap`, `board check`, `toolchain`/`doctor`, `editor` and `mcp`, with fixed exit codes, JSON/JUnit reports, GDB, screenshots and coverage. |
| Extensibility | Custom parts and boards as sandboxed WebAssembly components speaking `litz:part` and `litz:mcu`, with no WASI. |
| CI | `.github/workflows/ci.yml` and a composite action in `action.yml` that downloads the release binary and wraps `litz test`. |

## Rust

The simulator is a set of crates. `litz-engine` is the embeddable layer: boot a diagram already in memory, or load a project directory and evaluate its tests. `litz-board` is the layer below it, where a host owns the `Board` and drives a `Chip` itself.

```sh
cargo add litz-engine
```

```rust
use litz_engine::machine::{boot, BootOptions, Images};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let diagram = std::fs::read_to_string("diagram.json")?;
    let app = std::fs::read("firmware.bin")?;
    let mut session = boot(
        &diagram,
        Images { rom: &[], bootloader: &[], partition_table: &[], app: &app },
        &BootOptions::default(),
    )?;
    session.chip.run(100_000);
    print!("{}", String::from_utf8_lossy(&session.chip.take_console()));
    Ok(())
}
```

`session.board` is the `Board` the chip drives: `state(id)` reads a part, `press` and `set_control` drive it, `add_probe` and `scope` record nets. The [Rust reference](docs/rust.md) covers the project runner, custom `Part` and `Chip` implementations, WebAssembly components, probes and the analog solver.

## JavaScript

The npm package `@litz-sim/litz` (version 0.4.1) is built from `crates/litz-wasm` and `js/`. It loads one WebAssembly module, boots one chip per development board in a shared circuit, and wraps the module's exports in plain objects. There is no DOM code and no runtime dependency.

```sh
npm install @litz-sim/litz
```

```js
import { readFile } from "node:fs/promises";
import { loadLitz } from "@litz-sim/litz";

const litz = await loadLitz();
const session = litz.createSession({
  diagram: await readFile("diagram.json", "utf8"),
  app: new Uint8Array(await readFile("firmware.bin")),
});

session.advance(500);                       // simulated milliseconds
process.stdout.write(session.serial.take());
console.log(session.chip().name, session.stop, session.ms);
session.close();
```

| Surface | Calls |
|---|---|
| Serial | `session.serial.take()`, `session.serial.write(bytes)` |
| Parts and controls | `session.parts()`, `session.part(id)`, `session.press(id, pressed)`, `session.setControl(id, permille)`, `session.setNamedControl(id, name, value)` |
| Pin levels | `session.gpio(pin)`, `session.partPin(id, pin)` |
| Display and touch | `session.display()`, `session.touch(x, y, down)` |
| Probes, decoders, VCD | `session.probe.add(name, point)`, `session.scope()`, `session.decode.add(spec)`, `session.trace.vcd()` |
| Run state | `session.chip()`, `session.stop`, `session.fault`, `session.heap()`, `session.conflicts()` |

`createSession` also accepts `rom`, `boot`, `ptable`, `flashSize`, `mac`, `analog`, `manifests`, `parts` and `boards`. `run(steps)` advances by scheduling steps; `advance(ms)` advances by simulated time.

## CLI

Every version tag publishes a prebuilt `litz` binary for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-apple-darwin` and `x86_64-pc-windows-msvc`. Download `litz-<version>-<target>.tar.gz` from the [release](https://github.com/Litz-Sim/litz/releases) (a `.zip` on Windows), verify the archive against the `checksums.txt` published beside it, and put the executable on `PATH`. Each binary is compiled on a native runner for its own target rather than cross-compiled, and the x86_64 Linux build runs inside an Ubuntu 20.04 container so the glibc floor stays at 2.31.

`cargo install litz-cli` is the source path: it builds the same binary from crates.io, and `cargo install --path crates/litz-cli` builds it from a checkout. The commands below use `cargo run` because they are written for this repository.

```sh
cargo install litz-cli                 # installs the `litz` binary from crates.io
cargo install --path crates/litz-cli   # or build it from a checkout
cargo run -p litz-cli -- test examples/blinky   # LED on GPIO8 blinks, exit 0
cargo run -p litz-cli -- run examples/blinky    # stream serial output
cargo run -p litz-cli -- check examples/blinky  # validate the diagram only
```

The composite action in `action.yml` downloads the release binary for the tag it is pinned to and checks it against `checksums.txt`, so a workflow using it needs no Rust toolchain. On a platform the release does not cover, or a tag without a release, it prints one line and builds from source with `cargo run` instead.

| Command | What it does | Exit codes |
|---|---|---|
| `litz check [dir]` (`lint`) | validates `diagram.json` without running | 0 valid, 1 errors |
| `litz run [dir]` | boots the firmware and streams serial output | 0 all boards halted, 42 time limit, 1 fault |
| `litz test [dir]` | runs the `[[test]]` entries in `litz.toml` | 0 pass, 1 fail |
| `litz test [dir] --scenario scenario.yaml` | runs an automation scenario instead | 0 pass, 1 fail |
| `litz build [dir] [--dry-run] [--push [--board ID]]` | runs the project's `litz.build.toml` recipe; `--push` hands the image to a connected editor | 0 success, 1 failure |
| `litz connect-editor [dir] [--port N]` | opens the loopback link an editor page builds through (default port 4317) | runs until interrupted |
| `litz serve [dir] [--port N]` | serves a directory over HTTP on 127.0.0.1 (default `.`, port 8000) | runs until interrupted |
| `litz board check [dir]` | instantiates a board bundle and reports it | 0 valid, 1 invalid |
| `litz catalog` (`list`, `check`, `add`, `new`, `remove`, `--json`) | prints or manages parts and boards | 0 success, 1 failure |
| `litz heap FIRMWARE.elf` | reads heap symbols from an ELF | 0 success, 1 failure |
| `litz toolchain [--json]` (`doctor`) | reports installed firmware toolchains | 0 always |
| `litz editor --vscode [dir] [--force] [--gdb-port N]` | writes VS Code launch/tasks files | 0 success, 1 failure |
| `litz mcp` | serves the Model Context Protocol over stdio | runs until interrupted |

`--strict` (also `--warnings-as-errors`) turns validation warnings into failures. Other run and test flags include `--json`, `--junit FILE`, `--build`, `--update-lock`, `--vcd FILE`, `--pcap FILE`, `--wifi SPEC`, `--probe [NAME=]POINT`, `--scope FILE`, `--decode SPEC`, `--analog`, `--coverage`, `--interactive`, `--heap`, `--gdb [BOARD@]PORT` and `--screenshot [BOARD@]FILE`. An unknown option exits 2. `check` needs only `diagram.json`; `run` and `test` also need the firmware named in `litz.toml`, and ESP-IDF images need a mask ROM.

## Chips

The wasm module boots every builtin chip named by the diagram's board:

- ESP32-C3, ESP32-C6, ESP32-P4 and ESP32-S31 (RISC-V).
- ATmega328P (Arduino Uno, Nano, Pro Mini).
- RP2040 (Raspberry Pi Pico).

The classic ESP32 and ESP32-S3 are Xtensa. Their simulator executes guest blocks through a host JIT, which the wasm build does not carry; `createSession` reports that instead of booting them. The native CLI builds both Xtensa parts.

## Custom parts and boards

A custom part or board from the catalog is a WebAssembly Component. Transpile it once with jco, then pass factories and manifests to `createSession`:

```sh
npx jco transpile component.wasm -o out --instantiation sync
```

```js
import { readFileSync } from "node:fs";
import { loadLitz, partFromJco, partImports, chipFromJco, chipImports } from "@litz-sim/litz";
import { instantiate as instantiatePart } from "./out/component.js";
import { instantiate as instantiateChip } from "./out/chip.js";

const partCore = new WebAssembly.Module(readFileSync("./out/component.core.wasm"));
const chipCore = new WebAssembly.Module(readFileSync("./out/chip.core.wasm"));

const session = litz.createSession({
  diagram,
  app,
  manifests: [partToml, { toml: boardToml, schematic: onboardJson }],
  parts: { "my-part.wasm": () => partFromJco(instantiatePart(() => partCore, partImports())) },
  boards: { "my-chip.wasm": (host) => chipFromJco(instantiateChip(() => chipCore, chipImports(host))) },
});
```

The `instantiate` functions and core modules are the jco output for each component. `parts` and `boards` are keyed by the module name in the part or board manifest. Both `litz:mcu@0.1.0` and `litz:mcu@0.2.0` chips are bridged in full: the 0.2 analog, I2C, SPI, UART, Wi-Fi, TCP/UDP, BLE and radio calls are answered by the same `HostState` the native runner uses, through the module's `litz_guest_call` export.

## Repository layout

| Path | Contents |
|---|---|
| `crates/` | Rust workspace: engine, CLI, MCP server, chip adapters, board runtime, catalog, analog solver, decoders and the wasm ABI |
| `js/` | the `@litz-sim/litz` package: ES module wrapper, type declarations, built wasm and tests |
| `examples/` | complete projects with `diagram.json`, `litz.toml` and firmware, including `blinky`, `button`, `pot`, `rv32`, `uno-blink` and the Pico, MicroPython and CircuitPython projects |
| `docs/` | reference documentation for formats, architecture, parts, builds and MCP |
| `crates/litz-cli/templates/` | board and part scaffolds used by `litz catalog new` |
| `tools/` | scripts that fetch pinned MicroPython and CircuitPython images |
| `action.yml`, `.github/workflows/ci.yml` | CI entry points |

The dependency direction is one way: chip adapters depend on `litz-board`, while the board depends only on `litz-core`, `litz-diagram` and `litz-catalog`.

## Documentation

- [Getting started](docs/getting-started.md)
- [CLI reference](docs/cli.md)
- [Rust reference](docs/rust.md)
- [JavaScript reference](docs/javascript.md)
- [Architecture](docs/architecture.md)
- [File formats](docs/formats.md)
- [Parts and boards](docs/parts.md)
- [Writing a chip](docs/chips.md)
- [Building firmware](docs/build.md)
- [MCP server](docs/mcp.md)
- [Examples](examples/README.md)
- [Changelog](CHANGELOG.md)
- [Contributing](CONTRIBUTING.md)
- [Security policy](SECURITY.md)

## License

Apache-2.0. The license is declared once in `Cargo.toml` under `[workspace.package]` and inherited by every crate. Every crate in the workspace and the npm package carry the same version, also declared there; the `0.x` line makes no compatibility promise between minor versions. The esp32sim fork under `vendor/` keeps its upstream MIT license and ships that license file with each vendored crate.

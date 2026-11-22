# Getting started

Litz is a deterministic electronics and firmware simulator. A `diagram.json` file describes the parts and the wiring, a `litz.toml` names the firmware image and the assertions, and the `litz` command boots that image on an emulated microcontroller and advances only when asked. This page installs the tool, runs the smallest project in the repository, explains how a project is laid out, and opens the same project from JavaScript. The complete command surface is in the [CLI reference](cli.md).

## Install

The native runner is a Rust binary. Install it from a checkout of this repository:

```sh
cargo install --path crates/litz-cli
```

That puts `litz` on `PATH`. Without installing it, every command below also runs through Cargo, with the binary crate named explicitly:

```sh
cargo run -p litz-cli -- test examples/blinky
```

The JavaScript path is the npm package `@litz-sim/litz`, built from `crates/litz-wasm` and `js/`. It loads one WebAssembly module and opens a session for one board; it does not need a native toolchain.

```sh
npm install @litz-sim/litz
```

## A first run: blinky

`examples/blinky` is a complete project with no downloaded assets. Its firmware is 272 bytes of hand-assembled RV32IMC that configures GPIO8, toggles it, and marks each edge on UART0. From the repository root:

```sh
cargo run -p litz-cli -- test examples/blinky
```

The command runs the `[[test]]` entries in `examples/blinky/litz.toml`, expects the string `litz: blinky` on the console and the LED part `led1` to be on, and exits 0. To watch the serial output:

```sh
cargo run -p litz-cli -- run examples/blinky
```

`run` streams the console until the default simulated time limit of ten seconds. The blinky firmware loops forever, so this run ends at the time limit and exits 42 rather than 0. That is the documented result for a bounded run that was cut short, not an error in the project. To validate the diagram without booting anything:

```sh
cargo run -p litz-cli -- check examples/blinky
```

`check` reads `diagram.json`, resolves every `type` against the catalog, checks wiring and attributes, and exits 0 when the diagram is valid. It does not need the firmware file.

## How a project is laid out

A project is a directory. `diagram.json` is required; the other files appear when a command needs them.

| File | Purpose |
|---|---|
| `diagram.json` | The parts and the connections between them. |
| `litz.toml` | Which firmware to boot and, for `test`, the assertions to run. |
| `firmware/` | Images named by `litz.toml`; the directory name is a convention. |
| `parts/` | Optional project catalog of extra part manifests. |
| `boards/` | Optional project catalog of extra board bundles. |
| `litz.build.toml` | Optional build recipe run by `litz build` or `--build`. |
| `litz.lock` | Catalog and firmware hashes the runner writes for reproducibility. |
| `scenario.yaml` | Optional automation script for `--scenario`. |

The blinky circuit places an ESP32-C3 development board, a resistor and an LED, and wires the resistor between GPIO8 and the LED anode. Its `diagram.json` is:

```json
{
  "version": 1,
  "author": "Litz",
  "editor": "litz",
  "parts": [
    { "id": "esp",  "type": "board-esp32-c3-devkitm-1", "attrs": {} },
    { "id": "r1",   "type": "litz-resistor", "attrs": { "value": "220" } },
    { "id": "led1", "type": "litz-led", "attrs": { "color": "green" } }
  ],
  "connections": [
    ["esp:GPIO8", "r1:1", "green", []],
    ["r1:2", "led1:A", "green", []],
    ["led1:C", "esp:GND.1", "black", []],
    ["esp:TX", "$serialMonitor:RX", "", []]
  ]
}
```

A connection is a four-element array: source pin, destination pin, wire colour and route. Only the first two elements affect behaviour. The pin reference is `<part>:<pin>`; `$serialMonitor:RX` is the host serial endpoint, and `$gnd`, `$v33`, `$v5` and `$vin` are global rails that need no `parts` entry.

`litz.toml` names the image and the test. The blinky manifest is:

```toml
[project]
diagram = "diagram.json"

[sim]
firmware = "firmware/blinky.bin"
flash_size = "4MB"

[[test]]
name = "the LED on GPIO8 blinks"
timeout = "2s"
expect_state = "led1:on"
expect = "litz: blinky"
```

`[project] diagram` defaults to `diagram.json`. `[sim]` describes the one board when the diagram has one; a diagram with several development boards gives each a `[[board]]` entry with its own `firmware`. `[[test]]` is repeatable and supports `timeout`, `expect`, `fail`, `expect_state`, `state_at`, `state_stays` and repeatable `[[test.input]]` steps. Paths in `litz.toml` are relative to the project directory.

## Run, test and check

| Command | What it does | Exit codes |
|---|---|---|
| `litz run [directory]` | Boots the firmware and streams its serial output. | 0 when every board halts, 42 at the time limit, 1 on a fault or trap. |
| `litz test [directory]` | Runs the `[[test]]` entries and asserts on console text and part state. | 0 when all pass, 1 when any fails. |
| `litz check [directory]` (`lint`) | Validates `diagram.json` against the catalog without booting. | 0 valid, 1 errors. |

The directory defaults to the current one. `check` and `lint` are the same command. `--strict` (also `--warnings-as-errors`) promotes validation warnings to errors; `--json` prints one machine-readable record instead of the transcript, and `--junit FILE` also writes a JUnit XML report. A run also accepts `--scenario FILE` to execute a YAML automation script instead of the `[[test]]` entries, and `--build` to run `litz.build.toml` first. These options are covered in the [CLI reference](cli.md).

A project whose firmware is built locally can keep its commands in `litz.build.toml`, an ordered list of shell steps. `litz build [directory]` runs the recipe, and `litz test [directory] --build` runs it before the tests. The recipe format is described in [Building firmware](build.md). `litz serve [directory]` serves a directory over HTTP on `127.0.0.1:8000` for a page that loads the WebAssembly build.

## Using the Rust library

The crates are the simulator, and `litz-engine` is the layer a host embeds. `cargo add litz-engine` and call `machine::boot` with a diagram and its images, or `runner::execute` with a project directory to get the same pass/fail result the CLI prints. [The Rust reference](rust.md) covers the boot loop, reading part state, the project runner, probes, custom `Part` and `Chip` implementations and WebAssembly components.

## Using the JavaScript library

The npm package wraps the WebAssembly module in plain objects. Load the module once with `loadLitz`, open a session with `createSession`, call `advance` with a number of simulated milliseconds, and drain the console with `serial.take`. From a checkout of this repository:

```js
import { readFile } from "node:fs/promises";
import { loadLitz } from "@litz-sim/litz";

const litz = await loadLitz();
const session = litz.createSession({
  diagram: await readFile("examples/blinky/diagram.json", "utf8"),
  app: new Uint8Array(await readFile("examples/blinky/firmware/blinky.bin")),
});

session.advance(500);                       // simulated milliseconds
process.stdout.write(session.serial.take());
console.log(session.chip().name, session.stop, session.ms);
session.close();
```

`createSession` also accepts `rom`, `boot`, `ptable`, `flashSize`, `mac`, `analog`, `manifests`, `parts` and `boards`. `advance(ms)` advances by simulated time and returns the simulated milliseconds reached; `run(steps)` advances by scheduling steps instead. A session runs one board. The WebAssembly build carries the ESP32-C3, ESP32-C6, ESP32-P4 and ESP32-S31, the ATmega328P and the RP2040; the classic ESP32 and ESP32-S3 are Xtensa and are not compiled into the WebAssembly module.

Beyond serial, the session exposes `parts()`, `part(id)`, `press(id, pressed)`, `setControl(id, permille)`, `gpio(pin)`, `partPin(id, pin)`, `display()`, `touch(x, y, down)`, `probe.add(name, point)`, `scope()`, `decode.add(spec)`, `trace.vcd()`, `heap()` and `conflicts()`. The type declarations in `js/index.d.ts` list every call, and [File formats](formats.md) documents the simulation options the constructor takes.

## Where to go next

- [CLI reference](cli.md) documents every command and option.
- [File formats](formats.md) covers `diagram.json`, `litz.toml`, `litz.lock`, probes, decoders and the analog solver.
- [Parts and boards](parts.md) describes catalog manifests and installing a bundle.
- [Building firmware](build.md) describes `litz.build.toml`.
- [Architecture](architecture.md) explains how the crates fit together.
- [MCP server](mcp.md) exposes the same runner over the Model Context Protocol.
- [Examples](../examples/README.md) lists the other projects, including `button`, `pot`, `rv32`, `two-boards`, `uno-blink`, the Pico projects and the MicroPython and CircuitPython projects.

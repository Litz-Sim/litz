# Litz

Litz is an open-source, modular electronics simulator that runs real firmware. You describe a circuit in a `diagram.json`, point it at a firmware image, and Litz boots that image on an emulated microcontroller, resolves pins, buses and analog nodes against the parts on the board, and lets a test assert on serial output, part state and waveforms. It is built to run as a CI step first and in a browser second: the same core compiles natively and to WebAssembly. The engine is pure Rust — no JavaScript emulator cores, no external solver — and the part library is loaded from TOML manifests, so the catalog grows without recompiling the runner. A Rust MNA solver covers the analog side (DC operating point and a backward-Euler transient) rather than approximating it with lookup tables.

## Status

The following works today and can be checked from the repository.

- Parsing a `diagram.json`, validating it against a catalog, and flattening a development board into a flat part list and nets. `cargo test --workspace` covers this and needs no downloaded assets.
- Eight microcontrollers behind one `Chip` interface. Six ESP32 variants share a generic `EspChip<S>` adapter over esp32sim — classic ESP32 (Xtensa LX6), ESP32-S3, ESP32-C3, ESP32-C6, ESP32-P4 and ESP32-S31. The ATmega328P is an in-house AVR8 core (`crates/litz-avr`) with cycles, 26 interrupt vectors, Timer0/1/2 PWM, ADC, SPI, TWI, EEPROM and USART, and the RP2040 is an in-house ARMv6-M Cortex-M0+ core (`crates/litz-rp`) with exceptions and `SysTick`, the SIO block, the microsecond timer and its four alarms, the PL011 UARTs, SPI, I2C, PWM, ADC, DMA and the QSPI/XIP handshake. It boots the way the chip does — the ROM copies a 256-byte stage 2 into SRAM and hands off to the application's vector table at `0x1000_0100` — so `arm-none-eabi-gcc -mcpu=cortex-m0plus` output runs as compiled, SysTick handlers included. Each has a board manifest; boot images this repository builds itself run without a mask ROM, ESP-IDF images need one.
- Booting an unmodified ESP-IDF `hello_world` for the ESP32-C3 from the real mask ROM through the `esp32sim` backend, and asserting on its UART output. That test is ignored by default and needs external files.
- MicroPython and CircuitPython, not just ESP-IDF: pinned official images for ESP32, ESP32-S3, ESP32-C3 and ESP32-C6 drive a script over the REPL, and the `examples/micropython-*` and `examples/mp-*` projects assert on what the board does. `examples/circuitpython` boots the official ESP32-C3 build through the mask ROM, waits out `code.py` and pastes the project's own file.
- A builtin part catalog and board list compiled in from TOML manifests, resolved on nets with strong and weak drives, a fixed point per change, and faults reported instead of hidden. It covers LEDs and displays (SSD1306, SH1106, ST7735, MAX7219, character LCDs, seven-segment), buttons and switches, sensors (DHT22, BMP280, BME680, MPU6050, ADXL345, QMC5883L, LM75, TMP102, SGP30, VL53L1X, LDR, NTC, flex and force sensors, HC-SR04, PIR, Hall, TCRT5000 line sensor), memory and I/O expanders (24-series EEPROM, W25Q32, PCF8574, MCP23017, TCA9548A, 74HC165/138/139/238, MCP3008), real-time clocks, motor drivers, relays, IR parts and passives.
- Buses, not just pins: I2C (7-bit address), SPI (by chip select) and UART (by port) route through the chip and reach parts that implement the device side, so a bus transaction is a real transfer rather than a set of pin wiggles.
- Instruments that read the circuit. A probe names one point — `<board>:<GPIO>` or `<part>:<pin>` — and records every transition there, with the solved voltage added when analog co-simulation is on. Bus traffic is observable too: a UART write, an I2C transaction or an SPI transfer is reconstructed as pin-level edges on the wires the diagram says carry it, so a probe on SDA/SCL or TX shows the frame instead of a flat line. A run reports frequency, duty, edge count, pulse widths and voltage range per channel, and `--scope FILE` writes the record as a CSV. A `litz-signal-generator` part drives a circuit with no firmware at all: square, sine, triangle or sawtooth on simulated time. A `[[decode]]` entry or `--decode` reads UART, I2C or SPI off the same probe record, so a protocol frame is reported without anything touching the circuit.
- Radios that belong to the circuit. A `litz-wifi-router` part puts a deterministic network behind the access point (DNS, TCP, UDP, TLS 1.3 with a shipped trust store, WebSocket and an MQTT 3.1.1/5.0 broker — no host sockets), a `litz-ble-beacon` part puts a scannable advertiser in range and may publish a GATT table a custom chip connects to, reads, writes and subscribes to, and `litz-nrf24l01`, `litz-sx1276` and `litz-at86rf233` parts exchange packets on one shared medium that schedules each frame by its own airtime and destroys overlapping ones. IEEE 802.15.4 frames (PAN/address filtering, FCS, broadcast) are decoded, so Zigbee/Thread-shaped traffic is testable at the MAC layer.
- More than one MCU on a diagram. Every board instance is expanded with its own chip pins and boots its own image (`[[board]]` in `litz.toml`); a wire between two boards is one net, so one chip's output is the other's input. `examples/two-boards` lights a LED on the second board from a pin the first one holds. Board 0 keeps the original `Chip(pin)` endpoints and the single-board API is unchanged, and each board has its own `i2c0`/`spi0`/`uart0` fabric. A test can address one board's console (`expect = "b@..."`) and schedule input on one board (`board = "b"`).
- Observability for a failing run: `--screenshot` writes the board's display as a PNG, a scenario's `take-screenshot` writes one display part by id and compares it pixel by pixel, `--heap` follows the firmware's free heap on an interval with an optional minimum and CSV, `--gdb` serves the GDB remote protocol against a booted RISC-V chip, and `[sim] elf` supplies the firmware's symbol table for the Bluetooth controller, named addresses and the debugger.
- Analog co-simulation in Rust. `litz-analog` is a dense-LU MNA solver with damped Newton–Raphson, junction limiting and a backward-Euler transient; the board maps nets to nodes and GPIOs to Thévenin sources, so an ADC reads the solved voltage. Off by default, enabled with `--analog` or `[sim] analog = true`.
- Boot images this repository builds itself: `litz-fixture` assembles a few hundred bytes of RV32IMC, so a circuit with a blinking LED and a working button runs with no ESP-IDF, no RISC-V toolchain and no mask ROM.
- The `litz` command with `run`, `test`, `check` (or `lint`), `serve` and `catalog`, fixed exit codes (0 pass, 1 fail, 42 time limit), part-state assertions (`expect_state = "led1:on"`) including timed ones (`state_at`, `state_stays`), host input scheduled from the test file, `--json` / `--junit` reports, `--vcd` waveform capture, `--analog`, `--coverage`, `--interactive`, `--wifi`, `--break`, `--heap`/`--heap-interval`/`--heap-min-free`/`--heap-csv` and `--scenario`.
- `litz build`, which runs a project's own `litz.build.toml` recipe — ordered shell steps with workdir, env and timeout, like a CI job — and `--build` on `test`/`run` to build first; see [`docs/build.md`](docs/build.md).
- `litz mcp`, a Rust-native Model Context Protocol server over stdio that exposes check, test, run, circuit editing, code generation and diagram import to an editor; see [`docs/mcp.md`](docs/mcp.md).
- A current comparison with Wokwi, SimulIDE, Renode, QEMU and PICSimLab, including the gaps this engine still has: [`docs/comparison.md`](docs/comparison.md).
- A browser build of the same core: `web/` loads a wasm module, draws the schematic, streams the serial monitor, lets a click press a pushbutton or turn a knob, and exports the recorded nets as a VCD for PulseView or GTKWave.
- Parts and chips that are not compiled in. A `part.toml` can name a WebAssembly component that speaks the `litz:part` ABI (I2C/SPI/UART slave callbacks, timers, a framebuffer), and a `board.toml` can name one that speaks `litz:mcu`. Both are sandboxed by the host with no WASI. `examples/rv32` runs `blinky.bin` on a reference RV32IMC chip written as a component (`crates/litz-chip-rv32`).
- Packet capture. `--pcap FILE` records every TCP, UDP and DNS exchange the simulated router answers as a nanosecond-resolution pcap that Wireshark opens; the frames are synthesised from the router's decisions, so an ESP32, a C6 and a wasm MCU produce the same file for the same firmware.
- Custom chips with the full peripheral set. `litz:mcu@0.2.0` gives a community chip analog reads, I2C/SPI/UART transactions and the circuit's Wi-Fi (scan, join, DNS, TCP/UDP, MQTT) and BLE services (scan, advertise, connect, GATT read/write/notify), so a board can be a sensor node or a radio device rather than a CPU wiggling pins. `litz catalog new board <id>` writes a manifest, schematic, template README and a buildable 0.2 guest under `boards/<id>/`; `litz board check <dir>` instantiates the component and reports what it is before a project depends on it. A board whose guest ships its own program may omit `firmware` entirely.

CI runs the same commands from `.github/workflows/ci.yml`, and `action.yml` wraps `litz test` as a composite action with an optional JUnit report.

The workspace tests:

```sh
cargo test --workspace
```

The commands and their exit codes:

| Command | Validates the diagram | Runs firmware | Exit codes |
|---|---|---|---|
| `litz check [dir]` | yes | no | 0 valid, 1 errors |
| `litz run [dir]` | yes | yes | 0 when every board halts, 42 at the simulated time limit, 1 on a fault |
| `litz test [dir]` | yes | yes | 0 all tests passed, 1 any failed |
| `litz build [dir]` | no | no | 0 recipe succeeded, 1 a step or the firmware output failed |
| `litz board check [dir]` | no | no | 0 bundle valid, 1 manifest, schematic or module failed |
| `litz serve [dir]` | n/a | no | serves `web/` on 127.0.0.1:8000 |

```sh
cargo run -p litz-cli -- check examples/blinky
cargo run -p litz-cli -- run examples/hello-c3
cargo run -p litz-cli -- test examples/hello-c3
```

`check` needs only a `diagram.json`; `run` and `test` also need the firmware named in `litz.toml` and, for ESP-IDF images, the mask ROM. `lint` is an alias for `check`, `--strict` (also `--warnings-as-errors`) turns validation warnings into failures, and an unknown option exits 2.

The ROM-backed ESP32-C3 boot test, and its determinism companion `c3_run_is_deterministic`, are `#[ignore]`d and need two environment variables:

```sh
LITZ_ROM_DIR=build/roms \
LITZ_C3_ASSETS=build/esp32sim/web/wasm/fw/public \
cargo test -p litz-esp --features esp32c3 --test c3_boot -- --ignored
```

`LITZ_ROM_DIR` names a directory holding `esp32c3_rev3_rom.elf` (Espressif's ROM, Apache-2.0, not vendored here). `LITZ_C3_ASSETS` names a directory holding `c3-hello-bootloader.bin`, `c3-hello-ptable.bin` and `c3-hello_world.bin`.

The examples are complete projects under `examples/`:

```sh
cargo run -p litz-cli -- test examples/blinky    # needs nothing downloaded
cargo run -p litz-cli -- test examples/button    # presses the button from the test file
cargo run -p litz-cli -- test examples/rv32      # the same firmware on a wasm chip
cargo run -p litz-cli -- test examples/pot       # turn a knob, see the LED follow
cargo run -p litz-cli -- test examples/uno-blink # ATmega328P, hand-assembled HEX
cargo run -p litz-cli -- test examples/hello-c3  # needs LITZ_ROM_DIR and an ESP-IDF build
cargo run -p litz-cli -- test examples/micropython-esp32  # pinned MicroPython image
cargo run -p litz-cli -- test examples/circuitpython      # pinned CircuitPython image, ESP32-C3
```

## Browser

```sh
./web/build.sh                     # wasm module and the two demo firmware images
cargo run -p litz-cli -- serve     # http://127.0.0.1:8000/
```

The page boots the **Blink** demo on its own; the **Button** demo lights the LED while you hold the pushbutton down. Both run the same core as the CLI, compiled to `wasm32-unknown-unknown`, through a plain C ABI (`crates/litz-wasm/src/lib.rs`) rather than wasm-bindgen, so the module needs no JavaScript glue step. `web/README.md` has the details, and `web/build.sh` regenerates everything the page fetches.

The workbench beside it (`web/workbench/`) is the editor: `litz catalog --json` feeds its part picker, it places and wires parts on a grid, validates ids and pins client-side, saves a `.litz` project, and imports and exports `diagram.json`. `cargo run -p litz-cli -- serve web/workbench` serves it on its own.

`js/` packages the same module for a host that wants no page at all: an ES module with no dependencies and no DOM code, packaged for npm as `@litz-sim/litz`. It loads the wasm once, opens sessions, and exposes the console, part state, the display, probes and decoders.

```sh
cd js && npm run build && npm test   # build the module, run the package's tests
```

## How it is put together

| Crate | Owns | Must not know about |
|---|---|---|
| `litz-core` | `Tick`, `Level`, `Drive`, the `Bus`/`Mcu`/`Part` traits, `PartState` | any chip, catalog entry or file format; it depends on nothing |
| `litz-diagram` | `diagram.json` parsing, structural and catalog-backed validation, the raw netlist | simulation, catalog manifests, chips |
| `litz-catalog` | `part.toml` and `board.toml` parsing, the `Registry` | running a simulation, diagram geometry, chips |
| `litz-mcu` | `MemoryRegion`/`MemoryMap`, `Clock`, `Core`, `InterruptController` | a concrete chip, a board, a part |
| `litz-board` | flattening, net resolution, the builtin part engines, the bus fabric, the part scheduler, the `Chip` trait | any chip backend; it talks to pins, never to an MCU |
| `litz-avr` | the ATmega328P AVR8 core, its timers/ADC/SPI/TWI/USART and the board bridge | the CLI, catalog manifests, file formats |
| `litz-analog` | the MNA solver: DC operating point, damped Newton–Raphson, backward-Euler transient | pins, manifests, chips |
| `litz-esp` | the ESP32 backend over `esp32sim`, chip memory maps, the `BoardModel` adapter | presentation, the CLI, the file formats |
| `litz-mcu-wasm`, `litz-mcu-wit` | the sandboxed host loader for a wasm MCU and the `litz:mcu` contract it speaks | which chip it is; the concrete adapter |
| `litz-part-wasm`, `litz-part-wit` | the `litz:part` ABI and the host that dispatches to a component part | the builtin part engines |
| `litz-editor` | in-memory circuit edits, undo/redo, wire routing | files, the CLI, the simulation |
| `litz-project` | the `.litz` project file, its files section and validation | how anything is simulated |
| `litz-mcp` | the Model Context Protocol server, tool schema and path confinement | the CLI's argument parsing |
| `litz-decode` | UART/I2C/SPI decoders over probe waveforms | the board, chips, files |
| `litz-engine` | project loading, boot, the run loop, assertions, probes and protocol decoding | the CLI's arguments, files and presentation |
| `litz-cli` | the `litz run`/`test`/`check` command, argument parsing, report formats and the static server; a host over `litz-engine` | nothing above it; it is the top of the stack |
| `litz-wasm` | the WebAssembly ABI surface (a plain C ABI, no wasm-bindgen) over `litz-engine` and the chip adapters | presentation, the CLI |
| `litz-fixture` | hand-assembled RV32IMC programs and the ESP image builder they are wrapped in | the simulation; it only produces bytes |
| `litz-chip-rv32` | a reference RV32IMC chip built as a wasm component | the host that loads it |
| `litz` | the umbrella crate that re-exports the family seams | runtime logic of its own |

The dependency runs one way, from a chip adapter into the board: `litz-board` never depends on a chip — its dependencies are only `litz-core`, `litz-diagram` and `litz-catalog` — and it drives an MCU through `set_chip_output` and `take_chip_inputs`. Three adapters implement the same `Chip` trait: `litz-esp` over esp32sim, `litz-avr` in-house, and `litz-mcu-wasm` for a component. Each optionally depends back on `litz-board` because its adapter holds a board; nothing in the board reaches for any of them.

## File formats

Five files describe a simulation, and `docs/formats.md` is the reference: `diagram.json` (what is placed and what is wired to what), `part.toml` (what a leaf component is), `board.toml` plus its onboard schematic (what a development board is), `litz.toml` (which firmware to run and what to assert), and `litz.lock` (which catalog versions and image hashes produced a result; the runner checks it and `--update-lock` rewrites it). A sixth, the `.litz` project file, wraps a circuit together with the source files that belong to it and the settings a runner needs, so one frontend can write what another reads. Circuits are JSON; manifests are TOML because people read and edit them. Presentation never changes behaviour: coordinates, rotation, `hide`, wire colour and wire routing are parsed so a file round-trips, then dropped.

## How a simulation runs

1. `litz-engine` loads `litz.toml` through `Config::load_project` in `crates/litz-engine/src/config.rs`: the diagram path, the `[sim]` section and the `[[test]]` entries. In `check` mode a missing or invalid manifest is ignored and defaults are used, so `check` needs only a diagram.
2. `Circuit::from_json` in `crates/litz-diagram/src/lib.rs` parses the diagram into parts, connections and presentation fields. Unknown keys are kept so a newer editor's file still loads.
3. `Circuit::validate` checks structure and the catalog: known types, declared pins, pin indices, compatible pin kinds, rail shorts and attribute types. Errors — or warnings under `--strict` — are printed as `error[code]: message` and the run stops with exit 1. `check` stops here.
4. The runner builds a `Registry` from the compiled-in catalog plus any `parts/` and `boards/` directories next to the diagram (`crates/litz-engine/src/runner.rs`).
5. `FlatBoard::build` in `crates/litz-board/src/flatten.rs` expands the single development board: the onboard schematic's `$chip` becomes chip pins, a header pin with `gpio = n` becomes chip pin `n`, a fixed header pin becomes a global rail, child parts get an `<instance>/` prefix, and top-level parts are added. Every wire is unioned into nets. No levels are resolved yet.
6. `Board::new` in `crates/litz-board/src/runtime.rs` instantiates the builtin part engines and resolves every net once, producing the chip's initial input levels.
7. The runner picks an adapter from the board manifest: `litz-esp` for an ESP32 `chip`, `litz-avr` for `atmega328p`, or `litz-mcu-wasm` for a `[board.simulator]` component, attaches the board, and asks the adapter for its flash offsets. A bootloader and partition table, when present, are written at those offsets; otherwise the bare application is written and started directly.
8. The run loop checks for existing conflicts, advances `mcu.run(1_000_000)` scheduling steps, drains the console into the serial buffer, and stops on a conflict, a `fail` string, every `expect` string, or the simulated timeout. Time-scheduled parts are advanced at their own timestamps, so an edge keeps the instant it was scheduled.
9. The serial output is printed, followed by any `litz:` fault or missing-expectation notes. `run` exits 0 when every board halts, 42 at the time limit, or 1 on a fault; `test` exits 0 only when there are no conflicts, no `fail` hit and no missing `expect`, otherwise 1.

## Getting started

Every directory under `examples/` is a complete project: a `diagram.json`, a `litz.toml`, and the firmware or script each manifest names. `blinky`, `button`, `echo`, `pot`, `rv32` and `uno-blink` carry firmware that runs on a clean checkout; `examples/README.md` describes each.

- `examples/hello-c3` expects `firmware/hello_world.bin`, `firmware/bootloader.bin` and `firmware/partition-table.bin` from an ESP-IDF build. The generated ones build themselves with `litz-fixture`; regenerate with `cargo run -p litz-fixture -- blinky examples/blinky/firmware/blinky.bin` and `cargo run -p litz-fixture -- button examples/button/firmware/button.bin`.
- An ESP-IDF image boots from the second-stage bootloader and needs the mask ROM. Point `LITZ_ROM_DIR` at a directory holding `esp32c3_rev3_rom.elf`, or set `[sim] rom` in `litz.toml`.
- MicroPython and CircuitPython are pinned and fetched, not vendored: `./tools/fetch-micropython.sh` and `./tools/fetch-circuitpython.sh` download the images and the four ROMs into `build/`, and the `micropython-*` and `circuitpython` examples point there.
- A self-made application image needs no ROM: the runner writes it at the chip's app offset and jumps straight to it. `crates/litz-esp/tests/boot_app.rs` builds such an image in code and boots it with no ESP-IDF and no ROM.

```sh
export LITZ_ROM_DIR=/path/to/roms
cargo run -p litz-cli -- test examples/hello-c3
```

## Licence

MIT OR Apache-2.0. The licence is declared once in `Cargo.toml` under `[workspace.package]` and inherited by every crate. Every crate carries `version = "0.0.1"` from the same place; treat that version as a placeholder rather than a compatibility promise.
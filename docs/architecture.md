# Architecture

This is the document to read before touching code. It covers how the crates depend on each other, what the two circuit models are, how nets resolve, how time works, what keeps a run deterministic, and where the seams for new backends, rendering, WebAssembly and parts are.

## Dependency direction

Dependencies point one way. A crate may use the crates below it in this table; none depends on a crate above it.

| Crate | Layer | Depends on |
|---|---|---|
| `litz-core` | foundation | nothing |
| `litz-radio` | shared medium | nothing |
| `litz-ble` | BLE advertiser model | nothing |
| `litz-ieee802154` | 802.15.4 frames | nothing |
| `litz-fixture` | test images | nothing |
| `litz-analog` | analog solver | `thiserror` |
| `litz-diagram` | file format | `serde`, `serde_json`, `thiserror` |
| `litz-catalog` | file format | `litz-diagram`, `serde`, `thiserror`, `toml` |
| `litz-mcu` | chip building blocks | `litz-core` |
| `litz-net` | network services | `rustls`, `rustls-rustcrypto`, `x25519-dalek`; `getrandom` on wasm32 |
| `litz-board` | board runtime | `litz-analog`, `litz-ble`, `litz-catalog`, `litz-core`, `litz-diagram`, `litz-ieee802154`, `litz-net`, `litz-radio`, `serde_json`, `thiserror`; `litz-part-wasm` only off wasm32 |
| `litz-avr` | chip backend (in-house AVR8) | `litz-core`, `litz-board` |
| `litz-rp` | chip backend (in-house RP2040) | `litz-core`, `litz-board`, `litz-mcu` |
| `litz-mcu-wit` | contract | nothing (WIT only) |
| `litz-part-wit` | contract | nothing (WIT only) |
| `litz-mcu-wasm` | chip backend | `litz-ble`, `litz-board`, `litz-core`, `litz-mcu-wit`, `litz-net`, `serde`, `serde_json`, `thiserror`; `wasmtime` only off wasm32 |
| `litz-part-wasm` | part backend | `litz-core`, `litz-part-wit`, `wasmtime` |
| `litz-chip-rv32` | reference chip (guest) | `wit-bindgen` |
| `litz-part-sdk` | guest SDK | `wit-bindgen` |
| `litz-part-74hc595` | reference part (guest, 0.1.0) | `wit-bindgen` |
| `litz-part-eeprom` | reference part (guest, 0.2.0) | `litz-part-sdk` |
| `litz-part-display` | reference part (guest, 0.2.0) | `litz-part-sdk` |
| `litz-part-runaway` | sandbox fixture (guest) | `litz-part-sdk` |
| `litz-esp` | chip backend | `litz-core`, `litz-mcu`, `gdbstub`; `litz-board`, `litz-ble`, `litz-net` under any ESP feature |
| `litz-editor` | document editing | `litz-catalog`, `litz-diagram`, `serde_json`, `thiserror` |
| `litz-project` | project format | `litz-diagram`, `serde`, `serde_json`, `thiserror` |
| `litz-mcp` | protocol server | `litz-catalog`, `litz-core`, `litz-diagram`, `serde`, `serde_json`, `toml` |
| `litz-decode` | protocol decoding | `litz-core` |
| `litz-engine` | engine | `litz-avr`, `litz-ble`, `litz-board`, `litz-catalog`, `litz-core`, `litz-decode`, `litz-diagram`, `litz-net`, `litz-rp`, `png`, `serde`, `serde_yaml_ng`, `sha2`, `toml`; `litz-esp` with all six ESP features natively and the four RISC-V parts on wasm; `litz-mcu-wasm` only off wasm32 |
| `litz-wasm` | edge | `litz-board`, `litz-core`, `litz-diagram`, `litz-engine`, `litz-esp`, `litz-mcu`, `serde`, `serde_json`; `litz-net` only for the `tls-probe` feature, `litz-mcu-wasm` only on wasm32 |
| `litz` | umbrella | `litz-core`, `litz-mcu`, `litz-esp`, `litz-diagram`, `litz-catalog` |
| `litz-cli` | application | `litz-catalog`, `litz-diagram`, `litz-engine`, `litz-mcp`, `serde`, `serde_json`, `toml` |

The engine is the embeddable layer: it loads a project, boots the board, runs the loop and evaluates assertions, probes and decoders, and it knows nothing about arguments or output formats. The CLI and the `litz-wasm` module are hosts over it that own arguments, files and presentation. The file-based runner and the in-memory `litz_engine::machine::boot` share `builtin_chip`, which selects a builtin backend from the board's manifest, so a desktop run, a CI step and a wasm session cannot drift apart.

The invariants that direction protects:

- **No chip backend in `litz-board`.** `crates/litz-board/Cargo.toml` lists `litz-analog`, `litz-ble`, `litz-catalog`, `litz-core`, `litz-diagram`, `litz-ieee802154`, `litz-net` and `litz-radio`, plus `litz-part-wasm` for non-wasm targets. It names no MCU, no register address and no chip backend; the only backend it knows is the part backend it builds parts with. Its chip-facing surface is the board-indexed pin pair `Board::set_chip_output_on(board, gpio, drive, tick)` and `Board::take_chip_inputs_on(board)`, with `set_chip_output` and `take_chip_inputs` as board-0 aliases, in `crates/litz-board/src/runtime.rs`. An MCU adapter lives on the chip side and calls into the board; the board never calls out. The one `Chip` implementation the crate itself defines is `host::HostChip`, compiled only for wasm32, which forwards to the `litz_host` imports and holds no chip behaviour.
- **No presentation in behaviour.** `litz-diagram` parses `left`, `top`, `rotate`, `hide`, wire colour and wire routing so a file round-trips. `flatten.rs` and `runtime.rs` never read any of those fields. Wire routing (the fourth array element) is not even stored. Two diagrams that differ only in geometry produce the same netlist and the same run.
- **No wasm in the core.** `litz-core` has no dependencies, is `#![no_std]`, and knows nothing about JavaScript or `wasm-bindgen`. `litz-wasm` is the only host-side crate built as a wasm module and sits at the edge, consuming `litz-board`, `litz-core`, `litz-diagram`, `litz-engine`, `litz-esp` and `litz-mcu`. The guest crates (`litz-chip-rv32`, the part guests) are also `cdylib` but compile only inside a component.
- **An MCU is reached through one trait.** `litz_board::chip::Chip` is what the runner and the wasm session drive; five things implement it: `litz-esp`'s `EspChip` over `esp32sim`, `litz-avr`'s `Atmega328p`, `litz-rp`'s `Rp2040`, `litz-mcu-wasm`'s `WasmChip` over a component, and `litz-board::host::HostChip` over the `litz_host` imports on wasm32. Every `esp32sim` type stays behind the esp32 features in `crates/litz-esp/src/lib.rs`, and every `wasmtime` type stays in `litz-mcu-wasm` and `litz-part-wasm`; nothing above either crate stores one.
- **Deterministic ordering.** Registries, nets and endpoint maps use `BTreeMap`/`BTreeSet`, and catalogs load directory entries in sorted order (`litz-catalog/src/lib.rs`, `load_dir`). No hash iteration order leaks into a result.

## Two levels: `Circuit` versus `FlatBoard`

`Circuit` is what the file says. It lives in `crates/litz-diagram/src/lib.rs`: a list of `PartInstance` (`id`, `type_key`, `attrs`, the presentation fields `left`/`top`/`rotate`/`hide`, and any unknown keys) and a list of `Connection { from, to, color }`, where each end is a `PinRef { part, name, index }`. It is a direct, lossy-in-the-safe-direction view of `diagram.json`: unknown keys are retained, while wire routing is dropped.

`FlatBoard` is what runs. It lives in `crates/litz-board/src/flatten.rs` and holds:

- `parts: Vec<FlatPart>` — one entry per leaf, with `id`, `type_key`, the `Engine` (`Builtin(name)` or `Wasm(module)`), the pin names and pin kinds in catalog order, the merged attributes, the manifest's bus declaration, and the resolved bus `Device` when the manifest puts the part on a bus.
- `nets: Vec<Vec<Endpoint>>` — the endpoints that are electrically one node.
- `endpoint_net: BTreeMap<Endpoint, u16>` — the reverse index.
- `chip_pins: BTreeSet<u8>` — every chip GPIO that appears in a wire, across all boards.
- `boards: Vec<BoardInfo>` — the development boards in diagram order; an instance's index is the board a `ChipOn` endpoint names.
- `board_ids: Vec<String>` — their instance ids in `diagram.json`, parallel to `boards`, so a point probe can be written `b:GPIO8`.
- `chip_pins_of: BTreeMap<u16, BTreeSet<u8>>` — the same pins, kept apart per board.
- `buses: FlatBuses` — the I2C, SPI and UART wiring derived from the manifest declarations and the nets.

`Endpoint` is one of `Chip(u8)`, `ChipOn { board: u16, pin: u8 }`, `Part { part: u16, pin: u16 }` or `Rail(Rail)`. A pin is an index into a part's pin table, assigned once at flatten time, so the runtime never matches strings. Board 0 uses `Chip(pin)` so the single-board path is unchanged; a second board's pins are `ChipOn`. The runtime exposes the same split: `set_chip_output` and `take_chip_inputs` mean board 0, and `set_chip_output_on` / `take_chip_inputs_on` name a board. A net that spans two boards is one node, so a wire from `mcu_a:GPIO4` to `mcu_b:GPIO5` makes one drive visible as the other's input.

Two boards share the global rails by construction: `$gnd`, `$v33`, `$5v` and `$vin` are one node each, because the rails are what ties separate supplies together in a circuit and a differently supplied pair is not modelled. The bus fabric is keyed by `(board, controller)`, so two boards may each run their own `i2c0`, `spi0` or `uart0` without their claims meeting. A device belongs to the board whose chip pin its bus pins reach; a declaration whose bus pins land on two different boards is refused as a wiring mistake rather than treated as a bridge.

Flattening (`FlatBoard::build`) does the following:

- Requires at least one development board; each is expanded and its chip pins are tagged with that board's index. A board nested inside a board's schematic is a `BuildError`.
- Reads the board's onboard schematic (compiled in for the stock boards, read from the file named by `schematic` next to a directory-loaded `board.toml` otherwise) and expands it. The schematic's reserved `$chip` part is consumed: it never becomes a `FlatPart`, it becomes `Endpoint::Chip(n)`.
- Maps a header pin with `gpio = n` to `Endpoint::chip(board, n)`, which is `Endpoint::Chip(n)` on board 0 and `Endpoint::ChipOn` on a later board, and a `fixed` header pin (`gnd`, `3v3`, `5v`, `vin`) to the corresponding global rail.
- Adds each onboard child part under the instance prefix, so an onboard `led_pwr` on instance `esp` becomes `esp/led_pwr` and cannot collide with a top-level `led_pwr`.
- Adds every top-level part that is not the board.
- Turns top-level wires into endpoint pairs. A `$host` end such as `$serialMonitor:RX` produces no endpoint and is skipped; it is a host-side sink, not a node.
- Unions the endpoint pairs into nets with a union-find pass (`resolve_nets`). Endpoints are interned in sorted order, so net numbering does not depend on the order a diagram lists its wires.

Expansion is depth-first over a board's children and deterministic because it iterates the schematic's parts and connections in file order and stores child indices in a `BTreeMap`.

## Net resolution

`runtime.rs` owns `Board`, which holds the part instances, the resolved level of every net, the chip's current drive per GPIO, and the queue of levels to report back to the chip.

Resolution is the function `resolve`. Each endpoint on a net contributes a `Drive { level, strength }`:

- A global rail is `Strong` at its level (`Rail::level`).
- A chip GPIO that is currently an output is `Strong` at its latch value; a GPIO that is not enabled is `HIGH_Z`.
- A resistor is a weak pass-through: each terminal weakly drives the level it last saw on the other terminal (`parts::Resistor`).
- A pressed pushbutton strongly joins its two contacts; an unpressed one drives nothing (`parts::Pushbutton`).
- An LED never drives; it only listens.

Given the drives on a net:

| Situation | Result |
|---|---|
| one or more strong drivers agree | that level |
| strong drivers disagree | electrical fault (`Conflict`); the run stops |
| no strong driver, weak drivers agree | that level |
| no strong driver, weak drivers disagree | `HighZ` (floating) |
| nothing drives | `HighZ`; a receiving part's own pull applies on its side |

Strong beats weak because a pin or a rail can source current and a resistor cannot. Two strong drivers that disagree are a short, which is exactly a GPIO driving high into ground and is why `crates/litz-board/tests/board.rs::gpio_against_ground_is_a_short` expects a conflict only after the pin is enabled.

Two resistors pulling opposite ways are not a fault. That is a divider, legal hardware holding a level between the rails. A two-state model cannot say "half way", so the net reads `HighZ` and the run continues (`a_resistor_divider_reads_floating_without_a_conflict`).

A probe is a net with a name attached: `Board::add_probe` resolves a point (`<board>:<GPIO>` or `<part>:<pin>`) through the same net index the resolver uses, so a channel is not a second observation path that could disagree with the circuit. While a trace runs, every net transition is recorded; with analog co-simulation on, each probed net also gets the solved voltage at every settle. `probe::channels` turns those two records into a step waveform — a value holds until the circuit changes it — and `probe::measure` reads frequency, duty, pulse widths and the voltage range from it. Nothing in that path drives a net, so adding a probe cannot change what the circuit does.

A distinct fault is caught earlier, at validation: two different fixed rails on one wire is a `rail-short` in `litz-diagram`, and it is rejected before anything runs. The runtime fault is the dynamic case where a driver changes after validation.

Changes propagate to a fixed point. `Board::settle` recomputes every net, notifies the parts on any net that moved, and repeats. `MAX_SETTLE_ITERATIONS` is 8, far more than any passive network of this size needs. If the board has not converged by then, `settle` reports a `Conflict` with net `u16::MAX` and the message "the board did not settle; a passive loop is feeding itself". A non-converging board is an error because the alternative is an unbounded loop inside one tick; a board that oscillates is a circuit bug, not something to spin on.

## Analog co-simulation

The digital net model above is the whole story for logic and the default for every run. `crates/litz-board/src/analog.rs` adds an optional second view over the *same* flat nets, so an ADC can read a real voltage without the digital path changing shape. `Board::set_analog(true)` turns it on; the switch defaults off, and an off board never builds a netlist or calls the solver.

The mapping is deliberately one-way and derived, never declared:

- **Net to node.** One node per flat net. The net holding `$gnd` is node 0. The `$v33`, `$v5` and `$vin` nets are ideal voltage sources to ground at 3.3 V, 5 V and 5 V (`AnalogConfig` overrides them). Connectivity is a union-find over the element terminals; a connected component that never reaches ground or a rail has no absolute reference, so it is left out of the netlist and every node in it reads as floating.
- **Part to element.** `resistor` becomes `Element::Resistor` with its `value` parsed by `parse_resistance` (`220`, `10k`, `4k7`, `1M`, `4R7`). `led` becomes a `Diode` whose `DiodeModel` is derived from the colour's forward voltage at 20 mA (`Is = 20 mA / (exp(Vf/(n·Vt)) - 1)` with `n = 2`), followed by a small series resistor on an internal node. `potentiometer` becomes two resistors from pin 1 to the wiper and the wiper to pin 3, with the wiper fraction taken from the part's `PartState::Knob` (so `set_control` moves it) and a 10 kΩ track unless the manifest names `resistance`. A generic `diode`, `voltage-source` and `current-source` engine are mapped from `vf`, `voltage` and `amps`.
- **Reactive elements.** `capacitor` becomes `Element::Capacitor` with `value` in electronics notation (`100n`, `4.7u`, `2.2µF`); `inductor` becomes `Element::Inductor` the same way (`10u`, `4.7mH`). At DC the capacitor is open and the inductor a short; in transient the solver substitutes its backward-Euler companion. A capacitor counts as a connection when deciding which nets are solvable only in transient mode, so a node reached solely through one is floating in a DC solve and a real node in a transient.
- **Switches.** `pushbutton` is modelled from its `PartState::Button` and `slide-switch` from `PartState::Switch`: a closed contact is 0.1 Ω, an open contact 10 MΩ. The two legs of each pushbutton contact are joined by a milliohm resistance, so a wire to either leg reaches the switch. The throw the slide switch is not resting on is the 10 MΩ branch.
- **Sensors.** `ntc-temperature-sensor` and `photoresistor-sensor` become a two-resistor divider (10 kΩ track) from their VCC pin, or `$v33` when VCC is unwired, to ground. The ratio is the part's `PartState::Knob` fraction, so `set_control` moves the solved output; the manifest's `temperature`/`light` is the initial value.
- **Transistors.** `bjt` becomes `Element::Bjt` from `beta`, `vbe` (turn-on drop at 1 mA) and `early_voltage`; `mosfet` becomes `Element::Mosfet` from `vth`, `k` (or `beta`) and `lambda`; `opamp`/`op-amp` becomes `Element::OpAmp` from `gain` and optional `rail_low`/`rail_high`, defaulting to ground and `$v33`. Every other part is skipped, leaving its pins as digital endpoints. An analog-only engine with no digital part yet still runs: `Board::new` gives it a silent pin shell when `parts::build` reports the engine is absent, and the analog mapper reads its attributes directly.
- **Mixed mode.** A chip pin that is a strong output is stamped as a Thevenin source — 3.3 V or 0 V behind `AnalogConfig::driver_ohms` (50 Ω) — so a GPIO really sources current into a series resistor and LED. A high-impedance pin stamps nothing, which is why an input on a divider does not load it. The solve reports the LED's forward current, and `Board::state` uses it to decide `lit` when analog is on.
- **ADC path.** `Board::analog_inputs` keeps its existing behaviour when analog is off: it returns each analog part pin's static `Part::analog()` value. When analog is on it substitutes the solved voltage at that part pin, and a pin whose net did not solve is omitted rather than reported as 0 V. `LitzModel` in `litz-esp` consumes `analog_inputs`, so the value `publish_analog` writes to a pad is the solved node voltage with no extra plumbing.

### Transient

`Board::analog_transient(dt_ns)` advances one backward-Euler step of `dt_ns` nanoseconds and returns the new node voltages; `Board::analog_advance(to_ns)` advances to the chip's time in slices of the last chosen step (default `ANALOG_DEFAULT_DT_NS`, 1 µs) and shortens the final slice so it lands exactly on `to_ns`. The run keeps its own clock, which starts at 0 when transient mode begins. The run is built once and kept across calls, so reading a pin between steps never rebuilds the netlist. The first call starts from a zero state — every node at 0 V, every inductor at 0 A — which is what makes an RC charging curve start discharged rather than at its DC steady state. Backward Euler is first order and unconditionally stable; with a hundred steps per time constant its error is about 0.3%, and the accuracy is entirely the caller's step choice.

`Board::analog_solve` is always the DC operating point, even while a transient is live. `Board::node_voltage`, `Board::node_voltage_gpio`, `Board::analog_inputs` and `Board::state` read the live transient solution when one exists, and the DC solution otherwise.

`Board::analog_solve` caches its `AnalogSolution` in a `RefCell` and the transient keeps its state on the board. One invalidation rule covers both: a chip drive change, a press, a control change (`set_control`), or a circuit/configuration change (`set_analog`, `set_analog_config`) drops the cached DC point and discards the whole transient run. Nothing tries to splice old numeric history onto new topology. The solve itself is pure and deterministic (net order, part order, fixed Newton loop), and calling it is observational: it cannot change a net level, a part state or a conflict. That is what keeps the analog-off regression exact.

## Time

Time is a `u64` count of nanoseconds, `Tick`, with `TICKS_PER_SEC = 1_000_000_000` (`crates/litz-core/src/lib.rs`). There is no wall-clock time anywhere in the scheduling model. A peripheral declares its own clock period in ticks; when the period is not a whole number of nanoseconds, `Clock::advance` in `crates/litz-mcu/src/lib.rs` carries the remainder into the next call rather than rounding each cycle, which is the same fixed-point divider the hardware uses.

Simulated time versus wall-clock:

- The runner's stop condition is simulated time: `mcu.frontier_ns() >= timeout` in `crates/litz-engine/src/runner.rs`. A ten-second test takes however long the host needs, and the same firmware always gets the same ten simulated seconds.
- A fault stops the run immediately, before any work is spent on a doomed boot: the loop checks `mcu.conflicts()` before and after each chunk.
- The `run` command's exit code is 42 at the time limit (0 if every board halts first), which is why it is a "did it boot without fault" probe rather than a pass/fail assertion.

The runner advances in fixed chunks of `mcu.run(1_000_000)` scheduling steps. Between chunks it drains the boards' consoles, checks conflicts and evaluates the expected and failure strings. Fixed chunks keep those checks bounded and give every chunk the same shape regardless of what the firmware is doing; the runner cannot see inside the backend, so the chunk boundary is its only observation point.

The ESP adapter pins the backend's batching knobs in `EspChip::build`: `machine.vq_max = 1` and `machine.bb_max = 1`. Those fields would otherwise default from `ESP32SIM_VQ` and `ESP32SIM_BB` at construction. Pinning them selects the plain one-quantum schedule, which is the bit-exact reference the batching is defined against, and removes the environment from the result.

## Determinism

A CI gate is only a gate if the same inputs produce the same bytes. The properties that would break determinism, and what the project does instead:

| Breaker | What the project does |
|---|---|
| Environment variables | `EspChip::build` builds the machine directly instead of through `esp32c3::machine`, which calls `DebugFlags::from_env()`; it then pins `vq_max` and `bb_max` away from `ESP32SIM_VQ`/`ESP32SIM_BB`. |
| Host clock | Time is `Tick` nanoseconds from simulation start; the runner never reads the system clock. The timeout compares against `frontier_ns()`, an integer tick. |
| Hash iteration order | Catalogs, nets, endpoints and pin indices use `BTreeMap`/`BTreeSet`; `load_dir` sorts filenames before parsing. |
| Floating point in the schedule | Scheduling is integer nanoseconds and `Clock` carries the fractional remainder. Duration strings are parsed once from decimal text into ticks (`parse_duration`) before any time is scheduled. |
| Input ordering | `resolve_nets` interns endpoints in sorted order, so net ids do not depend on wire order. |

The enforcing test is `c3_run_is_deterministic` in `crates/litz-esp/tests/c3_boot.rs`. It boots the same firmware on two fresh machines and asserts byte-identical serial output. Like the rest of that file it is `#[ignore]`d and needs `LITZ_ROM_DIR` and `LITZ_C3_ASSETS`. A backend's `seconds()` is a floating-point convenience derived from its cycle count, used for the reported `sim_time_ms`; it never drives scheduling or the timeout.

## The backend boundary

The runner and the `litz-wasm` session each hold a `Box<dyn litz_board::chip::Chip>` (`crates/litz-board/src/chip.rs`). The session also keeps the `Rc<RefCell<Board>>` and the parsed `Circuit` that `machine::boot` returned, so probes, part state and part enumeration do not need another backend method. The trait is the whole surface a backend has to provide:

| Group | Calls |
|---|---|
| identity | `info()` — name, `cpu-hz`, flash offsets |
| images | `load_image(kind, offset, bytes)`, `boot()`, optional `set_psram_size(bytes)` |
| time | `run(budget_cycles)`, `seconds()` |
| console | `write_console(bytes)`, `take_console()` |
| scheduling | `schedule(Vec<Input>)` — GPIO levels and UART bytes at simulated times |
| circuit | `state(part)`, `press(part, pressed)`, `set_control(part, value)`, `set_named_control(part, name, value)`, `part_pin_level(part, pin)`, `gpio_level(gpio)`, `conflicts()`, `fault()` |
| observation | `start_trace()`, `vcd()`, `display()`, `display_version()`, `save_display(path)`, `save_part_display(part, path)`, `touch(x, y, down)`, `sample_heap()`, `start_air_capture(path)`, `start_ethernet_capture(path)`, `attach_host_gateway(forwards)`, `report()`, `enable_coverage()`, `break_at(pc)`, `registers()`, `debug()` |

Every call after the circuit group has a default that reports "this backend cannot do that", so a backend implements only the groups it supports. `boot()` decides how the chip starts, so the runner does not know that a bootloader image means "do not call `boot_app`"; that is the ESP adapter's business. Image offsets come from `info().flash`, so the runner has no `0x8000`/`0x10000` constants either.

**`litz-esp`.** One generic `EspChip<S: Soc>` implements `Chip` for every esp32sim machine; the per-chip part is a type alias, a `SocBus` factory and a feature flag. ESP32, C3, C6, S3, P4 and S31 are wired. The build for `target_family = "wasm"` enables only C3, C6, P4 and S31; classic ESP32 and S3 are Xtensa, and esp32sim executes their guest blocks through host JIT imports that a module with only the `litz_host` namespace does not have. `schedule` renders the `Input` list into esp32sim's script syntax (`<seconds> gpio <pin> <0|1>`, `<seconds> uart0 <text>`) and calls `load_script`. The board side is the `LitzModel` adapter described below.

**`litz-mcu-wasm`.** `WasmChip` loads a component, reads `info()` once, and implements the trait by pushing `set-inputs` before each `run`, pulling `take-outputs` after it, and translating each output into `Board::set_chip_output_on`. The guest has no WASI: memory is capped through `StoreLimits` and each call gets a fuel budget, so a runaway chip comes back as `stop: trap` instead of hanging the host. Only the wasmtime loader is compiled out of the wasm build; `HostState`, the host-call implementation the browser bridge dispatches to, is compiled there too. In that build a custom chip is `litz-board::host::HostChip`, driven by the JavaScript host.

### The board side of an esp32sim backend

The `BoardModel` adapter, `LitzModel` in `crates/litz-esp/src/lib.rs`, bridges `esp32sim`'s board callbacks to the Litz board:

- `gpio_output_at(cycle, changes, enabled, output)` turns the GPIO latch and enable mask into `Board::set_chip_output`. Only enabled bits are `Strong`; everything else is `HIGH_Z`. It deliberately diffs the previous enable and output values, because an enable-only transition is not in `changes` and releasing an output must stop driving the net.
- `take_edges` drains `Board::take_chip_inputs` into `BoardEdge`s; a `HighZ` result becomes a released input.
- `next_deadline` returns `Some(0)` while inputs are pending, so a board change reaches the chip immediately rather than at its next poll.
- `input_levels` and `named_pin` forward to the board for attach-time state and script addressing.

Adding another esp32sim chip means a `pub type`, a `SocBus` factory and a feature in `litz-esp`, then a `board.toml` whose `chip` names it. Adding a chip esp32sim does not model means none of that: write a wasm component against the WIT contract, point `[board.simulator]` at it, and nothing in `litz-board`, `litz-diagram`, `litz-catalog` or the runner changes. On the wasm target that component runs through `litz-board::host::HostChip`, which registers a `GuestHost` built from `litz_mcu_wasm::HostState`, so both the 0.1 and 0.2 worlds reach the same board code the native wasmtime loader uses.

### Buses and devices

A part that answers transactions rather than pin levels is a bus device. Its manifest declares one `[buses.*]` section (`docs/formats.md` §2.1), its pins carry bus kinds, and flatten resolves the wires into `FlatBuses` in one pass. Membership has a single source: the manifest says the part is a device and the diagram's wires say which chip GPIOs carry the bus. Flatten refuses two devices at one I2C address, two spellings of one controller's pins, a shared chip-select or UART port, and one GPIO claimed by two bus roles, so the runtime never has to choose between contradictory declarations.

`Board` owns `Buses` from `crates/litz-board/src/bus.rs`: one `I2c` per `(board, controller)` keyed by 7-bit address, one `Spi` per `(board, controller)` keyed by chip-select GPIO, and one `Uart` per `(board, port)`. `devices::build` constructs the builtin devices during `Board::new` — the BMP280, SSD1306, DS3231, PCF8574, MPU6050, MCP3008, the I2C sensor family, the SPI displays, storage parts and the rest of the catalog's bus devices — and `parts::build` gives the same part a passive `BusShell` so its VCC and GND pins stay in the netlist without driving the rails a second time.

The ownership rule is that the board keeps its devices. A backend never takes a device out of `Board`; esp32sim's controller is given a handle (`BoardI2cDevice`) that holds the shared `Rc<RefCell<LitzBoard>>`, a controller index and an address, and borrows the board for the length of one byte. That is why `i2c_devices()` can hand out device boxes and the board remains usable by tests and the host. SPI and UART need no handle at all: `Board::spi_transfer_at(board, controller, chip_select, tx, rx_len, at)` selects the addressed device on that controller, `Board::uart_receive(board, port, at, bytes)` passes the chip's bytes to `Uart::receive`, and `Board::uart_poll(board, port, at)` drains `Uart::poll`. The resolved bus wiring supplies the pins a probe observes and the line rates the waveform uses; a UART with no configured rate falls back to 115 200 baud, I2C to 100 kHz and SPI to 1 MHz mode 0.

### The wasm chip contract

`crates/litz-mcu-wit/wit/litz-mcu.wit` is package `litz:mcu@0.1.0`, world `simulator`: imports `host.console` and `host.fault`, exports the `chip` interface. `wit-v2/litz-mcu.wit` is `litz:mcu@0.2.0`, the same world plus analog reads, I2C/SPI/UART transactions, Wi-Fi, TCP/UDP, BLE scanning and advertising, and BLE connections with GATT discovery, read, write and notify. The two package versions are different component types, so `litz-mcu-wasm` links both sets of generated bindings and `WasmChip` picks one from the manifest's `abi`, never by inspecting the component's exports; a 0.1 module stays loadable and a 0.2 module can be checked before a project depends on it with `litz board check`. The host side is generated by `wasmtime::component::bindgen!` straight from each file, so a guest and a host cannot disagree about the shape without one failing to build. `docs/formats.md` §10 lists the functions. On the wasm target the same WIT files describe the component, but the host is the JavaScript side rather than wasmtime: `litz-board::host::HostChip` pushes images and pins through `litz_host`, the transpiled guest's imports are answered by `chipImports` in `js/src/jco.js`, and every 0.2 call on that path is a `litz_guest_call(chip, op, args, out, cap)` export returning a `{"ok":true,"value":...}` or `{"ok":false,"error":...}` JSON envelope. The export dispatches to the registered `litz_mcu_wasm::HostState`, so analog reads, I2C/SPI/UART, Wi-Fi scan/join/leave/resolve, TCP/UDP, BLE scan/advertise/GATT and raw radio all reach the same board code the native loader uses. Both `litz:mcu@0.1.0` and `0.2.0` are fully bridged.

Every 0.2 bus call also feeds the observation record: `Board::i2c_write_read_at`, `spi_transfer_at` and `uart_receive` synthesize the edges a probe on the wired pins would have seen (`crates/litz-board/src/waveform.rs`), and `scope()`/`vcd()` merge them with the settled net trace in time order. The transaction still reaches the device through the byte-level bus; the waveform is an observation, never a second driver.

The reference guest is `crates/litz-chip-rv32`: an RV32IMC interpreter (`src/cpu.rs`), a machine with IRAM/DRAM, GPIO and UART0 (`src/machine.rs`), and the glue (`src/component.rs`, compiled only for `wasm32` because its exports name host symbols that exist inside a component). It speaks 0.1. The 0.2 template is `crates/litz-cli/templates/board/`, which `litz catalog new board <id>` renders into a complete bundle under `boards/<id>/`; its guest is a small register machine whose byte programs call into the buses, the ADC, Wi-Fi, TCP, BLE and the radio medium, and a copy built from it is the fixture for `crates/litz-mcu-wasm/tests/v2.rs`. `build.sh` compiles either for `wasm32-unknown-unknown` and componentizes the result with `litz-componentize`, a small binary in `litz-mcu-wasm` that wraps `wit-component`; no `cargo-component` install is needed.

### The wasm part contract

`crates/litz-part-wit/wit/litz-part.wit` is package `litz:part@0.2.0`, world `component`. It imports `host.log` and `host.rom` and exports the `part` interface. The first half is `litz_core::Part` across a component boundary: pins by index, `set-pin(pin, level, at-ns)`, `advance`, `next-event`, `drive`, `state`, `press`, `set-control`, `analog`, and one `info()` call at load. The second half is what turns a component into a catalog device:

- **Bus transactions.** `i2c-connect`/`i2c-read`/`i2c-write`/`i2c-stop`, `spi-select`/`spi-transfer`, and `uart-receive`/`uart-poll`. `devices::build` attaches a wasm part declared on `[buses.*]` exactly as it attaches a builtin device, so one component can be an EEPROM, a display or a sensor without anything above the bus knowing.
- **Attributes.** `attributes()` returns `{ name, description, value }` entries and `set-attribute` writes one; the value variant is `number`, `text` or `flag`.
- **Framebuffer.** `framebuffer()` advertises `{ width, height }`, `draw(offset, pixels)` uploads RGBA8888 bytes, and `damage()` returns and clears the pending changed rectangle.

The 0.1.0 package is kept verbatim in `crates/litz-part-wit/wit-legacy/litz-part.wit`. A part manifest's `abi` is checked against the accepted names but does not choose the world: `WasmPart` inspects the component's exported interface name to decide between the 0.1 and 0.2 bindings, so a module built against either contract loads; the 0.1 calls that 0.2 added answer with empty defaults. `crates/litz-part-74hc595` still builds against the legacy package and remains the compatibility proof.

`WasmPart` in `litz-part-wasm` mirrors `WasmChip`: no WASI, a `StoreLimits` memory cap and fuel per call (a runaway fixture traps in well under a second). Both worlds share one `HostState`, which holds the ROM image and the log lines; memory and fuel are not duplicated. `parts::build` selects it from `Engine::Wasm`, which flatten fills from the catalog manifest. A builtin entry carries its module bytes in the binary; an entry loaded from a directory is read from beside its manifest, after the catalog has refused an absolute path or one containing `..`. The dependency is target-gated rather than a feature because a wasm32 module cannot host a second component; there, `parts::build` and `devices::build` select `host::HostPart` and the JavaScript host does the instantiation. A plain Cargo feature would be re-enabled through `litz-esp`'s optional `litz-board` dependency.

`litz-board` implements the bus traits (`I2cDevice`, `SpiDevice`, `UartPeer`) for `WasmPart` in `devices.rs` rather than in `litz-part-wasm`, because those traits live in `litz-board` and the alternative would be a dependency cycle. `I2cDevice::connect` carries only the read/write phase, so the board records the catalog address on the instance (`WasmPart::set_i2c_address`) for the guest's `i2c-connect(addr, read)`.

Guest sources use `crates/litz-part-sdk`, which runs `wit-bindgen` once and re-exports `Guest`, `export!`, the record types and the `host` helpers. The reference devices are `crates/litz-part-eeprom` (`litz-24c02`) and `crates/litz-part-display` (`litz-rgb-panel`); each keeps its logic in a host-testable module and its glue in `src/component.rs`, compiled only for `wasm32`. `crates/litz-part-runaway` is a test-only fixture whose `drive` never returns, used to prove the fuel cap. Every guest's `build.sh` uses the same two `cargo` commands as the chip's.

## Seams

**Rendering.** Two things are stable for a front end: `Circuit` keeps presentation fields so an editor can round-trip a file, and `Board::state` / `EspChip::state` return `PartState` (`Led { lit }`, `Button { pressed }`, `None`). A renderer reads those; it may assume part ids and pin levels, and it must not assume wall-clock timing or reach into chip registers. `litz-wasm` is the layer that exposes this across the wasm boundary, and the `@litz-sim/litz` 0.4.1 package (`js/`, built from `litz-wasm` and the JavaScript sources) wraps it as a headless library. `litz serve [dir]` is only a static file server — default directory `.`, default port 8000 on loopback — for a host that needs to load a wasm module or firmware over HTTP.

**WebAssembly.** `litz-wasm` (`cdylib` + `rlib`) is the only host-side crate with a wasm boundary, and `crates/litz-wasm/src/lib.rs` is that ABI: construct a machine from firmware and a diagram, advance by steps, take serial bytes, read part state, press a part, set a control, record a VCD, and read the scope of the points a host watches. It also accepts `litz_decode_add(handle, spec, len)`, which takes the same `SPEC` text a `--decode` flag takes, and the scope document it returns carries a top-level `decodes` array beside `probes`, one entry per decoder with its frames as `{ at, end, text, error }`. It is a plain C ABI rather than wasm-bindgen, so the module needs no wasm-bindgen glue. `litz-core` stays `no_std` with no JS. A wasm host may assume only those calls; it may not assume a filesystem, a network, a clock or stdin/stdout. `litz_new_json` is the constructor the `@litz-sim/litz` package uses: one JSON document names the diagram, the images' buffers in linear memory, and optional `flashSize`, `mac`, `analog` and host-supplied catalog `manifests`, so a host with no directory can boot a custom board (`docs/formats.md` §12). The module imports exactly one namespace, `litz_host`, which the JavaScript side answers for custom components; a simulation with no custom component calls none of it. The wasm build boots the RISC-V ESP parts (C3, C6, P4 and S31), the ATmega328P and the RP2040. Classic ESP32 and ESP32-S3 are absent because their Xtensa backend needs host JIT imports that namespace does not provide.

**Radios and networks.** An air interface is a property of the circuit, not of a chip adapter, so four crates describe one each and the parts on the board carry it. `litz-radio` is the shared packet medium: a part computes how long its own frame occupies the air and hands that in with the payload, so an nRF24 model and a LoRa model share one medium without either teaching the hub its register map; frames on one network and channel whose airtimes overlap destroy each other, and only a completed frame can be read. `litz-net` is the network behind an access point: DNS, TCP, UDP, a TLS 1.3 endpoint, a WebSocket server and an MQTT broker answered byte for byte, and a nanosecond pcap of the frames the router carried (`--pcap`), reached through a `litz-wifi-router` part and read by the backend through `Board::network`. TLS is deterministic because `rustls` is given a seeded ChaCha20 generator for both its randoms and its X25519 key exchange, and the trust store is the simulated CA shipped with the crate; the cryptography is `rustls-rustcrypto` and one suite is offered, so the wasm build and the native engine produce the same handshake bytes. `vendor/rustls-pki-types` answers rustls's clock call with the epoch so the TLS stack needs no JavaScript imports, and `getrandom` uses a custom backend that reports unsupported because the seeded provider replaces both of its callers. `litz-ble` is the advertisers around a station — their payloads and their GATT tables — reached through `litz-ble-beacon` parts and `Board::bluetooth`, with connections and writes held by the board so a central and a peripheral on the same circuit meet without either backend knowing the other. `litz-ieee802154` supplies the FCF, address-filtering and FCS handling the AT86RF233 part uses on the same medium. Each backend translates the board's configuration into its own radio (esp32sim's 802.11 link, its HCI Bluetooth controller, or the medium's calls) and nothing above the backend learns an esp32sim type. A diagram therefore keeps its network, its beacons and its packet peers whichever chip carries them.

**More parts.** A new part is a `part.toml` in the catalog plus a behaviour. Behaviour is either a builtin engine added to `crates/litz-board/src/parts.rs` (selected by `engine = "..."`) or a wasm module (selected by `backend = "wasm"`, loaded by `litz-part-wasm`). A pin part implements `Part` from `litz-core`: it sees `Drive`, `Level` and `Tick`, it may not read chip state, and its pins are indices into its own table. A bus or display part writes a 0.2 component, declares `[buses.*]`, and is built by `devices::build`; it needs no change in `litz-board` beyond the contract it implements. The catalog id is the one identifier that may never change. On the wasm target `litz-part-wasm` is compiled out; a wasm part is instantiated by the JavaScript host and reached through `litz_host` (`host::HostPart`), so a custom part loads in a wasm host as well as natively.

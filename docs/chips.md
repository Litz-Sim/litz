# Writing a chip

A chip in Litz is a microcontroller that is itself a WebAssembly component. It is the layer below a schematic part: instead of being wired into the diagram and driven by pin changes, it boots an image, runs a cycle budget, and reaches the rest of the circuit through a small host interface. This page is the authoring guide for such a component. It assumes the reader has a project, a Rust toolchain with the `wasm32-unknown-unknown` target, and either a Litz checkout or the `litz` binary on `PATH`.

The reference implementation is `crates/litz-chip-rv32`, an RV32IMC core with enough of the ESP32-C3 memory map to run the repository's firmware. The scaffold that `litz catalog new board` writes is a smaller, self-contained example of the same contract.

## What a chip is

A chip is one of three pieces of a board bundle, and all three are needed before a diagram can use it:

- a `board.toml` manifest, which declares the header pins the outside world sees and points at the component under `[board.simulator]`;
- an onboard schematic, `<id>.onboard.json`, whose single reserved `$chip` entry stands for the MCU and whose other parts are the devices soldered to the board;
- the component file, `<id>.wasm`, which implements the `litz:mcu` world and runs inside the host.

The board is what `diagram.json` places. The chip is what the board boots. A diagram never names the `.wasm` file directly: it names the board's catalog id, and the runner reads the manifest, resolves the module and schematic beside it, and instantiates the component.

That is the difference from a schematic part. A part speaks `litz:part`, lives in a part manifest with its own `[pins.*]` table, is placed by `diagram.json` (or by a board schematic), and is driven by `set-pin`, bus transactions and `press`/`set-control`. A chip speaks `litz:mcu`, has no `[pins]` table of its own, is the MCU of exactly one board, and is driven by images, pad inputs and `run(budget-cycles)`. It owns the clock: its `run` reports how many cycles it consumed, and the board advances to that time. A part never has a clock of its own; it sees only the `at-ns` values the host passes in.

| Question | Schematic part | MCU chip |
|---|---|---|
| Contract | `litz:part@0.1.0` or `0.2.0` | `litz:mcu@0.1.0` or `0.2.0` |
| Manifest | `[part]` with `[pins.*]` | `[board]` with `[[pins]]` |
| Where it is named | a `diagram.json` part or a board schematic part | the board's `[board.simulator]`, never the diagram |
| Pin table | its own | the board's header pins, seen as GPIO numbers |
| Driven by | `set-pin`, I2C/SPI/UART, `press`, `set-control` | `load-image`, `set-inputs`, `run`, `write-console` |
| Time | host-supplied `at-ns` only | its own cycle count, reported to the host |
| Count | many per diagram | one per board |

A chip may also be installed as a finished bundle, exactly like a part. `litz catalog add <bundle> --dir boards` validates the manifest, the module and the schematic, then copies the three into the receiving project's `boards/<id>/`. The simulator source is not installed; only the manifest, the module it names and the schematic.

## Choosing the ABI

Two contracts are current. The manifest's `abi` field picks one, and that field is the only thing that picks it:

| `abi` | WIT package | World | Adds |
|---|---|---|---|
| `litz-mcu-1` (default) | `litz:mcu@0.1.0` | `simulator` | pins and console only |
| `litz-mcu-2` | `litz:mcu@0.2.0` | `simulator` | analog reads, I2C/SPI/UART, Wi-Fi with TCP/UDP, BLE with GATT, and the raw radio medium |

The 0.1 contract has one core loop and two host calls; a guest can emit console bytes, drive and read pads, and report a fault, and that is all. The 0.2 contract keeps the same exported `chip` interface and adds the host calls a real peripheral block needs: an ADC that reads the solved node voltage, bus transactions against the parts the diagram wired, the deterministic router's Wi-Fi and sockets, BLE advertising and GATT, and the shared nRF24/SX1276/AT86RF233 packet medium. If the board has an I2C sensor, an analog input or a radio, it needs 0.2.

The version is not inferred from the component. `crates/litz-mcu-wasm/src/lib.rs` links both sets of bindings and builds the one the manifest field names; a typo or an unknown value is refused before the component is instantiated, with a message naming `litz-mcu-1` and `litz-mcu-2`. This is deliberately unlike the part contract, where `WasmPart` reads the component's exported interfaces and selects the version from them. For an MCU the manifest is authoritative, because the two worlds have different import shapes and the host must build a linker before it can look inside.

A board that omits `abi` gets `litz-mcu-1`. The scaffold that `litz catalog new board` writes always uses `litz-mcu-2`; to author a 0.1 chip, copy `crates/litz-mcu-wit/wit/litz-mcu.wit` into the bundle (for example as `wit/litz-mcu.wit`), point the guest's `wit-bindgen` path at it, and set `abi = "litz-mcu-1"`.

## The scaffold

From the project that should own the board:

```sh
litz catalog new board vendor-mcu --name "Vendor MCU"
```

The id gains a `board-` prefix when it does not already have one, and the bundle lands under `boards/` unless `--dir` says otherwise. The command prints the directory and the two commands that turn it into a running board:

```
created /project/boards/board-vendor-mcu
  next: cd /project/boards/board-vendor-mcu && ./simulator/build.sh
        litz board check .
```

It refuses to merge into a non-empty directory and refuses an id that is not a plain file name. The generated directory is:

```
boards/board-vendor-mcu/
  board.toml
  board-vendor-mcu.onboard.json
  simulator/
    Cargo.toml
    src/lib.rs
    build.sh
  wit-v2/
    litz-mcu.wit
  README.md
```

| File | What it is for |
|---|---|
| `board.toml` | The catalog entry. Declares the header pins, `flash_size`, the schematic file name, and `[board.simulator]` with `kind = "wasm"`, `module = "board-vendor-mcu.wasm"` and `abi = "litz-mcu-2"`. |
| `board-vendor-mcu.onboard.json` | The board's internal circuit. One `$chip` part stands for the MCU; any devices soldered to the board are listed here too. |
| `simulator/Cargo.toml` | The guest crate. `crate-type = ["cdylib"]`, one dependency on `wit-bindgen`, and its own empty `[workspace]` so it does not join a parent workspace. |
| `simulator/src/lib.rs` | The guest. A toy register machine with 4 KiB of RAM, a 64-byte flash window and one byte-instruction per cycle, plus the `litz:mcu` glue at the bottom. Replace the machine, leave the glue. |
| `simulator/build.sh` | Compiles for `wasm32-unknown-unknown` and wraps the core module in a component, writing `board-vendor-mcu.wasm` next to `board.toml`. |
| `wit-v2/litz-mcu.wit` | A byte-identical copy of `crates/litz-mcu-wit/wit-v2/litz-mcu.wit`, the contract the guest compiles against. |
| `README.md` | Usage notes and the scaffold's opcode-to-host-call table. |

The generated `board.toml` pins are `3V3`, `GND` with `repeat = 2`, `GPIO0` through `GPIO7`, and `RX`/`TX` aliases of GPIO 4 and 5 tagged `functions = ["uart"]`. The aliases carry no traffic on a wasm board — the guest's console is the `host.console` call, not a pad — but they let a diagram written for a real board validate.

## Step-by-step authoring

### The pin table in `board.toml`

Every `[[pins]]` entry is one header contact. Each sets exactly one of `gpio` or `fixed`.

```toml
[board]
id = "board-vendor-mcu"
name = "Vendor MCU"
version = "0.1.0"
flash_size = "4MB"
psram = false
schematic = "board-vendor-mcu.onboard.json"

[board.simulator]
kind = "wasm"
module = "board-vendor-mcu.wasm"
abi = "litz-mcu-2"

[[pins]]
name = "3V3"
fixed = "3v3"

[[pins]]
name = "GND"
fixed = "gnd"
repeat = 2

[[pins]]
name = "GPIO0"
gpio = 0
functions = ["adc"]
```

`gpio` names a chip pad by number; the chip's `set-inputs` and `take-outputs` use that number. `fixed` is one of `gnd`, `3v3`, `5v` or `vin`, and reaches a global rail rather than a pad. `repeat` says how many physical contacts share the name, so a repeated pin is addressed as `GND.1` and `GND.2` from a diagram. `functions` is free-form description (`adc`, `pwm`, `i2c`, `uart`, `spi`) and does not change behaviour. Names must be unique; GPIO numbers need not be, which is how the scaffold's `RX` and `TX` alias GPIO 4 and 5.

The pin names are load-bearing in two places: the onboard schematic wires `$chip` pins by header name, and a diagram wires `mcu:<name>`. Renaming a header pin is a breaking change to every diagram that used it.

### The `$chip` part in the onboard schematic

The onboard schematic is an ordinary `diagram.json` shape with one reserved part type. The scaffold is the minimum:

```json
{
  "version": 1,
  "author": "Litz",
  "editor": "litz",
  "parts": [
    { "id": "chip", "type": "$chip", "attrs": {} }
  ],
  "connections": []
}
```

The `$chip` id is local (`chip` by convention) and the expansion consumes it: it never becomes a leaf part. Its pins are the board's header pins, by name. An on-board LED on GPIO 0 is added as a child part and wired like this:

```json
{
  "version": 1,
  "parts": [
    { "id": "chip", "type": "$chip", "attrs": {} },
    { "id": "led_pwr", "type": "litz-led", "attrs": { "color": "green" } },
    { "id": "r_pwr", "type": "litz-resistor", "attrs": { "value": "1000" } }
  ],
  "connections": [
    ["$v33", "r_pwr:1", "", []],
    ["r_pwr:2", "led_pwr:A", "", []],
    ["led_pwr:C", "$gnd", "", []]
  ]
}
```

Power pseudo-parts are `$gnd`, `$v33`, `$v5` and `$vin`, and they join the board's global rails. A wire to `chip:GPIO0` addresses chip GPIO 0; because the scaffold names its header pins `GPIO0` and so on, the two spellings coincide there. A board that names a header pin `D13`, as the built-in Uno does, wires `chip:D13`. A `$chip` pin that is not a header name is refused when the board is flattened, and a child part that is itself a board is refused too: a board may not contain a board.

Child part ids are prefixed with the board instance id when the board is expanded, so `led_pwr` inside `mcu` is reachable as `mcu/led_pwr` and cannot collide with a top-level `led_pwr`.

### Implementing the WIT world

The world is `simulator`: it imports `host` and exports `chip`. The guest's bindings module names the WIT directory and the world:

```rust
mod bindings {
    wit_bindgen::generate!({
        path: "../wit-v2",
        world: "simulator",
    });
}
```

The exported `chip` interface is the same in both ABI versions:

| Export | Signature | Meaning |
|---|---|---|
| `info` | `() -> chip-info` | Name, CPU rate, memory windows and flash layout. Called once at load. |
| `load-image` | `(kind: image, offset: u64, bytes: list<u8>) -> result<_, string>` | Place a ROM, bootloader, partition table or firmware image. `offset` is in the guest's non-volatile space. |
| `reset` | `(mac: list<u8>)` | Reset the core; `mac` is a network chip's station address across reset. |
| `boot` | `() -> result<u32, string>` | Start the firmware and return its entry address. |
| `set-inputs` | `(inputs: list<pin-input>)` | Apply the pad levels the board resolved since the last run. |
| `run` | `(budget-cycles: u64) -> run-result` | Execute up to the budget and report `cycles` plus a `stop`. |
| `take-outputs` | `() -> list<pin-output>` | Report pads whose drive changed since the last call. |
| `write-console` | `(bytes: list<u8>)` | Receive terminal bytes into the guest's console path. |
| `read` | `(addr: u32, len: u32) -> result<list<u8>, string>` | Debug peek at the address space; an unmapped range is an error, not zeroes. |

`chip-info` carries the name, a description, `cpu-hz` (the rate `run-result.cycles` is counted at), a list of executable memory regions, and the flash layout (`size`, `bootloader`, `partition-table`, `app`). The host places each image at the offset the guest declares for its kind, so a guest must be honest about its layout or the image lands in the wrong place. `run-result` is `{ cycles, stop }`, where `stop` is `budget`, `idle`, `halted`, `trap` or `reset`.

One rule governs the whole contract: **one call runs a budget**. `run` is called once per few million instructions, not once per instruction, because a call per instruction would spend more time in the component boundary than in the machine. A guest may return early (`idle`, `halted`), but it may not loop past the budget, and it must return so the host can advance its own clock. The scaffold's `Machine::run` is a good model: a bounded loop over byte instructions, one cycle each, returning `Stop::Halted` or `Stop::Budget`.

State lives per component instance. The scaffold uses a `thread_local!` holding a `RefCell<Machine>`, which gives each instance its own machine without `unsafe`; the contract has one thread per instance.

### Building the component

`simulator/build.sh` is two commands plus a guard. It needs `litz-componentize`, which ships with Litz; from a checkout, install it once:

```sh
cargo install --path crates/litz-mcu-wasm --features tooling
```

Then build from the board directory:

```sh
./simulator/build.sh
```

The script runs, from inside `simulator/`:

```sh
cargo build --target wasm32-unknown-unknown --release --target-dir target
litz-componentize \
  target/wasm32-unknown-unknown/release/<crate_snake>.wasm \
  ../<id>.wasm
```

The crate is named `<id>-sim` and its snake-case form replaces hyphens with underscores, so `board-vendor-mcu` builds `board_vendor_mcu_sim.wasm` and writes `board-vendor-mcu.wasm` next to `board.toml`. The `--target-dir target` keeps the artifact path predictable when the author has a global `build.target-dir` configured.

`litz-componentize` is not a formatter: it reads the `component-type:<world>` custom section that `wit-bindgen` embeds in the core module and re-emits the module as a component the host can link. Building for the target without that step leaves a core module, and `board check` will say it does not instantiate. Any toolchain that produces a component carrying the world's interface metadata works; the manifest only names the `.wasm` file and the ABI.

### Keeping the guest free of WASI

The host links no WASI, no filesystem, no clock and no randomness. The only imports a guest sees are the `litz:mcu/host` calls and the contract's own types interface. Build for `wasm32-unknown-unknown`, as the scaffold does; a component built for a WASI target, or one that pulls in a crate calling `getrandom`, `std::time::SystemTime`, `std::fs`, `std::net` or threads, will fail to instantiate because those imports have no implementation.

The scaffold uses `std` freely for `Vec`, `RefCell`, `thread_local!` and `format!`, and that is fine: `std` itself does not require WASI. What matters is that the guest never asks for an operating system. Time is the cycle count it reports; bytes come from `write-console`; network answers come from the circuit's router; and every peripheral answer is a host call. A guest that respects that boundary runs unchanged on every host.

## Checking and installing

`litz board check` answers the questions a project would ask before it depends on a bundle, without a project or a firmware. It accepts the bundle directory or the manifest path:

```sh
litz board check boards/board-vendor-mcu
```

For the scaffold built as-is, the report looks like this:

```
board-vendor-mcu 0.1.0 (Vendor MCU)
  module board-vendor-mcu.wasm (litz-mcu-2)
  component reports board-vendor-mcu
  cpu 16000000 Hz
  header pins 12
  schematic wires 0
```

The first line is the catalog id, version and display name. The `module` line names the file and the ABI the manifest selected. `component reports` and `cpu` come from the guest's own `info()` and are printed only when the component instantiated. `header pins` counts `[[pins]]` entries and `schematic wires` counts the schematic's connections.

The check fails, with exit code 1, when the manifest does not parse, when it is a part manifest rather than a board, when `chip` is not a builtin backend, when a declared schematic is missing or malformed or has no `$chip` part, or when the module does not instantiate under the ABI the manifest names. A wrong ABI or a core module rather than a component is the common case, and the message says the guest must be built against the WIT that `abi` names. Exit code 2 means the subcommand or arguments are wrong.

A board created under a project's `boards/` is already in that project's catalog. To move it to another project, run this from the receiving project's root:

```sh
litz catalog add /path/to/boards/board-vendor-mcu --dir boards
```

`add` validates the whole bundle before writing: the manifest parses, the id is neither builtin nor already installed, the module and schematic are relative paths inside the bundle, and every referenced file exists. It copies the manifest, the module and the schematic, and leaves `simulator/` and `wit-v2/` behind.

A diagram then names the board exactly as it names any built-in board:

```json
{
  "version": 1,
  "parts": [
    { "id": "mcu", "type": "board-vendor-mcu", "attrs": {} },
    { "id": "r1", "type": "litz-resistor", "attrs": { "value": "220" } },
    { "id": "led1", "type": "litz-led", "attrs": { "color": "green" } }
  ],
  "connections": [
    ["mcu:GPIO0", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "mcu:GND.1", "", []]
  ]
}
```

Header pins are addressed by name, and a repeated pin by `name.index`. The chip's own firmware image, if it has one, is named in the project's `litz.toml`; a board whose component carries its own program needs no `firmware` at all.

## Host calls a chip can make

The host interface is the whole point of a custom chip, and it is also the sandbox boundary. Nothing here reaches a real file, clock, socket or radio: bus transactions go to the parts the manifest attached, every network answer comes from the deterministic router the circuit places, and the packet medium is shared with the circuit's radio parts.

### `litz-mcu-1`

Version 0.1 imports two calls:

| Call | Shape | Meaning |
|---|---|---|
| `console` | `(bytes: list<u8>)` | Bytes the chip wrote to its console. The host routes them; the chip does not know where they end up. |
| `fault` | `(detail: string)` | A fault the host must report. The run stops. |

A 0.1 guest exchanges pin levels and console bytes and nothing else. Use it for a retro CPU or a device with no peripherals beyond GPIO.

### `litz-mcu-2`

Version 0.2 keeps those two and adds the groups below. The full WIT is in `docs/formats.md` section 10; this is the map of what is available and the shape of each call.

| Group | Call | Shape |
|---|---|---|
| analog | `analog-read` | `(pin: u8) -> option<u32>` — solved millivolts, or `none` when the pin floats or there is no analog solution |
| buses | `i2c-transfer` | `(controller: u8, address: u8, write: list<u8>, read-len: u32) -> result<list<u8>, string>` |
| buses | `spi-transfer` | `(controller: u8, chip-select: u8, tx: list<u8>, read-len: u32) -> result<list<u8>, string>` |
| buses | `uart-write` | `(port: u8, bytes: list<u8>) -> bool` — false when nothing is wired there |
| buses | `uart-read` | `(port: u8) -> list<u8>` — bytes a part has for the chip since the last call |
| Wi-Fi | `wifi-scan` | `() -> list<wifi-network>` (ssid, channel, rssi, secure, joined) |
| Wi-Fi | `wifi-join` | `(ssid: string, psk: option<string>) -> result<wifi-lease, string>` (ip, gateway, dns) |
| Wi-Fi | `wifi-leave` | `()` |
| Wi-Fi | `wifi-resolve` | `(name: string) -> result<list<u8>, string>` — four octets |
| sockets | `tcp-connect` | `(host: list<u8>, port: u16) -> result<u32, string>` — returns a flow id |
| sockets | `tcp-write` | `(id: u32, bytes: list<u8>) -> result<u32, string>` — bytes accepted |
| sockets | `tcp-read` | `(id: u32, max: u32) -> result<list<u8>, string>` — empty means nothing ready |
| sockets | `tcp-close` | `(id: u32)` |
| sockets | `udp-open` | `(port: u16) -> result<u32, string>` |
| sockets | `udp-send` | `(id: u32, host: list<u8>, port: u16, bytes: list<u8>) -> result<u32, string>` |
| sockets | `udp-read` | `(id: u32, max: u32) -> result<list<u8>, string>` |
| sockets | `udp-close` | `(id: u32)` |
| BLE | `ble-scan` | `() -> list<ble-advertisement>` (address, name, rssi, data, scan-response) |
| BLE | `ble-advertise` | `(address: list<u8>, data: list<u8>, scan-response: list<u8>) -> result<list<u8>, string>` — returns the address used |
| BLE | `ble-stop-advertising` | `(address: list<u8>)` |
| GATT | `ble-set-gatt` | `(services: list<gatt-service>) -> result<_, string>` |
| GATT | `ble-connect` | `(address: list<u8>) -> result<u16, string>` — returns a handle |
| GATT | `ble-disconnect` | `(handle: u16)` |
| GATT | `ble-discover` | `(handle: u16) -> result<list<gatt-service>, string>` |
| GATT | `ble-read` | `(handle: u16, service: u16, characteristic: u16) -> result<list<u8>, string>` |
| GATT | `ble-write` | `(handle: u16, service: u16, characteristic: u16, value: list<u8>) -> result<_, string>` |
| GATT | `ble-subscribe` | `(handle: u16, service: u16, characteristic: u16, enabled: bool) -> result<_, string>` |
| GATT | `ble-notifications` | `(handle: u16) -> list<gatt-notification>` |
| GATT | `ble-take-writes` | `() -> list<gatt-write>` |
| GATT | `ble-notify` | `(service: u16, characteristic: u16, value: list<u8>) -> result<_, string>` |
| radio | `radio-configure` | `(network: string, channel: u16, address: list<u8>) -> result<_, string>` |
| radio | `radio-send` | `(bytes: list<u8>, to: option<list<u8>>, airtime-ns: u64) -> result<_, string>` |
| radio | `radio-receive` | `() -> list<radio-frame>` (sender, to, bytes, sent-at, airtime) |

A few shapes carry meaning beyond their types. `analog-read` returning `none` is not 0 V; it means the pin is floating or the circuit has no solution. `i2c-transfer` clocks `write` first and then reads `read-len` bytes, so a plain write uses `read-len = 0`; an unpopulated SPI select reads `0xFF`, the floating-MISO level. `uart-write` returning false is the same information a floating TX pin gives the chip. `tcp-read` returning an empty list means "poll again", not end of file. BLE addresses are six octets, least significant first, and a GATT link is plain simulated state with no timing: connect, discover, read, write and notify answer immediately.

BLE and radio have a scope limit worth designing around. A runtime advertisement is heard by every other 0.2 station on the board and by no one else; a native esp32sim radio's peer list is fixed when its board boots, so it hears the circuit's beacon parts but not a later wasm advertisement. The same applies to GATT: `ble-connect` reaches circuit beacons and the tables other 0.2 stations register, not an esp32sim peer.

## Testing a chip

A chip is tested through the ordinary project harness: a `diagram.json`, a `litz.toml`, and `[[test]]` entries that assert console text and part state. For a board whose guest carries its own program, no firmware entry is needed.

The `litz-chip-conformance` crate is the executable floor under that. `Fixture` names the chip, its board diagram, a firmware image and what the run must produce; `check` builds a fresh board, boots the image twice and compares the console, pin level, stop reason, cycle count and part state. The builtin backends run it in `crates/litz-esp`, `crates/litz-avr` and `crates/litz-rp`; a community chip points at the same crate to show it clears the same floor.

```toml
[project]
diagram = "diagram.json"

[[test]]
name = "the guest prints and drives the LED"
timeout = "1s"
expect = "litz"
expect_state = "led1:on"
```

With the diagram above wiring `led1` to `mcu:GPIO0` through `r1`, this test passes when the guest emits `litz` on its console and drives chip GPIO 0 high. The scaffold's built-in program prints `litz` and halts; a guest that also drives a pad needs a program that does both. In the scaffold's own instruction set that is:

```text
1 'l'   1 'i'   1 't'   1 'z'   1 '\n'     # emit "litz\n"
2 0 1                                        # drive pin 0 high
0                                            # halt
```

`expect` matches merged console text (with an optional `<board>@` prefix when several boards are present), `fail` forbids a string, and `expect_state` asserts a part state at some point before the test ends. `state_at` samples one instant instead; `state_stays` holds a match over an interval. A test passes when every `expect` appeared, no `fail` appeared, and the run did not trap before `timeout` of simulated time.

Determinism is a contract, not a hope. The same component image and the same inputs produce byte-identical console output, part states and pin transitions on every host. There is no wall clock and no unseeded randomness; the guest's only time is the cycle count it reports, and host input lands at a simulated tick rather than racing the run. Two `litz test` runs of one project therefore produce the same transcript, and `litz test --json` produces the same records. A result is reproducible because `litz.lock` records the catalog manifests and the exact firmware bytes that produced it.

## Limits and troubleshooting

The component path is native-first. `litz-mcu-wasm` hosts a component with wasmtime, and that code is compiled only for non-wasm targets; the CLI, `litz run` and `litz test` use it. The in-memory Rust entry point `litz_engine::machine::boot` does not instantiate a wasm board on a native host; run a custom board from a project through the CLI, or use the JavaScript session. In the browser build the component is not hosted by wasmtime at all.

The browser path is jco. Transpile the component once and register a factory keyed by the manifest's module name:

```sh
npx jco transpile board-vendor-mcu.wasm -o out --instantiation sync
```

```js
import { readFileSync } from "node:fs";
import { loadLitz, chipFromJco, chipImports } from "@litz-sim/litz";
import { instantiate } from "./out/board-vendor-mcu.js";

const core = new WebAssembly.Module(readFileSync("./out/board-vendor-mcu.core.wasm"));
const litz = await loadLitz();
const session = litz.createSession({
  diagram,
  manifests: [{ toml: boardToml, schematic: onboardJson }],
  boards: {
    "board-vendor-mcu.wasm": (host) =>
      chipFromJco(instantiate(() => core, chipImports(host))),
  },
});
```

`chipFromJco` translates the exported chip interface, `chipImports(host)` supplies the host calls, and `host.call` forwards every 0.2 operation to the same circuit code the native runner uses. Both ABI versions are bridged there. A wrong component shape raises `TypeError`, and a failed `run` in the bridge becomes `{ cycles: 0, stop: "trap" }`.

Images and flash are the guest's declaration. `info()` states the flash layout and the memory windows; the host writes each image at the offset the guest gives for its kind. The manifest's `flash_size` is the board's size hint. A guest may accept ROM, bootloader and partition-table images it does not use — the scaffold does, so a `litz.toml` written for a real board still loads — but it should do so deliberately and must not pretend to implement a device it does not. A board whose component ships its own program can omit `firmware`; the host then calls `boot()` with whatever is already in flash.

Traps are the failure mode, not invented behaviour. A guest that ignores its budget or exceeds the memory ceiling (256 MiB) is stopped: a trap reached during `run` surfaces as `Stop::Trap` with the detail from `host.fault`, and the host meters each call with fuel so a runaway guest cannot hang a run. An unmapped debug read is an error from `read`, not a page of zeroes. Where a chip cannot implement a piece of hardware, it should report that through `load-image`'s error or `host.fault` rather than returning plausible values: a model that pretends is worse than one that stops loudly.

| Symptom | Usual cause | What to do |
|---|---|---|
| `does not instantiate as litz-mcu-2` | Built against the other world, or the output is a core module | Rebuild with `./simulator/build.sh` against `wit-v2/`, or set `abi` to the contract the guest was built for |
| `ABI "..." is not one this build understands` | Typo or an unknown version | Use `litz-mcu-1` or `litz-mcu-2` |
| `the onboard schematic has no $chip part` | Schematic written without the reserved entry | Add `{ "id": "chip", "type": "$chip", "attrs": {} }` |
| `wires $chip:X, which the board does not expose` | An onboard wire names a header pin the manifest does not declare | Add the `[[pins]]` entry or correct the wire |
| Browser session reports the component cannot be instantiated | A browser has no wasmtime | Transpile with jco and register `chipFromJco(instantiate(...))` |
| `has no firmware` | The board uses a builtin `chip`, not `[board.simulator]` | Name a wasm module, or add `[sim] firmware` |
| Guest traps immediately | A WASI import, or a loop that ignores the budget | Build for `wasm32-unknown-unknown` and return by the budget |
| A `read` of a missing address fails the run | By design: unmapped is an error | Map the window in `info().memory` or stop reading it |
| Board id collision or an unexpected board wins | Catalog load order | `boards/` is loaded after `parts/`; a project manifest replaces a builtin of the same id |

## How this stays stable

The manifest's `abi` field is the compatibility boundary, and the contract versions are additive. A new version is a new WIT package and a new `abi` value; the host keeps linking the old ones. `litz-mcu-wasm` already links both `0.1.0` and `0.2.0`, so a 0.1 guest written before 0.2 existed keeps loading and never sees a 0.2-only call. A future 0.3 will follow the same rule: it adds host calls and leaves the 0.1 and 0.2 worlds loadable.

Within a version, the exported `chip` interface is fixed and the host calls only grow. An author who builds for 0.2 can rely on every call in the host-call tables above, and an author who builds for 0.1 can ignore everything that came later. Because the manifest selects the version rather than the component's shape, a guest is never silently rebound to a different world: the tuple of module file and `abi` is what runs.

The catalog id is a separate promise. A diagram names an id and expects the same pins and behaviour; `version` is informational and nothing resolves against it. Never reuse an id for a different device and never change the meaning of a header pin. Use patch versions for fixes, minor for additive changes such as a new optional attribute, and treat a behaviour change as a new id. That keeps an installed board a stable dependency even as the component and the contract move forward.

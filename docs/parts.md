# Parts and boards

A part bundle is a directory that a project installs into its own catalog. Nothing is fetched and nothing is recompiled: the manifest is data, and a wasm backend is a file next to it.

A bundle describes a part (`[part]`) or a board (`[board]`). Both resolve to the same manifest model, so a diagram names either one with a `type` string.

## Bundle layout

A part bundle holds one manifest and every file the manifest names:

```
my-part/
  part.toml
  mirror.wasm     # only when [simulation].backend = "wasm"
```

`part.toml` is the conventional name. If it is absent, `litz catalog add` takes the only `<name>.toml` in the directory. A builtin-backed part needs no other file.

A board bundle is a directory of its own because its manifest, simulator component and onboard schematic belong together:

```
board-my-board/
  board.toml
  board-my-board.wasm
  board-my-board.onboard.json
  simulator/          # authoring source, not installed as a catalog entry
    Cargo.toml
    src/lib.rs
    build.sh
  wit-v2/litz-mcu.wit
```

Installation copies the manifest, the wasm modules the manifest names and, for a board, the onboard schematic. It does not copy the simulator source tree. `catalog add` writes to `--dir` (default `parts`): a part lands directly in that directory, and a board lands in `<dir>/<id>`. Use `--dir boards` for a board, which produces the conventional layout:

```
project/
  parts/
    part.toml
    mirror.wasm
  boards/
    board-my-board/
      board.toml
      board-my-board.wasm
      board-my-board.onboard.json
```

`litz catalog new board <id>` writes the whole authoring bundle, including the simulator source and the WIT contract, directly under `boards/<id>/`. `litz catalog add` is for installing a finished bundle from elsewhere.

## Discovery

`litz run`, `litz test` and `litz check` (or `lint`) start from the builtin catalog, then load `parts/`, then `boards/`, beside the project directory they are given. Each directory contributes its `*.toml` files and the manifests one level down, which is where an installed board bundle lives. Files are read in sorted order, and a later manifest with the same id replaces an earlier one; because `boards/` is loaded after `parts/`, a board wins a collision between the two. A project manifest whose id matches a builtin entry replaces that entry for the run. A `Cargo.toml` inside a bundle is not a manifest and is ignored.

The loader reads manifests only. Parsing never executes code.

## Part manifests

A part manifest is a `[part]` table, pins, optional attributes and buses, and a `[simulation]` table.

```toml
[part]
id = "vendor-mirror"
name = "Mirror"
version = "0.1.0"
description = "Output follows input"
license = "Apache-2.0"

[pins.IN]
kind = "digital-in"
number = 1

[pins.OUT]
kind = "digital-out"
number = 2

[attrs.color]
type = "enum"
values = ["red", "green", "blue"]
default = "red"

[simulation]
backend = "builtin"
engine = "led"
```

`[part]` fields:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | the catalog key a `diagram.json` names; immutable |
| `name` | yes | the label a picker shows |
| `version` | yes | semver text; informational |
| `description` | no | one line for a picker |
| `license` | no | SPDX expression for the bundle |

Pin fields:

| Field | Required | Meaning |
|---|---|---|
| `kind` | yes | the electrical role (see below) |
| `number` | no | package pin number for display |
| `repeat` | no | how many contacts share the name, default 1; must be at least 1 |
| `label` | no | text shown next to the pin |

A pin name may not be empty and may not contain `:`. Pin kinds are `digital-in`, `digital-out`, `digital-io`, `analog-in`, `analog-out`, `analog-io`, `passive`, `power`, `ground`, `nc`, and the bus kinds `i2c-sda`, `i2c-scl`, `spi-mosi`, `spi-miso`, `spi-sck`, `spi-cs`, `uart-tx`, `uart-rx`. The bus kinds tell the flattener which wires carry a bus; they are otherwise ordinary pins.

Attribute fields: `type` is `bool`, `int`, `float`, `string` or `enum`. `values` is for an enum only and must be non-empty; a non-enum that lists values is refused. `default` must match the type and, for an enum, be one of `values`; an integer default is accepted for a float attribute. A part must declare at least one pin.

### Buses

A part that answers bus transactions declares at most one bus section:

```toml
[buses.i2c0]
address = 0x76        # 7-bit address, 0x08..=0x7f

[buses.spi0]
cs = "CS"             # the spi-cs pin that selects the part

[buses.uart0]
port = 0              # must agree with the name's index
```

The section name carries the kind and controller index (`i2c0`, `spi1`, `uart0`), and each kind takes only its own field: an I2C section refuses `cs` and `port`, an SPI section refuses `address` and `port`, and a UART section refuses `address` and `cs`. An I2C address must be usable as a 7-bit target; an SPI `cs` names one of the part's pins; a UART `port` must equal the index in the section name. The bus pins themselves are wired by the diagram to the board.

## Board manifests

A board manifest is a `[board]` table and a list of header pins. It has no `[attrs]` or `[buses]` table, and exactly one of `chip` and `[board.simulator]` provides the MCU.

```toml
[board]
id = "board-vendor-mcu"
name = "Vendor MCU"
version = "0.1.0"
description = "A board whose MCU is a wasm component"
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
functions = ["uart"]
```

`[board]` fields:

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | the catalog key a `diagram.json` names; immutable |
| `name` | yes | the label a picker shows |
| `version` | no | semver text, default `0.0.0` |
| `description` | no | one line for a picker |
| `chip` | one of `chip` / `simulator` | a builtin chip backend id such as `esp32c3` |
| `simulator` | one of `chip` / `simulator` | a wasm MCU component |
| `flash_size` | no | size text such as `4MB`, parsed by the runner |
| `psram` | no | boolean, default `false` |
| `schematic` | no | file name of the onboard schematic, next to the manifest |

`[board.simulator]` fields:

| Field | Required | Meaning |
|---|---|---|
| `kind` | yes | `wasm`; the only kind today |
| `module` | yes | module path relative to the manifest; no absolute path and no `..` |
| `abi` | no | `litz-mcu-1` (default, `litz:mcu@0.1.0`) or `litz-mcu-2` (`litz:mcu@0.2.0`) |

`[[pins]]` fields:

| Field | Required | Meaning |
|---|---|---|
| `name` | yes | unique within the board |
| `gpio` | one of `gpio` / `fixed` | GPIO number on the chip |
| `fixed` | one of `gpio` / `fixed` | `gnd`, `3v3`, `5v`, or `vin` |
| `repeat` | no | how many contacts share the name, default 1; must be at least 1 |
| `label` | no | text shown next to the pin |
| `functions` | no | advertised alternate functions such as `adc`, `pwm`, `i2c`, `uart`, `spi` |

A `gpio` pin becomes a `digital-io` header pin; a `fixed = "gnd"` pin becomes `ground`; any other fixed net becomes `power`. A board whose `chip` is not a builtin backend parses but fails `litz board check` and cannot boot; the message points at `[board.simulator]`. A board manifest resolves to the same manifest model a part does, with no attributes and no buses.

`litz board check <directory>` checks a board bundle before a project depends on it. It accepts the bundle directory or its manifest path, resolves the module and schematic relative to the manifest, and instantiates a wasm simulator under the ABI the manifest names. When the manifest names a schematic, the schematic must contain a `$chip` part. A builtin chip id is validated against the runner's builtin set instead. The report names the id, version and name, the chip or module, the component's own name and CPU rate when it can be read, the header pin count and the schematic wire count.

## Builtin catalog

The builtin catalog is compiled into the binary. It contains the boards listed below and the parts whose simulation is either a builtin engine or one of three builtin wasm components.

The engine names below are the values a part manifest writes in `[simulation].engine`. One engine can back several catalog ids; the id-to-engine mapping is in the manifests under `crates/litz-catalog/builtin/parts/`.

| Group | Engines |
|---|---|
| Pin-level | `4017`, `4051`, `4094`, `74hc138`, `74hc139`, `74hc164`, `74hc165`, `74hc166`, `74hc238`, `74hc573`, `a4988`, `analog`, `analog-joystick`, `analog-sensor`, `battery`, `bjt`, `buzzer`, `capacitor`, `cd4067`, `clock-generator`, `dc-motor`, `dht22`, `diode`, `dip-switch`, `dip-switch-8`, `dpdt-relay`, `ds18b20`, `flame-sensor`, `flex-sensor`, `force-sensor`, `fuse`, `gas-sensor`, `gate`, `hall-sensor`, `hc-sr04`, `hd44780`, `heart-beat-sensor`, `hx711`, `inductor`, `ir-emitting-diode`, `ir-receiver`, `l293d`, `led`, `led-bar-graph`, `level-shifter`, `lm393`, `max7219-single`, `membrane-keypad`, `mosfet`, `neopixel`, `ntc-temperature-sensor`, `opamp`, `photoresistor-sensor`, `pir-motion-sensor`, `potentiometer`, `pushbutton`, `pushbutton-latching`, `reed-switch`, `relay`, `resistor`, `rgb-led`, `rotary-encoder`, `segments`, `segments-4digit`, `servo`, `signal-generator`, `slide-potentiometer`, `slide-switch`, `stepper-motor`, `tcrt5000`, `tilt-switch`, `tm1637`, `traffic-light`, `uln2003`, `vibration-motor`, `voltage-source` |
| I2C device | `ads111x`, `adxl345`, `apds9960`, `bme280`, `bme680`, `bmp280`, `bh1750`, `ds1307`, `ds3231`, `hd44780-i2c`, `ina219`, `lm75`, `mcp23017`, `mcp4725`, `mlx90614`, `mpu6050`, `pca9685`, `pcf8574`, `pcf8591`, `qmc5883l`, `sgp30`, `sh1106`, `sht31`, `ssd1306`, `tca9548a`, `tcs34725`, `tmp102`, `tsl2561`, `veml7700`, `vl53l0x`, `vl53l1x` |
| SPI device | `epaper`, `ili9341`, `max7219`, `mcp3008`, `microsd`, `pcd8544`, `rfid-rc522`, `st7735`, `w25q32`, `xpt2046` |
| UART peer | `fingerprint`, `gps-neo6m`, `pms5003` |
| Radio, network and Bluetooth | `at86rf233`, `ble-beacon`, `nrf24l01`, `router`, `sx1276` |

A part that declares `[buses.*]` is built as that bus device and gets a passive pin shell, so its power and ground pins still join the netlist without a second driver touching a rail. An engine in the wrong group is a manifest error: a part cannot be an LED and an I2C device at the same time. An engine name the catalog accepts but this build does not implement is refused when the board is built.

The `voltage-source` engine backs `litz-power-supply` and `litz-battery`. The bench supply is adjustable while the run is live: `set_named_control(part, "voltage", "3.3")` moves its output, and `"current"` sets the limit in amperes (`0` turns the limit off). A scenario writes the same call as `set-control: { part-id: pwr, name: voltage, value: "3.3" }`, and the JavaScript session exposes it as `setNamedControl`. When the load would draw more than the limit, the analog solve stops holding the set voltage and delivers the limit instead, so the terminal sags to whatever the load takes at that current; `analog_solve().current(part)` reports the element current, negative when the supply is delivering. The battery keeps its fixed `voltage` attribute and has no limit.

Three builtin parts are wasm components rather than engines:

| Part id | Module | ABI |
|---|---|---|
| `litz-74hc595` | `74hc595.wasm` | `litz-part-1` |
| `litz-24c02` | `24c02.wasm` | `litz-part-2` |
| `litz-rgb-panel` | `display.wasm` | `litz-part-2` |

The builtin boards and their MCUs:

| Board id | MCU |
|---|---|
| `board-arduino-uno` | `atmega328p` |
| `board-arduino-nano` | `atmega328p` |
| `board-arduino-pro-mini` | `atmega328p` |
| `board-esp32-c3-devkitm-1` | `esp32c3` |
| `board-esp32-c6-devkitc-1` | `esp32c6` |
| `board-esp32-c6-devkitm-1` | `esp32c6` |
| `board-esp32-devkit-c-v4` | `esp32` |
| `board-esp32-p4-function-ev` | `esp32p4` |
| `board-esp32-s3-devkitc-1` | `esp32s3` |
| `board-esp32-s3-devkitc-1-n16r8` | `esp32s3` |
| `board-esp32-s31-coreboard` | `esp32s31` |
| `board-litz-rv32` | wasm component, `litz-mcu-1` |
| `board-raspberry-pi-pico` | `rp2040` |

The builtin catalog's per-part attributes and behaviour are described in [formats.md](formats.md) for the models that document covers.

## Backends

| Backend | Required fields | What runs |
|---|---|---|
| `builtin` | `engine` | a model compiled into the runner, selected by name |
| `wasm` | `module`; `abi` optional | a WebAssembly component implementing the part contract |

A wasm part names a module relative to the manifest. The path may not be absolute and may not contain `..`. `abi` defaults to `litz-part-2`; `litz-part-1` is still accepted. The module runs sandboxed: no filesystem, network, clock or randomness, with a 16 MiB memory ceiling and a fuel budget per call, so a bundle from someone else cannot reach the host.

## Custom wasm parts and boards

The native runner instantiates a component with wasmtime. The JavaScript package cannot instantiate a component directly, so a custom part or board must be transpiled with jco and registered with `createSession`:

```sh
npx jco transpile 74hc595.wasm -o part --instantiation sync
```

`createSession` accepts `manifests`, an array of catalog manifests as TOML text. A board that names an onboard schematic passes `{ toml, schematic }` instead of a bare string. `parts` and `boards` map the module name from the manifest to a factory; the factory is called once per instance while the board is built. `partFromJco` adapts a transpiled part, `chipFromJco` adapts a transpiled MCU, `partImports()` supplies the part host imports, and `chipImports(host)` supplies the MCU host calls. A board factory receives the guest's host object:

```js
import { readFileSync } from "node:fs";
import { loadLitz, partFromJco, partImports, chipFromJco, chipImports } from "@litz-sim/litz";
import { instantiate as instantiatePart } from "./part/74hc595.js";
import { instantiate as instantiateChip } from "./chip/litz-rv32.js";

const partCore = new WebAssembly.Module(readFileSync("./part/74hc595.core.wasm"));
const chipCore = new WebAssembly.Module(readFileSync("./chip/litz-rv32.core.wasm"));
const litz = await loadLitz();

const session = litz.createSession({
  diagram,
  app,
  manifests: [partToml, { toml: boardToml, schematic: onboardJson }],
  parts: { "74hc595.wasm": () => partFromJco(instantiatePart(() => partCore, partImports())) },
  boards: { "litz-rv32.wasm": (host) => chipFromJco(instantiateChip(() => chipCore, chipImports(host))) },
});
```

The JavaScript side instantiates each transpiled component and answers its host calls through `chipImports` in `js/src/jco.js` and the `litz_host` bridge. The Rust side implements `HostPart` and `HostChip`; the `litz:mcu@0.2.0` host calls are answered by `crates/litz-mcu-wasm/src/host.rs`, which is wasmtime-free and reached from the wasm build through `litz_guest_call` in `crates/litz-wasm`. The same board description therefore runs in a browser, under Node, or through the native wasmtime path.

The package boots every builtin chip a diagram's board names: ESP32-C3 (`esp32c3`), ESP32-C6 (`esp32c6`), ESP32-P4 (`esp32p4`) and ESP32-S31 (`esp32s31`) on RISC-V, ATmega328P (`atmega328p`) and RP2040 (`rp2040`). The classic ESP32 (`esp32`) and ESP32-S3 (`esp32s3`) are Xtensa; their simulator executes guest blocks through a host JIT, which the wasm build does not carry, so `createSession` reports that instead of booting them. A custom chip of either ABI version is bridged in full: images, pins, console and fault, and for `litz:mcu@0.2.0` the analog, bus, Wi-Fi, socket and BLE host calls. The only chips the wasm build excludes are the two Xtensa parts above.

## The part and MCU ABIs

### Part ABI

A `backend = "wasm"` part is a WebAssembly component. The world is named `component` and exports one `part` interface. Both ABI names below are part of the contract surface, and the host detects the version from the component's exported interface rather than from the manifest alone.

| Manifest `abi` | WIT package | Export surface |
|---|---|---|
| `litz-part-1` | `litz:part@0.1.0` | `info`, `set-pin`, `advance`, `next-event`, `drive`, `state`, `press`, `set-control` |
| `litz-part-2` | `litz:part@0.2.0` | every 0.1 export, plus `analog`, `attributes`, `set-attribute`, `i2c-connect`, `i2c-read`, `i2c-write`, `i2c-stop`, `spi-select`, `spi-transfer`, `uart-receive`, `uart-poll`, `framebuffer`, `draw`, `damage` |

The current WIT is [crates/litz-part-wit/wit/litz-part.wit](https://github.com/Litz-Sim/litz/blob/main/crates/litz-part-wit/wit/litz-part.wit); the unchanged 0.1 package is [crates/litz-part-wit/wit-legacy/litz-part.wit](https://github.com/Litz-Sim/litz/blob/main/crates/litz-part-wit/wit-legacy/litz-part.wit). Pins are `u16` indices into the part's own pin table, assigned once when the board is flattened, and time is `u64` nanoseconds since the simulation started. There is no WASI: 0.1 has no host imports, and 0.2 adds only `log` and `rom`.

A 0.1 module keeps running against the 0.2 host. The 0.2-only calls are answered with their empty defaults, so a pins-only guest never has to implement the bus, attribute or framebuffer groups.

### MCU ABI

The MCU contract has the same naming rule:

| Manifest `abi` | WIT package | World |
|---|---|---|
| `litz-mcu-1` | `litz:mcu@0.1.0` | `simulator` |
| `litz-mcu-2` | `litz:mcu@0.2.0` | `simulator` |

`litz-mcu-1` is the default when a board manifest omits `abi`. The current WIT is [crates/litz-mcu-wit/wit-v2/litz-mcu.wit](https://github.com/Litz-Sim/litz/blob/main/crates/litz-mcu-wit/wit-v2/litz-mcu.wit); the 0.1 package is [crates/litz-mcu-wit/wit/litz-mcu.wit](https://github.com/Litz-Sim/litz/blob/main/crates/litz-mcu-wit/wit/litz-mcu.wit). A board built from the template uses `litz-mcu-2`.

## State codes

A part's `state()` returns a `PartState`, and the wasm package exposes it as one integer code plus a numeric value. The codes are identical in the Rust ABI and the JavaScript wrapper:

| Code | Kind | State word | Value |
|---|---|---|---|
| `0` | `none` | `none` | 0 |
| `1` | `led` | `off` | 0 |
| `2` | `led` | `on` | 0 |
| `3` | `button` | `up` | 0 |
| `4` | `button` | `down` | 0 |
| `5` | `switch` | `off` | 0 |
| `6` | `switch` | `on` | 0 |
| `7` | `tone` | `silent` | `hz` |
| `8` | `tone` | `sounding` | `hz` |
| `9` | `servo` | `position` | degrees |
| `10` | `segments` | `mask` | segment mask |
| `11` | `knob` | `position` | permille |
| `12` | `radio` | `state` | 0 |

`-1` means the diagram has no part with that id, and is not a `PartState`. `litz_part_state` returns the code and `litz_part_value` returns the number behind it. The JavaScript wrapper maps the code to the `kind` and `state` words above.

## Versioning

The id is the promise. A diagram names an id and expects the same device with the same pins and behaviour forever. `version` is informational: nothing resolves or pins a version, and a run does not compare it.

Use semver for the reader's benefit: patch for fixes, minor for additive changes such as a new optional attribute, major for behaviour changes. Never reuse an id for a different device and never change the meaning of a pin.

## Licensing

A bundle is distributed by whoever wrote it, not by Litz. Put an SPDX expression in `[part].license`, keep the bundle's own license file, and check the license before installing a bundle from a third party. `litz catalog add` copies files; it does not inspect or enforce licenses.

## Commands

`litz catalog` and `litz catalog --json` print the builtin catalog. `litz catalog list` and `litz catalog check` take a project directory. `litz catalog add`, `litz catalog new` and `litz catalog remove` act on the project in the current directory. `--dir` chooses the catalog directory, `parts` by default; use `--dir boards` for a board. A board made with `new board` defaults to `boards`.

### Print the builtin catalog

```sh
litz catalog
```

```
id                                    kind   name                             pins
board-arduino-uno                     board  Arduino Uno                        24
...
litz-led                              part   LED                                  2
```

The `pins` column counts distinct pin names, not repeated contacts. `litz catalog --json` prints an array with one object per entry, with keys `id`, `kind`, `name`, `description`, `pins` and `attrs`.

### List a project

```sh
litz catalog list .
```

```
id                    kind   name              pins  source
litz-led              part   LED                  2  builtin
vendor-mirror         part   Mirror               2  parts/
```

The `source` column is `builtin`, `parts/`, `boards/`, or `project` for a manifest inside a board bundle's own directory. Rows are sorted by id.

### Check manifests

```sh
litz catalog check .
```

```
1 manifest ok
```

`check` reads only the project's `parts/` and `boards/` manifests, one level down, and does not count builtin entries. A missing directory is empty, not an error. A manifest that does not parse prints one line naming the file and exits 1:

```
parts/bad.toml: missing field `version`
```

### Install a bundle

```sh
litz catalog add ../vendor-mirror
```

```
installed vendor-mirror at /project/parts/part.toml
```

`add` validates the whole bundle before writing anything: the manifest parses, the id is neither builtin nor already installed, every referenced module and schematic is a relative path inside the bundle, and every referenced file exists. A bad bundle or an id collision leaves the destination untouched, and a write error rolls back the files this call created.

### Scaffold a part manifest

```sh
litz catalog new vendor-switch --name "Vendor Switch"
```

```
created /project/parts/vendor-switch.toml
```

The file declares two passive pins and leaves the backend commented out. Uncomment one `[simulation]` block, choose the engine or module, then run `litz catalog check .`. `new` refuses to overwrite an existing file and refuses an id that is not a plain file name.

### Scaffold a board bundle

```sh
litz catalog new board vendor-mcu --name "Vendor MCU"
```

```
created /project/boards/board-vendor-mcu
  next: cd /project/boards/board-vendor-mcu && ./simulator/build.sh
        litz board check .
```

The id gains the `board-` prefix when it does not already have one. The directory contains `board.toml`, `<id>.onboard.json`, `simulator/Cargo.toml`, `simulator/src/lib.rs`, `simulator/build.sh`, `wit-v2/litz-mcu.wit` and `README.md`. The generated manifest uses `litz-mcu-2`. `simulator/build.sh` needs `litz-componentize` on `PATH`; from a Litz checkout, install it with `cargo install --path crates/litz-mcu-wasm --features tooling`. The script compiles for `wasm32-unknown-unknown` and writes `<id>.wasm` next to `board.toml`. [Writing a chip](chips.md) covers the rest of the contract: the `$chip` schematic, the ABI choice, the host calls and the component build.

### Remove a part or board

```sh
litz catalog remove vendor-switch
```

```
removed vendor-switch (/project/parts/vendor-switch.toml)
```

`remove` deletes the manifest and the wasm modules it alone names; a module another manifest in the catalog still references is kept. A board's schematic is removed with it, and a board bundle's directory is removed once it is empty. Builtin ids are refused: no project file owns them.

## Worked example: a two-pin builtin part

A green LED on the `led` engine. Create the directory and manifest:

```sh
mkdir -p vendor-led
```

```toml
# vendor-led/vendor-led.toml
[part]
id = "vendor-led"
name = "Vendor LED"
version = "1.0.0"
description = "5 mm green LED"
license = "Apache-2.0"

[pins.A]
kind = "digital-in"
number = 1
label = "anode"

[pins.C]
kind = "ground"
number = 2

[attrs.color]
type = "enum"
values = ["green"]
default = "green"

[simulation]
backend = "builtin"
engine = "led"
```

Install and check:

```sh
litz catalog add vendor-led
litz catalog check .
litz catalog list .
```

```
installed vendor-led at /project/parts/vendor-led.toml
1 manifest ok
id          kind  name        pins  source
vendor-led  part  Vendor LED     2  parts/
```

The diagram may now use `"type": "vendor-led"` wherever `litz-led` is allowed.

## Worked example: a wasm part

Start from `crates/litz-cli/templates/part/`, a buildable part whose `OUT` pin mirrors its `IN` pin. Change the id and name in `part.toml` and in `src/lib.rs`, then build:

```sh
crates/litz-cli/templates/part/build.sh
```

The script compiles the crate for `wasm32-unknown-unknown` and writes the wrapped component next to the manifest:

```
crates/litz-cli/templates/part/part.toml
crates/litz-cli/templates/part/part.wasm
```

The manifest it ships is:

```toml
[part]
id = "litz-part-example"
name = "Example mirror"
version = "0.1.0"
description = "A two-pin part whose output follows its input"
license = "Apache-2.0"

[pins.IN]
kind = "digital-in"
number = 1
label = "input"

[pins.OUT]
kind = "digital-out"
number = 2
label = "output"

[simulation]
backend = "wasm"
module = "part.wasm"
abi = "litz-part-2"
```

From the project that should receive it:

```sh
litz catalog add crates/litz-cli/templates/part
litz catalog check .
```

In a project with no other manifest, `check` reports:

```
installed litz-part-example at /project/parts/part.toml
1 manifest ok
```

`add` copies `part.toml` and `part.wasm` together, so the manifest keeps resolving its module. The guest itself is ordinary Rust against `litz-part-sdk`; adjust `src/lib.rs` for real behaviour and rerun `build.sh` before reinstalling.

A part does not have to be written by hand. `examples/ym2610/` builds one from Verilog RTL with Verilator, `wit-bindgen c` and the same `litz:part` contract; [Writing a chip](chips.md#when-the-chip-already-exists-as-verilog) covers that pipeline.

## Worked example: a wasm board

Create a board bundle in the project, build its component, and check it before a diagram uses it:

```sh
litz catalog new board vendor-mcu --name "Vendor MCU"
cd boards/board-vendor-mcu
./simulator/build.sh
litz board check .
cd ../..
litz catalog check .
```

The generated `board.toml` names `board-vendor-mcu.wasm` and `board-vendor-mcu.onboard.json`, both beside it. `litz board check .` prints the id, version and name, the module and ABI, the component's own name and CPU rate, the header pin count and the schematic wire count. The board is already in the project's catalog, so a diagram may name `"type": "board-vendor-mcu"` without a separate `catalog add`. To move the built board to another project, copy the directory into that project's `boards/`, or run `litz catalog add /path/to/board-vendor-mcu --dir boards` from the other project.

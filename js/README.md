# @litz-sim/litz

`@litz-sim/litz` is the JavaScript binding for Litz, a deterministic electronics and firmware simulator. The package loads `litz_wasm.wasm`, opens a session on one board from a `diagram.json` and an application image, and exposes serial output, part state, pins, probes, decoders, displays and electrical conflicts as plain objects. It has no DOM code, no workers and no runtime dependencies, and it advances only when the host calls `run` or `advance`.

## Install

```sh
npm install @litz-sim/litz
```

The package is an ES module for Node 18 or newer and for browsers. It ships `index.js`, `index.d.ts`, `src/` and the compiled `litz_wasm.wasm`, about 5.5 MB uncompressed.

## First session

`loadLitz` reads, compiles and instantiates the module and returns a `Litz`. `createSession` opens one board, selected by the board named in the diagram. `advance` moves simulated time, `serial.take` drains the console and `close` releases the session. The sample below reads the repository's `blinky` example. In a checkout, `npm run example` runs it from `js/examples/blinky.mjs`, where Node resolves the package name through the enclosing `js/package.json`. In another project, install the package and pass that project's diagram and image.

```js
import { readFile } from "node:fs/promises";
import { loadLitz } from "@litz-sim/litz";

const litz = await loadLitz();
const session = litz.createSession({
  diagram: await readFile("../examples/blinky/diagram.json", "utf8"),
  app: new Uint8Array(await readFile("../examples/blinky/firmware/blinky.bin")),
});

session.advance(300);
process.stdout.write(session.serial.take());
console.log(session.chip().name, session.ms, session.stop);
session.close();
litz.close();
```

`run(steps)` executes at most `steps` scheduling steps and returns the simulated clock in milliseconds. A step is a unit of the scheduler rather than a fixed span of time, so `advance(ms)` repeats `run` in chunks until the clock reaches the target. Both calls are synchronous and bounded.

## Session surface

| Member | Kind | Description |
|---|---|---|
| `run(steps?)` | method | Advance at most `steps` scheduling steps; returns simulated milliseconds. Default `DEFAULT_STEPS`. |
| `advance(ms, options?)` | method | Advance until the clock has moved `ms` milliseconds; returns the clock. Throws when `maxSteps` is exhausted. |
| `ms` | getter | Simulated milliseconds since reset. |
| `chip()` | method | The booted backend: `{ name, cpuHz, flash }`. |
| `stop` | getter | How the last `run` ended: `budget`, `idle`, `halted`, `trap`, `reset` or `breakpoint`. |
| `fault` | getter | The backend's trap text, or `null`. |
| `heap()` | method | The firmware's free heap in bytes, or `null` when it cannot be asked. |
| `serial.take()` | method | Drain the console since the last call; returns UTF-8 text. |
| `serial.write(data)` | method | Deliver a string or bytes to the board's UART0 receiver. |
| `trace.start()` | method | Begin recording net transitions. |
| `trace.transitions` | getter | Number of transitions recorded so far. |
| `trace.vcd()` | method | The recording as a VCD document. |
| `probe.add(name, point)` | method | Watch `<board>:<GPIO>` or `<part>:<pin>`; samples appear in `scope()`. |
| `decode.add(spec)` | method | Add a UART, I2C or SPI decoder over probes. |
| `scope()` | method | Every probe's samples and every decoder's frames. |
| `parts()` | method | Every part the diagram declares, with current state. |
| `part(id)` | method | One part by id, including board-expanded ids such as `esp/led_pwr`; `null` when absent. |
| `press(id, pressed?)` | method | Press or release a pushbutton; defaults to pressed. |
| `setControl(id, permille)` | method | Move a control through 0 to 1000 permille of its travel. |
| `setNamedControl(id, name, value)` | method | Set a named control such as a sensor's `temperature` or `lux`. |
| `partPin(id, pin?)` | method | The level on a part pin, or `null`; the first pin when `pin` is omitted. |
| `gpio(pin)` | method | The level the chip sees on one of its own pads, by GPIO number. |
| `display()` | method | The panel, or `null` when the circuit has none. |
| `touch(x, y, down?)` | method | Deliver a touch at panel pixels; `false` without a panel. |
| `conflicts()` | method | Electrical conflicts, oldest first. |
| `handle` | getter | The handle inside the module; `0` is never a session. |
| `close()` | method | Drop the session and release its module memory. |

`Display` carries `width`, `height`, a `version` that grows when pixels change, and `pixels()`, which returns RGBA8888 bytes sized `width * height * 4`.

## Session options

| Field | Type | Default | Description |
|---|---|---|---|
| `diagram` | `string` | required | The `diagram.json` text. Its board selects the chip. |
| `app` | `Uint8Array \| ArrayBuffer \| ArrayBufferView` | required | The application image. |
| `rom` | bytes or `null` | `null` | Mask ROM for firmware that needs one. |
| `boot` | bytes or `null` | `null` | Second-stage bootloader; the module accepts it only with `ptable`. |
| `ptable` | bytes or `null` | `null` | Partition table; the module accepts it only with `boot`. |
| `flashSize` | `number` or `null` | board manifest, then module default | Flash size in bytes; must be a positive whole number. |
| `mac` | `string` or `null` | module default | The station MAC, for example `24:0a:c4:12:34:56`. |
| `analog` | `boolean` | `false` | Solve the analog network instead of treating every net as digital. |
| `manifests` | `Array<string \| { toml, schematic }>` | `[]` | Catalog manifests as TOML text; a board with an internal schematic uses `{ toml, schematic }`. |
| `parts` | `Record<string, () => HostPart>` | `{}` | Wasm part factories keyed by the module name in the part manifest. |
| `boards` | `Record<string, (host) => HostChip>` | `{}` | Wasm chip factories keyed by the module name in the board manifest. |

## Chips and firmware images

The board named in `diagram.json` selects the backend. The shipped module boots ESP32-C3, ESP32-C6, ESP32-P4 and ESP32-S31 (RISC-V), the ATmega328P on Arduino Uno, Nano and Pro Mini boards, and the RP2040 on Raspberry Pi Pico boards. The classic ESP32 and ESP32-S3 run on Xtensa, whose simulator executes guest blocks through a host JIT that this module does not carry; `createSession` throws a `LitzError` that names the chip and the supported alternatives.

`app` is the application image. `rom` adds a mask ROM. `boot` and `ptable` add the second-stage bootloader and partition table and are supplied together. A merged ESP image is recognised and written at the bootloader offset; a bare application is written at the chip's application offset. `chip().flash` reports where the images landed.

## Custom parts and boards with jco

A catalog part or board can be a WebAssembly Component. Browsers cannot instantiate a component directly, so transpile it once with [jco](https://github.com/bytecodealliance/jco):

```sh
npx jco transpile 74hc595.wasm -o out --instantiation sync
```

The generated JavaScript exports `instantiate(getCoreModule, imports)`. Pass the generated modules to `partFromJco` with `partImports`, or to `chipFromJco` with `chipImports`. The part sample below uses the shipped `74hc595` fixture; save it as `demo-part.mjs` in `js/` and run it from that directory.

```js
import { readFile } from "node:fs/promises";
import { loadLitz, partFromJco, partImports } from "@litz-sim/litz";
import { instantiate } from "./test/fixtures/74hc595/74hc595.js";

const core = new WebAssembly.Module(await readFile("./test/fixtures/74hc595/74hc595.core.wasm"));
const diagram = JSON.stringify({
  version: 1,
  parts: [
    { id: "esp", type: "board-esp32-c3-devkitm-1", attrs: {} },
    { id: "sh", type: "litz-74hc595", attrs: {} },
    { id: "r1", type: "litz-resistor", attrs: { value: "220" } },
    { id: "led1", type: "litz-led", attrs: { color: "green" } },
  ],
  connections: [
    ["esp:GPIO8", "sh:SHCP", "", []],
    ["esp:GPIO8", "sh:STCP", "", []],
    ["esp:3V3", "sh:DS", "", []],
    ["esp:3V3", "sh:MR", "", []],
    ["esp:GND.1", "sh:OE", "", []],
    ["sh:Q0", "r1:1", "", []],
    ["r1:2", "led1:A", "", []],
    ["led1:C", "esp:GND.1", "", []],
  ],
});

const litz = await loadLitz();
const session = litz.createSession({
  diagram,
  app: new Uint8Array(await readFile("../examples/blinky/firmware/blinky.bin")),
  parts: {
    "74hc595.wasm": () => partFromJco(instantiate(() => core, partImports())),
  },
});

session.advance(500);
console.log(session.part("led1"));
session.close();
litz.close();
```

The factory is called once per part instance while the board is built, and the session keeps the instance until it is closed. `parts` and `boards` are keyed by the module name in the part or board manifest. A chip factory receives the guest's host object:

```js
import { readFileSync } from "node:fs";
import { chipFromJco, chipImports } from "@litz-sim/litz";
import { instantiate } from "./test/fixtures/board-v2/board-v2.js";

const cores = new Map();
const getCoreModule = (name) => {
  if (!cores.has(name)) {
    cores.set(name, new WebAssembly.Module(readFileSync(`./test/fixtures/board-v2/${name}`)));
  }
  return cores.get(name);
};

const boards = {
  "board-v2.wasm": (host) => chipFromJco(instantiate(getCoreModule, chipImports(host))),
};
```

Pass `boards` with the board manifest in `manifests`; [the JavaScript reference](../docs/javascript.md#a-custom-litzmcu020-chip) has a complete bootable example.

## The `litz:mcu` bridge

Both `litz:mcu@0.1.0` and `litz:mcu@0.2.0` chips are bridged in full. The 0.1 contract covers images, pins, console and fault. The 0.2 contract adds analog reads, I2C, SPI and UART transactions, Wi-Fi scan, join and resolve with TCP and UDP, BLE scanning, advertising and GATT, and the raw radio. Every 0.2 call reaches the circuit through the module's `litz_guest_call` export, answered by the same host state the native runner uses. `chipImports(host)` turns each contract function into a `host.call(op, args)` request where `op` is the contract's camelCase name and `args` is a plain JSON object; a call the circuit refuses throws `LitzError`.

## Errors

Every failing loader or session call throws `LitzError`. The message is the reason the module gave, so a bad diagram reports the parse error and a bad pin reports the pin. `LitzError` extends `Error`, sets `name` to `"LitzError"` and accepts an `options` object with a `cause`.

```js
import { loadLitz, LitzError } from "@litz-sim/litz";

const litz = await loadLitz();
try {
  litz.createSession({ diagram: "{}", app: new Uint8Array([1, 2, 3, 4]) });
} catch (error) {
  if (error instanceof LitzError) console.error(error.message);
} finally {
  litz.close();
}
```

`partFromJco` and `chipFromJco` throw `TypeError` when the transpiled module does not export the expected interface. A factory that returns the wrong shape, or that throws, is reported as a `LitzError` naming the module during `createSession`.

## Loading the module

`loadLitz` accepts bytes, a URL, a `Response` or an already compiled `WebAssembly.Module`. Without `wasm` it reads the file shipped beside the package, exported as `wasmUrl`. A relative string is resolved against the loader's own module URL, so pass an absolute URL or bytes when the working directory differs.

```js
import { readFile } from "node:fs/promises";
import { loadLitz, wasmUrl } from "@litz-sim/litz";

const bytes = new Uint8Array(await readFile(new URL(wasmUrl)));
const fromBytes = await loadLitz({ wasm: bytes });
const fromModule = await loadLitz({ wasm: new WebAssembly.Module(bytes) });
const fromUrl = await loadLitz({ wasm: wasmUrl });

fromBytes.close();
fromModule.close();
fromUrl.close();
```

`imports` is merged into the module's import object. The `litz_host` namespace holds this package's part and chip bridge and is merged with the built-in runtime; other namespaces are passed through. The shipped module imports only `litz_host`.

## Browser and Node

In Node, the default source is a `file:` URL and the loader reads it with `node:fs/promises`. In a browser, the default source is fetched from the package URL; pass a `Response` or an absolute URL when the asset lives elsewhere. Bundlers that do not copy arbitrary assets can read the wasm themselves and pass `wasm` as bytes or a compiled module. The package uses standard web APIs and, on Node, `node:fs/promises` for `file:` sources; it contains no DOM access.

## Building and testing

`npm run build` compiles `crates/litz-wasm` for `wasm32-unknown-unknown`, copies `litz_wasm.wasm` next to `index.js`, and checks that the module imports only `litz_host` and exports every name the wrapper requires. The Rust target must be installed in the toolchain. `npm test` runs `node --test` over `test/*.test.js` against the shipped module and the repository's example firmware.

```sh
npm run build
npm test
```

## Reference

The full API reference, including every type, lifecycle rule, host import name and a complete custom `litz:mcu@0.2.0` chip, is in [docs/javascript.md](../docs/javascript.md).

## License

Apache-2.0.

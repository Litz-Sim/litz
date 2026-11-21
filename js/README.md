# litz

A deterministic electronics and firmware simulator in one WebAssembly module. It runs a board, its firmware, the parts wired around it and the electrical rules between them, and it advances only when you ask it to — the same input produces the same run, byte for byte, on every machine.

The module has no imports and no host bindings: it is linear memory plus integers, loaded with `WebAssembly.instantiate` and driven through this package's small JavaScript API. No DOM, no worker requirements, no network.

```sh
npm install litz
```

## Quick start

```js
import { readFile } from "node:fs/promises";
import { loadLitz } from "litz";

const litz = await loadLitz();

const session = litz.createSession({
  diagram: await readFile("diagram.json", "utf8"),
  app: new Uint8Array(await readFile("firmware.bin")),
});

for (let i = 0; i < 20; i += 1) {
  session.run(400_000);          // scheduling steps, not milliseconds
  process.stdout.write(session.serial.take());
}

console.log(session.ms, "simulated ms");
console.log(session.parts());     // [{ id, code, kind, state, value }, ...]
session.close();
```

In a browser the same code works; `loadLitz()` fetches `litz_wasm.wasm` from beside the module.

## The diagram

`diagram.json` is the wiring: parts, their attributes and the connections between their pins. It is the same file the editor and the `litz` CLI read.

```json
{
  "version": 1,
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

## Running

A call to `run(steps)` advances the machine by at most `steps` scheduling steps and returns the simulated clock in milliseconds. A step is one unit of the simulator's scheduler, not a fixed span of time, so a host that wants a deadline should use `advance(ms)`:

```js
session.advance(500);   // run until the simulated clock reaches +500 ms
session.ms;             // 500
```

Both calls are synchronous and bounded, which is what makes them usable inside a `requestAnimationFrame` loop or a tight server loop.

## Serial

The console is collected automatically and capped at 1 MiB inside the module, oldest bytes dropped first, so a firmware that prints in a loop cannot exhaust memory.

```js
session.serial.take();          // drain what the firmware printed, as UTF-8
session.serial.write("hello\r");// deliver bytes to the board's UART0 receiver
```

## Parts and controls

```js
session.parts();               // every part the diagram declares
session.part("led1");          // { id, code, kind, state, value }
session.press("btn1", true);   // hold a pushbutton
session.setControl("pot1", 500);// 0 to 1000 permille of travel
```

`state` is a word — `off`, `on`, `up`, `down`, `silent`, `sounding`, `position`, `mask`, `state` — and `kind` says what the part is. A board instance reports `state: null`; `part()` returns `null` for an id the diagram does not have. `value` carries the number behind the state: servo degrees, a segment mask, a knob's position.

## Display and touch

```js
const panel = session.display();   // null when the circuit has no display
if (panel) {
  panel.width; panel.height; panel.version;  // version changes when pixels do
  const rgba = panel.pixels();              // Uint8Array, width * height * 4
}
session.touch(x, y, true);         // false when the board has no panel
```

## Logic analyser

```js
session.probe.add("tx", "esp:TX");       // <board>:<GPIO> or <part>:<pin>
session.trace.start();
session.advance(200);

session.scope();          // { ms, probes, decodes } — samples and measurements
session.trace.vcd();      // the same recording as a VCD document
session.decode.add("uart:rx=tx,baud=115200");
```

Every probe carries `digital` (`[tick_ns, level]`), `volts` (`[tick_ns, volts]`) and the measurements the run can derive from them: edges, high and low time, frequency, duty, minimum and maximum volts.

## Faults

A short across a supply, two outputs fighting over a net, a part on the wrong rail: the board resolves the net and records a conflict rather than guessing.

```js
session.conflicts();      // array of fault descriptions
```

## Loading options

```js
await loadLitz();                                  // the file beside this package
await loadLitz({ wasm: bytes });                   // Uint8Array or ArrayBuffer
await loadLitz({ wasm: "/assets/litz_wasm.wasm" });// fetched
await loadLitz({ wasm: module });                  // a compiled WebAssembly.Module
```

Bundlers that cannot copy arbitrary assets should read the file themselves and pass `wasm` as bytes. The module is about 2.4 MB uncompressed and compresses to roughly a third of that.

## Errors

Every failing call throws `LitzError` with the reason the module gave, so a bad diagram reports the parse error and a bad pin reports the pin.

```js
import { LitzError } from "litz";
try {
  litz.createSession({ diagram, app });
} catch (error) {
  if (error instanceof LitzError) console.error(error.message);
}
```

## Firmware images

This build runs ESP32-C3 images: an application image, optionally with a second-stage bootloader and partition table, and with a mask ROM when the firmware needs one. `createSession` takes `rom`, `boot` and `ptable` alongside `app`.

```js
litz.createSession({ diagram, rom, boot, ptable, app });
```

Other chips are in the simulator's native builds but not in this module yet.

The `litz` CLI builds firmware, runs the test files and can export a project. It is a separate Rust tool, not part of this package.

## License

MIT OR Apache-2.0.

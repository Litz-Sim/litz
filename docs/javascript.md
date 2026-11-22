# JavaScript API

This page is the reference for `@litz-sim/litz` 0.2.0. It documents every export of `index.js` and `index.d.ts`, the shapes that cross the WebAssembly boundary, the lifecycle rules, the host bridge that runs custom parts and chips, and the determinism guarantees. All samples are ES modules and run from the repository root with `node`; the package name resolves through `js/package.json` when a sample is saved inside `js/`.

The same simulator is a Rust library: [the Rust reference](rust.md) covers `machine::boot`, the project runner and the native extension points. The package wraps one compiled module, `js/litz_wasm.wasm`, built from `crates/litz-wasm`. The module exposes a synchronous C ABI over linear memory: pointers are byte offsets, strings are UTF-8, and every call returns immediately. The wrapper turns that ABI into `Litz`, `Session` and their supporting objects.

## Loading the module

### `loadLitz(options?: LoadOptions): Promise<Litz>`

`loadLitz` obtains the module bytes, compiles them, checks the export set the wrapper needs, instantiates the module with the `litz_host` bridge, attaches the instance's memory to that bridge, and returns a `Litz`. It rejects with `LitzError` when the source cannot be read or compiled, when the module lacks a required export, or when instantiation fails. The returned runtime is reusable: `createSession` may be called more than once.

| `LoadOptions` field | Type | Default | Meaning |
|---|---|---|---|
| `wasm` | `Uint8Array \| ArrayBuffer \| ArrayBufferView \| string \| URL \| Response \| WebAssembly.Module \| null` | `wasmUrl` | The module source. |
| `imports` | `WebAssembly.Imports` | `{}` | Extra import namespaces, merged with this package's `litz_host` bridge. |

The source is resolved in this order:

| Source | Behavior |
|---|---|
| `WebAssembly.Module` | Checked for the required exports, then instantiated. |
| `Uint8Array`, `ArrayBuffer`, `ArrayBufferView` | Copied, compiled with `WebAssembly.compile`, checked, then instantiated. |
| `Response` | Rejected when `ok` is false; otherwise its `arrayBuffer()` is read and compiled. |
| `string` or `URL` | Resolved against the loader's module URL when relative. A `file:` URL is read with `node:fs/promises`; any other protocol uses `fetch`. |
| `null` or omitted | `wasmUrl`, the file shipped beside `index.js`. |

A compile failure throws `LitzError("the wasm module could not be compiled")` with the original error as `cause`. A module missing exports throws `LitzError("the wasm module is missing <names>")`. A failed fetch throws `LitzError` with the status, and a host without `fetch` throws a `LitzError` that asks for bytes or a URL.

The `imports` object is shallow-merged. For every other namespace, the caller's entries are used. For `litz_host`, the caller's entries are merged with the built-in bridge, and the built-in names take precedence when both define the same import. The shipped module imports only `litz_host`.

### `wasmUrl: string`

The absolute URL of the shipped wasm file, computed as `new URL("../litz_wasm.wasm", import.meta.url).href`. `loadLitz()` uses it when `wasm` is omitted. It can be passed back as `wasm`, passed to `fetch`, or read with `node:fs/promises`.

### `DEFAULT_STEPS: number`

The number of scheduling steps used by `Session.run()` when the caller omits `steps`. Its value is `400_000`. A step is a unit of the simulator's scheduler, not a fixed amount of simulated time.

## `Litz`

A `Litz` owns one instantiated module. Its constructor is private; obtain an instance from `loadLitz`.

### `createSession(options: SessionOptions): Session`

Builds one board from the diagram and images and returns a `Session`. The call is synchronous. The chip is selected by the board instance in `diagram.json`; a diagram must name exactly one development board.

| `SessionOptions` field | Type | Default | Meaning |
|---|---|---|---|
| `diagram` | `string` | required | The `diagram.json` text. |
| `app` | `Uint8Array \| ArrayBuffer \| ArrayBufferView` | required | The application image. The bytes are copied before the call returns. |
| `rom` | `Uint8Array \| ArrayBuffer \| ArrayBufferView \| null` | `null` | Mask ROM. |
| `boot` | `Uint8Array \| ArrayBuffer \| ArrayBufferView \| null` | `null` | Second-stage bootloader. The module requires `ptable` as well. |
| `ptable` | `Uint8Array \| ArrayBuffer \| ArrayBufferView \| null` | `null` | Partition table. The module requires `boot` as well. |
| `flashSize` | `number` or `null` | board manifest, then module default | Flash size in bytes. Must be a positive whole number. |
| `mac` | `string` or `null` | module default | Station MAC such as `24:0a:c4:12:34:56`. |
| `analog` | `boolean` | `false` | Solve the analog network for every net. |
| `manifests` | `Array<string \| { toml: string, schematic: string }>` | `[]` | Catalog manifests as TOML text. A board manifest with an internal schematic uses the object form. |
| `parts` | `Record<string, PartFactory>` | `{}` | Part factories keyed by the module name in the part manifest. |
| `boards` | `Record<string, ChipFactory>` | `{}` | Chip factories keyed by the module name in the board manifest. |

`PartFactory` is `() => HostPart`. `ChipFactory` is `(host: ChipHost) => HostChip`.

The wrapper validates the arguments before it crosses into the module. A diagram that is not a non-empty string throws ``LitzError("createSession needs the diagram.json text as `diagram`")``. An absent or empty application image throws ``LitzError("createSession needs the application image as `app`")``. A `flashSize` that is not a positive integer throws ``LitzError("`flashSize` must be a positive whole number of bytes")``. A `mac` that is not a string throws ``LitzError("`mac` must be a string such as 24:0a:c4:12:34:56")``. A `manifests` value that is not an array of strings and `{ toml, schematic }` objects throws ``LitzError("`manifests` must be an array of TOML texts or {toml, schematic}")``. A `parts` or `boards` value that is not a function throws `LitzError("the factory for NAME must be a function")`.

The module adds its own checks and returns its message through `LitzError`. A diagram with no board reports `the diagram has no development board`; a diagram with more than one board reports `the diagram has N boards (ids); an in-memory session runs one`. A bootloader without a partition table, or the reverse, reports `bootloader and partition table must be supplied together`. An Xtensa board reports that the chip runs on Xtensa and that the browser module cannot execute its JIT, and names the RISC-V, ATmega328P and RP2040 alternatives. An application image of length zero is also rejected by the module.

Factories are registered for the duration of the call and restored afterward, so two sessions can use different factories for the same module name. The factory of a part is called once per part instance while the board is built. The session keeps each returned object and drops it through the bridge when the session closes. A factory that throws, returns a non-object, or is missing for a module the diagram names causes `createSession` to throw a `LitzError` that includes the module name, such as `the host has no runtime for module test-inverter.wasm`.

### `exports: WebAssembly.Exports`

The raw export object of the instantiated module. It exposes the complete C ABI, including calls the wrapper itself does not use.

### `instance: WebAssembly.Instance`

The instantiated `WebAssembly.Instance` the runtime was built from.

### `close(): void`

Releases the runtime's reusable buffers and the bridge's host-call buffers. The call is idempotent. Close every session before calling it; a session's handle and memory live in the module, and the runtime does not track open sessions. After `close`, `createSession` throws `LitzError("this runtime has been closed")`.

## `Session`

A `Session` owns one board: its chip, flash, peripherals, flattened circuit, console, probes and display. Its constructor is private; obtain an instance from `Litz.createSession`. Every member that reads or writes module state throws `LitzError("this session has been closed")` after the session is closed, except `trace.vcd()`, which returns an empty document when the module refuses the dropped handle. `handle` remains readable and `close` is idempotent.

### `run(steps?: number): number`

Runs the machine for at most `steps` scheduling steps and returns the simulated clock in milliseconds. `steps` defaults to `DEFAULT_STEPS` and is converted to an unsigned 32-bit integer, so pass a non-negative value. The call is synchronous and bounded.

`stop` describes how the run ended. A firmware that is still working returns `budget`; one with nothing to do until an input changes returns `idle`; one that has halted returns `halted`. A trap sets `stop` to `trap` and fills `fault`.

### `advance(ms: number, options?: AdvanceOptions): number`

Advances until the simulated clock has moved `ms` milliseconds and returns the clock. Because a scheduling step is not a fixed span of time, `advance` loops over `run` in chunks. `ms` is converted with `ms | 0` and negative values are treated as `0`, so the target is the current clock plus a non-negative 32-bit value.

| `AdvanceOptions` field | Type | Default | Meaning |
|---|---|---|---|
| `chunkSteps` | `number` | `20_000` | Steps per inner `run` call. |
| `maxSteps` | `number` | `200_000_000` | Total step budget. |

If the clock has not reached the target after `maxSteps` steps, the call throws `LitzError("the session did not reach N ms within M steps")`. The returned clock can be later than the target because the final chunk runs to the end of its step budget.

### `ms: number`

The simulated milliseconds since reset. The module stores this as a saturating 32-bit value, so it stops at about 49.7 days of simulated time.

### `chip(): ChipInfo`

Returns the backend the diagram booted, its clock and its flash layout. The value is parsed from JSON. A malformed document throws `LitzError("the chip description was not valid JSON")`.

### `stop: StopWord`

How the last `run` ended. The module reports an index into the fixed list `budget`, `idle`, `halted`, `trap`, `reset`, `breakpoint`. An index outside that list produces `"unknown"` even though the declared type names only the six words.

### `fault: string | null`

The backend's explanation for a trap, or `null` when the last run produced none. The value is the empty response from the module mapped to `null`.

### `heap(): number | null`

The firmware's own free heap in bytes, or `null` when the backend cannot answer. The module returns a negative value for "cannot be asked", which the wrapper maps to `null`.

### `serial: Serial`

The console. The module buffers output at up to 1 MiB, dropping the oldest bytes first, so a firmware that prints without bound cannot exhaust memory. See [Serial](#serial).

### `trace: Trace`

Wire recording for VCD export. See [Trace](#trace).

### `probe: Probes`

Probe points that the scope document reports. See [Probes](#probes).

### `decode: Decoders`

Protocol decoders over the probes. See [Decoders](#decoders).

### `conflicts(): string[]`

Returns the electrical conflicts the board resolved, oldest first. Each entry is the fault text. A short across a supply, two outputs driving one net in opposite directions, or a part on the wrong rail appears here rather than being guessed away.

### `parts(): PartState[]`

Returns every part the diagram declares, in the order the module enumerates them. A part with no runtime state, such as a board instance, appears with `code: -1`, `kind: "unknown"`, `state: null` and `value: 0`.

### `part(id: string): PartState | null`

Returns one part by id, including ids a board expansion invents such as `esp/led_pwr`. An id that is not a non-empty string throws `LitzError("part() needs a part id")`. An unknown id returns `null`, not an error.

### `press(id: string, pressed?: boolean): void`

Presses or releases a pushbutton wired to a GPIO. `pressed` defaults to `true`. The module decides whether the part supports pressing; an unknown id or a part that is not a button produces a stored module error, which the wrapper throws as `LitzError`.

### `setControl(id: string, permille: number): void`

Moves a continuous control such as a potentiometer. The value is rounded and clamped to `0` through `1000` permille of travel. A non-finite value throws `LitzError("setControl needs a number")`. A module refusal throws the stored text as `LitzError`.

### `setNamedControl(id: string, name: string, value: string | number): void`

Sets a named control on a part, such as a sensor's `temperature` or `lux`. The value is converted to a string before it crosses into the module. When the module reports that the part has no such control, the call throws `LitzError("part ID has no control NAME")` or the module's own message.

### `partPin(id: string, pin?: string): Level | null`

Returns the level on one of a part's pins. `pin` defaults to the empty string, which selects the part's first pin. A missing part or pin returns `null`; otherwise the result is `"low"`, `"high"` or `"high-z"`.

### `gpio(pin: number): Level | null`

Returns the level the chip sees on one of its own pads, by GPIO number. The pin is converted to an unsigned 32-bit integer. A pad the chip does not have returns `null`.

### `display(): Display | null`

Returns the panel when the circuit has a display, otherwise `null`. See [Display](#display).

### `touch(x: number, y: number, down?: boolean): boolean`

Delivers a touch at panel pixel coordinates. `x` and `y` are converted to unsigned 32-bit integers and `down` defaults to `true`. Returns `true` when the module accepted the touch and `false` when the board has no panel.

### `scope(): ScopeDocument`

Returns every probe's samples and every decoder's frames. The document is parsed from JSON; a malformed document throws `LitzError("the scope document was not valid JSON")`. See [ScopeDocument](#scopedocument).

### `handle: number`

The handle the session holds inside the module. `0` is never a valid session.

### `close(): void`

Drops the session and releases its module memory. The call is idempotent. After it returns, members that assert an open session throw `LitzError("this session has been closed")`; `handle` remains readable and `trace.vcd()` returns an empty document.

### `[Symbol.dispose](): void`

Calls `close()`. It allows `using session = litz.createSession(...)` on runtimes that implement explicit resource management.

## `Serial`

| Member | Signature | Behavior |
|---|---|---|
| `take` | `() => string` | Drains the console since the last call and decodes it as UTF-8. Returns `""` when nothing is buffered. |
| `write` | `(data: string \| Uint8Array \| ArrayBuffer \| ArrayBufferView) => void` | Delivers bytes to the board's UART0 receiver. A string is encoded as UTF-8. Empty input is a no-op. A module refusal throws `LitzError`. |

`take` reads in 64 KiB chunks until the module reports a short or empty chunk, so large bursts are returned whole. The 1 MiB cap is inside the module and applies to output the host has not drained yet.

## `Trace`

| Member | Signature | Behavior |
|---|---|---|
| `start` | `() => void` | Begins recording net transitions. |
| `transitions` | `number` (getter) | The number of transitions recorded so far. |
| `vcd` | `() => string` | The recording as a VCD document. The wrapper starts with a 64 KiB buffer and grows it while the module reports a full buffer. After the session is closed, this call returns an empty string because the module refuses the dropped handle. |

The trace records nets over the whole board. Probes are a separate recording with per-point samples and measurements.

## `Probes`

`add(name: string, point: string): void` watches one point and files its samples under `name` in `scope().probes`. A point is `<board>:<GPIO>`, such as `esp:GPIO8`, or `<part>:<pin>`, such as `led1:A`. The module resolves the point to a net on the flattened board. A point it cannot resolve makes `add` throw `LitzError("the probe point could not be added")` or the module's own message.

## `Decoders`

`add(spec: string): void` adds a decoder over one or more probes. The specification is a single line with a protocol name and comma-separated keys:

```text
uart:rx=<probe>[,baud=N]
i2c:sda=<probe>,scl=<probe>
spi:sck=<probe>,mosi=<probe>,cs=<probe>[,miso=<probe>][,mode=N]
```

The probe names are the names passed to `probe.add`. A specification the module rejects throws `LitzError("the decoder could not be added")` or the module's own message. Decoded frames appear in `scope().decodes`, with the original specification echoed back.

## `Display`

| Member | Type | Meaning |
|---|---|---|
| `width` | `number` | Panel width in pixels. |
| `height` | `number` | Panel height in pixels. |
| `version` | `number` (getter) | Grows when the pixels change, so a host can skip an unchanged frame. |
| `pixels` | `() => Uint8Array` | The frame as RGBA8888, `width * height * 4` bytes. |

`pixels()` returns a copy, so the array stays valid after later session calls. A short read from the module throws `LitzError("the display frame could not be read")`.

## `PartState`

| Field | Type | Meaning |
|---|---|---|
| `id` | `string` | The id the diagram gave the part. |
| `code` | `number` | The ABI's state code. `-1` marks a part with no runtime state, such as a board. |
| `kind` | `string` | What the part is: `led`, `button`, `switch`, `tone`, `servo`, `segments`, `knob`, `radio`, `none` or `unknown`. |
| `state` | `PartStateName \| null` | What it is doing, or `null` for a part that reports nothing. |
| `value` | `number` | The number behind the state: servo degrees, a segment mask, a tone rate, a knob position. `0` when the state has no number. |

`PartStateName` is the union `"none" | "off" | "on" | "up" | "down" | "silent" | "sounding" | "position" | "mask" | "state" | "unknown"`.

The state codes are fixed by the ABI: `0` none, `1` LED off, `2` LED on, `3` button up, `4` button down, `5` switch off, `6` switch on, `7` tone silent, `8` tone sounding, `9` servo position, `10` segment mask, `11` knob position, `12` radio state.

## `ScopeDocument`

| Field | Type | Meaning |
|---|---|---|
| `ms` | `number` | Simulated milliseconds at the time of the call. |
| `probes` | `ProbeRecord[]` | One record per probe added with `probe.add`. |
| `decodes` | `DecodeRecord[]` | One record per decoder added with `decode.add`. |

### `ProbeRecord`

`ProbeRecord` is the type of each `ScopeDocument.probes` entry. The declaration file exports it under that name, not under `PartRecord`.

| Field | Type | Meaning |
|---|---|---|
| `name` | `string` | The name given to `probe.add`. |
| `point` | `string` | The point string given to `probe.add`. |
| `net` | `number` | The net the probe resolved to on the flattened board. |
| `digital` | `Array<[number, number]>` | Level transitions as `[tick_ns, level]`, where level `0` is low, `1` is high and `2` is high-Z. |
| `volts` | `Array<[number, number]>` | Voltage samples as `[tick_ns, volts]`. |
| `edges` | `number` | Number of digital edges seen. |
| `high_ns` | `number` | Nanoseconds spent high. |
| `low_ns` | `number` | Nanoseconds spent low. |
| `frequency_hz` | `number \| null` | Measured frequency, or `null` when it cannot be derived. |
| `duty` | `number \| null` | Duty cycle from `0` to `1`, or `null` when it cannot be derived. |
| `min_volts` | `number \| null` | Minimum sampled voltage, or `null` with no analog samples. |
| `max_volts` | `number \| null` | Maximum sampled voltage, or `null` with no analog samples. |

### `DecodeRecord`

| Field | Type | Meaning |
|---|---|---|
| `spec` | `string` | The specification given to `decode.add`. |
| `kind` | `string` | The protocol: `uart`, `i2c` or `spi`. |
| `name` | `string` | The decoder's name. |
| `frames` | `Array<{ at: number, end: number, text: string, error: boolean \| string }>` | Decoded frames, with start and end ticks in nanoseconds. `error` is `false` for a valid frame and a description otherwise. |

## `ChipInfo`

| Field | Type | Meaning |
|---|---|---|
| `name` | `string` | The backend: `esp32c3`, `esp32c6`, `esp32p4`, `esp32s31`, `atmega328p`, `rp2040`, or the name a custom chip reports. |
| `cpuHz` | `number` | The clock in hertz. |
| `flash` | `ChipFlash` | Where the images sit in flash. |

`ChipFlash` has four numeric fields: `size`, `bootloader`, `partitionTable` and `app`, all byte offsets or sizes. A bare application is placed at `app`; a merged ESP image is placed at `bootloader`.

## `StopWord`

`StopWord` is `"budget" | "idle" | "halted" | "trap" | "reset" | "breakpoint"`. `budget` means the step budget ended the run; `idle` means the machine has nothing to do until an input changes; `halted` means the firmware stopped itself; `trap` means the backend trapped and usually fills `fault`; `reset` means the machine reset; `breakpoint` is reported when a backend stops at a breakpoint.

## `Level`

`Level` is `"low" | "high" | "high-z"`. Session pin readers return it, and a missing pin returns `null`. Custom parts receive the same three electrical cases under the word `"floating"` for high impedance; see [HostPart](#hostpart).

## `LitzError`

```ts
class LitzError extends Error {
  name: "LitzError";
  constructor(message: string, options?: { cause?: unknown });
}
```

`LitzError` is the error type for every failing library call. The message is the text the module produced, or a wrapper-level explanation when the failure happens before the call reaches the module. The loader uses `cause` for a compile failure. Errors raised by the JCO adapters for a wrong component shape are `TypeError`, not `LitzError`.

## JCO adapters

A custom part or board is a WebAssembly Component. It is transpiled once with `jco transpile --instantiation sync`, which emits a JavaScript module exporting `instantiate(getCoreModule, imports)` plus one or more `.core.wasm` files. `getCoreModule(name)` must return a compiled `WebAssembly.Module` for the named core module. The adapters below translate between jco's generated interfaces and the plain objects the host bridge drives.

### `HostPart`

A `HostPart` is one instantiated `litz:part` guest.

| Member | Signature | Meaning |
|---|---|---|
| `setPin` | `(pin: number, level: string, atNs: number) => void` | The board drives a pin. The bridge passes `"low"`, `"high"` or `"floating"` for the module's level codes `0`, `1` and `2`. |
| `advance` | `(toNs: number) => void` | Time has reached `toNs` nanoseconds. |
| `nextEvent` | `() => number \| null \| undefined` | The next time the part wants to be advanced, or `null`/`undefined` for none. |
| `drive` | `(pin: number) => { level: Level, strength?: "strong" \| "weak" } \| Level \| null` | What the part drives on a pin. `null`, `undefined` and an unrecognised word mean high impedance. |
| `state` | `() => { kind: string, value?: number } \| null` | The part's state. The bridge knows `none`, `led-off`, `led-on`, `button-up`, `button-down`, `switch-off`, `switch-on`, `tone-silent`, `tone-sounding`, `servo`, `segments` and `knob`. |
| `press` | `(pressed: boolean) => boolean` | Optional. Handle a press or release and report whether it applied. |
| `setControl` | `(permille: number) => boolean` | Optional. Handle a control movement and report whether it applied. |
| `analog` | `(pin: number) => number \| null` | Optional. The voltage the part holds on a pin, or `null` for none. |
| `i2cConnect` | `(address: number, read: boolean) => void` | Optional. A controller addressed the part. |
| `i2cRead` | `() => number` | Optional. Return one byte; the bridge uses `0xff` when absent. |
| `i2cWrite` | `(byte: number) => boolean` | Optional. Accept one byte and report acknowledgement. |
| `i2cStop` | `() => void` | Optional. The transaction ended. |
| `spiSelect` | `(selected: boolean) => void` | Optional. Chip-select changed. |
| `spiTransfer` | `(tx: Uint8Array, rxLen: number) => Uint8Array` | Optional. Exchange bytes; the bridge returns zeros of `rxLen` when absent. |
| `uartReceive` | `(bytes: Uint8Array, atNs: number) => void` | Optional. Bytes arrived on the part's UART. |
| `uartPoll` | `() => Uint8Array` | Optional. Bytes the part wants to send; the bridge returns an empty array when absent. |

The module calls only the operations the part's manifest enables. When it calls an optional method that the object does not provide, the call fails. `partFromJco` always supplies every method, so a transpiled component does not have this concern.

`partFromJco` also preserves an `info()` method when the component exports one, although `HostPart` in `index.d.ts` does not declare it.

### `HostChip`

A `HostChip` is one instantiated `litz:mcu` guest.

| Member | Signature | Meaning |
|---|---|---|
| `info` | `() => { name: string, description?: string, cpuHz: number, flash: { size: number, bootloader: number, partitionTable: number, app: number } }` | The chip's identity and flash layout. |
| `loadImage` | `(kind: "rom" \| "bootloader" \| "partition-table" \| "firmware", offset: number, bytes: Uint8Array) => void` | One image to place in flash. |
| `boot` | `() => number` | Start the chip; the result is passed back to the module as an unsigned integer. |
| `setInputs` | `(inputs: Array<{ pin: number, level: Level }>) => void` | The levels the board presents on the chip's pads. |
| `run` | `(budget: number) => { cycles: number, stop: StopWord }` | Execute up to `budget` cycles and report how the slice ended. |
| `takeOutputs` | `() => Array<{ pin: number, level: Level, enabled: boolean }>` | Pins the chip drove since the last call. |
| `writeConsole` | `(bytes: Uint8Array) => void` | Bytes the chip wants on the console. |

The bridge catches exceptions from these methods and stores the message. A failing `info` makes session creation fail with that message; a failing `run` becomes `{ cycles: 0, stop: "trap" }`. `host.console` and `host.fault` are the usual ways to report from inside a chip.

### `ChipHost`

The host object passed to every `ChipFactory`.

| Member | Signature | Meaning |
|---|---|---|
| `console` | `(bytes: string \| Uint8Array) => void` | Append text to the session's serial output. A string is encoded as UTF-8. |
| `fault` | `(detail: string) => void` | Record a fault; the text becomes `session.fault`. |
| `call` | `(op: string, args: Record<string, unknown>) => unknown` | Answer one `litz:mcu@0.2.0` host call against the circuit. `op` is the contract's camelCase name and `args` is a plain JSON object. Returns the reply's JSON value. |

`call` throws `LitzError` when the module was built without the `litz_guest_call` bridge, when the reply is not valid JSON, or when the circuit refuses the call (`reply.ok` is false). The error text is the circuit's reason.

### `partFromJco(root: unknown): HostPart`

Wraps the object returned by a transpiled part's `instantiate`. `root` is the whole return value; the adapter looks for `root.part`, then `root["litz:part/part@0.2.0"]`, then `root["litz:part/part@0.1.0"]`. It throws `TypeError("the transpiled module exports no litz:part interface")` when none exists.

The adapter converts nanosecond times to `BigInt` with rounding before calling the guest, converts `nextEvent()`'s `undefined` or `null` to `null`, and translates the component's tagged state to the bridge's `{ kind, value }` form: `led` becomes `led-on` or `led-off`, `button` becomes `button-down` or `button-up`, `switch` becomes `switch-on` or `switch-off`, `tone` becomes `tone-sounding` or `tone-silent` with the rate as `value`, and `segments`, `servo` and `knob` keep their tag as `kind` with the number as `value`. Any other tag becomes `{ kind: "none" }`.

The adapter supplies defaults for missing bus operations: `i2cRead` returns `0xff`, `i2cWrite` returns `false`, `spiTransfer` returns a zero-filled array of the requested length, and `uartPoll` returns an empty array. Exceptions from the guest propagate.

### `chipFromJco(root: unknown): HostChip`

Wraps the object returned by a transpiled chip's `instantiate`. It looks for `root.chip`, then `root["litz:mcu/chip@0.2.0"]`, then `root["litz:mcu/chip@0.1.0"]`, and throws `TypeError("the transpiled module exports no litz:mcu interface")` when none exists.

It normalizes `info()` so `cpuHz` and the four flash fields are JavaScript numbers, and converts the `loadImage` offset to `BigInt` with rounding. `run` converts the guest's `cycles` to a number and passes `stop` through. The guest's host import object must come from `chipImports(host)`, because the adapter only translates the chip interface and leaves the host side to that object.

### `partImports(options?: { log?: (message: string) => void, rom?: () => Uint8Array }): Record<string, unknown>`

Returns the import object a transpiled `litz:part` component expects: `{ "litz:part/host": { log, rom }, "litz:part/types": {} }`. `log` defaults to a no-op and `rom` defaults to a function returning an empty `Uint8Array`. The keys are unversioned because the generated jco glue indexes the import object by package name. The declaration files for a component may spell the names with a version suffix; the adapter returns the form jco reads.

### `chipImports(host: ChipHost): Record<string, unknown>`

Returns `{ "litz:mcu/host": { ... }, "litz:mcu/types": {} }` for a transpiled `litz:mcu@0.1.0` or `0.2.0` component. Every contract function is forwarded to `host.call` with the contract's camelCase operation name. Byte lists cross as arrays and are rebuilt as `Uint8Array` on return. A WIT `result<_, string>` error is thrown as an `Error` with a `payload` property, which is how jco distinguishes a declared error from a trap.

The bridged operations are `analogRead`, `i2cTransfer`, `spiTransfer`, `uartWrite`, `uartRead`, `wifiScan`, `wifiJoin`, `wifiLeave`, `wifiResolve`, `tcpConnect`, `tcpWrite`, `tcpRead`, `tcpClose`, `udpOpen`, `udpSend`, `udpRead`, `udpClose`, `bleScan`, `bleAdvertise`, `bleStopAdvertising`, `bleSetGatt`, `bleConnect`, `bleDisconnect`, `bleDiscover`, `bleRead`, `bleWrite`, `bleSubscribe`, `bleNotifications`, `bleTakeWrites`, `bleNotify`, `radioConfigure`, `radioSend` and `radioReceive`. `console` and `fault` are forwarded directly; `radioReceive` preserves the `sentAt` and `airtime` values as `BigInt`.

## Host import namespaces

The module imports exactly one namespace, `litz_host`, and has no WASI dependency. `loadLitz` fills it. The names and their behavior are fixed; a caller that passes its own `imports` can add other namespaces but cannot replace these names.

| Part import | Signature | Behavior |
|---|---|---|
| `part_create` | `(namePtr, nameLen) -> u32` | Look up a factory by UTF-8 module name, call it, and return an instance id. `0` on failure, with a message for `host_error`. |
| `part_drop` | `(id) -> void` | Forget the instance. |
| `part_set_pin` | `(id, pin, level, atNs) -> void` | Call `setPin` with `"low"`, `"high"` or `"floating"` and `Number(atNs)`. |
| `part_advance` | `(id, toNs) -> void` | Call `advance(Number(toNs))`. |
| `part_next_event` | `(id) -> i64` | Return `nextEvent()` rounded, or `-1` for none. |
| `part_drive` | `(id, pin) -> u32` | Pack `drive()`: low `0`, high `1`, high-Z `2`, with bit 8 set for weak drive. |
| `part_state` | `(id, ptr, cap) -> u32` | Write kind and value as two little-endian `u32`; return `8`, or `0` when `cap < 8`. |
| `part_press` | `(id, pressed) -> u32` | Call `press(pressed === 1)`; return `1` for truthy. |
| `part_set_control` | `(id, permille) -> u32` | Call `setControl(permille)`; return `1` for truthy. |
| `part_analog` | `(id, pin) -> f64` | Return `analog(pin)`, or `NaN` for none. |
| `part_i2c_connect` | `(id, address, read) -> void` | Call `i2cConnect(address, read === 1)`. |
| `part_i2c_read` | `(id) -> u32` | Return `i2cRead()`, defaulting to `0xff`. |
| `part_i2c_write` | `(id, byte) -> u32` | Call `i2cWrite(byte)`; return `1` for truthy. |
| `part_i2c_stop` | `(id) -> void` | Call `i2cStop()`. |
| `part_spi_select` | `(id, selected) -> void` | Call `spiSelect(selected === 1)`. |
| `part_spi_transfer` | `(id, txPtr, txLen, rxLen, outPtr, outCap) -> u32` | Read the transmit bytes, call `spiTransfer`, write up to `outCap` received bytes, return the count. |
| `part_uart_receive` | `(id, ptr, len, atNs) -> void` | Call `uartReceive(bytes, Number(atNs))`. |
| `part_uart_poll` | `(id, outPtr, outCap) -> u32` | Call `uartPoll()` and write up to `outCap` bytes; return the count. |
| `host_error` | `(ptr, cap) -> u32` | Write the last error text, UTF-8, up to `cap` bytes; return the count. |

| Chip import | Signature | Behavior |
|---|---|---|
| `chip_create` | `(namePtr, nameLen) -> u32` | Look up a chip factory by module name, call it with a `ChipHost`, and return an instance id. `0` on failure. |
| `chip_drop` | `(id) -> void` | Forget the chip. |
| `chip_info` | `(id, ptr, cap) -> u32` | Write `JSON.stringify(info())` up to `cap`; return the count, or `0` on failure. |
| `chip_load_image` | `(id, kind, offset, ptr, len) -> u32` | Call `loadImage` with kind `0` ROM, `1` bootloader, `2` partition table or `3` firmware. Return `1` on success. |
| `chip_boot` | `(id) -> i64` | Call `boot()` and return it as an unsigned integer; `-1` on failure. |
| `chip_set_inputs` | `(id, ptr, count) -> void` | Read `count` records of 8 bytes (`pin` and `level`, both `u32`) and call `setInputs`. |
| `chip_run` | `(id, budget, outPtr) -> u32` | Call `run(Number(budget))`, write 16 bytes (`cycles` as `u64`, then `stop` as `u32`), and return `16`. A failure writes zero cycles and stop code `3` (trap). |
| `chip_take_outputs` | `(id, ptr, cap) -> u32` | Write up to `cap / 12` records of `pin`, level code and enabled flag; return the bytes written. |
| `chip_write_console` | `(id, ptr, len) -> void` | Call `writeConsole(bytes)`. |

The bridge and the module share these code tables.

| Table | Values |
|---|---|
| Pin level | `0` low, `1` high, `2` floating (high impedance). |
| Image kind | `0` ROM, `1` bootloader, `2` partition table, `3` firmware. |
| Part state kind | `0` none, `1` LED off, `2` LED on, `3` button up, `4` button down, `5` switch off, `6` switch on, `7` tone silent, `8` tone sounding, `9` servo, `10` segments, `11` knob. |
| Stop | `0` budget, `1` idle, `2` halted, `3` trap, `4` reset. |

The module exposes three guest-side exports that the bridge calls: `litz_guest_console(id, ptr, len)`, `litz_guest_fault(id, ptr, len)` and `litz_guest_call(id, opPtr, opLen, argPtr, argLen, outPtr, outCap)`, whose return value is the reply length. `litz_guest_call` answers the 0.2 JSON door; when the module does not export it, `host.call` throws `LitzError("this module was built without the host-call bridge")`.

## A custom `litz:mcu@0.2.0` chip

This example boots the shipped `board-v2` fixture, a register-machine guest that exercises the bridge: it prints, scans Wi-Fi, reads I²C from a BMP280, scans BLE, reads an analog pin, writes UART, joins a network and opens a TCP echo. The fixture is the same component the test suite uses. Save the file as `demo-chip.mjs` in the repository root and run `node demo-chip.mjs`.

```js
import { readFileSync } from "node:fs";
import { loadLitz, chipFromJco, chipImports } from "./js/index.js";
import { instantiate } from "./js/test/fixtures/board-v2/board-v2.js";

const fixtureDir = "./js/test/fixtures/board-v2";
const cores = new Map();
const getCoreModule = (name) => {
  if (!cores.has(name)) {
    cores.set(name, new WebAssembly.Module(readFileSync(`${fixtureDir}/${name}`)));
  }
  return cores.get(name);
};

const diagram = JSON.stringify({
  version: 1,
  parts: [
    { id: "mcu", type: "board-v2-fixture", attrs: {} },
    { id: "bmp1", type: "litz-bmp280", attrs: {} },
    { id: "ap", type: "litz-wifi-router", attrs: { ssid: "lab", echo: "7" } },
    { id: "beacon", type: "litz-ble-beacon", attrs: {} },
    { id: "gps", type: "litz-gps-neo6m", attrs: {} },
  ],
  connections: [
    ["mcu:GPIO0", "bmp1:SDA", "", []],
    ["mcu:GPIO1", "bmp1:SCL", "", []],
    ["mcu:3V3", "bmp1:VCC", "", []],
    ["mcu:GND.1", "bmp1:GND", "", []],
    ["mcu:GPIO2", "gps:TX", "", []],
    ["mcu:GPIO3", "gps:RX", "", []],
    ["mcu:3V3", "gps:VCC", "", []],
    ["mcu:GND.1", "gps:GND", "", []],
  ],
});

const program = new Uint8Array([
  1, 0x68, 1, 0x69, 1, 0x0a,
  4, 0,
  4, 1, 0, 0x76, 0xd0,
  4, 2,
  4, 3, 0,
  4, 4, 0, 2, 0x4f, 0x4b,
  4, 5, 3, 0x6c, 0x61, 0x62,
  4, 6, 0x00, 0x07, 3, 0x70, 0x69, 0x6e,
  0,
]);

const litz = await loadLitz();
const session = litz.createSession({
  diagram,
  app: program,
  manifests: [
    {
      toml: readFileSync("crates/litz-mcu-wasm/tests/fixtures/board-v2/board.toml", "utf8"),
      schematic: readFileSync(
        "crates/litz-mcu-wasm/tests/fixtures/board-v2/board-v2.onboard.json",
        "utf8",
      ),
    },
  ],
  boards: {
    "board-v2.wasm": (host) =>
      chipFromJco(instantiate(getCoreModule, chipImports(host))),
  },
});

let serial = "";
for (let i = 0; i < 64 && session.stop !== "halted"; i += 1) {
  session.run(100_000);
  serial += session.serial.take();
}

process.stdout.write(serial);
console.log(session.chip().name, session.stop, session.fault);
session.close();
litz.close();
```

The expected console begins with `hi`, then carries lines for the Wi-Fi scan, the I²C read (`i2c 76 58`), the BLE scan, the analog read (`adc 0 none`), the UART write (`uart 0 ok`), the join (`joined lab at 10.0.2.15`) and the TCP echo (`tcp 7 pin`). The run ends with `session.stop` equal to `halted` and `session.fault` equal to `null`.

The two pieces that matter for a custom chip are the manifest and the factory. `manifests` supplies the board manifest as TOML plus its internal schematic JSON; the `boards` map uses the manifest's `module` name, `board-v2.wasm`, as its key. The factory receives a `ChipHost` and returns a `HostChip`. `chipFromJco(instantiate(getCoreModule, chipImports(host)))` connects the transpiled component's `host` import to that object, so every 0.2 call lands in the module's circuit.

## A custom part

A part follows the same pattern with `partFromJco` and `partImports`. The factory is keyed by the part manifest's `module` and is called once for each instance the diagram places.

```js
import { readFile } from "node:fs/promises";
import { loadLitz, partFromJco, partImports } from "./js/index.js";
import { instantiate } from "./js/test/fixtures/74hc595/74hc595.js";

const core = new WebAssembly.Module(
  await readFile("./js/test/fixtures/74hc595/74hc595.core.wasm"),
);
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
  app: new Uint8Array(await readFile("./examples/blinky/firmware/blinky.bin")),
  parts: {
    "74hc595.wasm": () => partFromJco(instantiate(() => core, partImports())),
  },
});

session.advance(500);
console.log(session.part("led1"));
session.close();
litz.close();
```

The part's `module` name in the catalog manifest is `74hc595.wasm`; the built-in `litz-74hc595` part manifest already names it, so no extra `manifests` entry is needed. For a part outside the built-in catalog, add its TOML to `manifests`.

## Lifecycle

Sessions and runtimes have a fixed close order: close every `Session` first, then call `Litz.close()`. Each session drops its handle, flash and peripherals when it closes, and the runtime then releases the scratch and host-call buffers that the sessions shared.

One session runs one board. The module rejects a diagram with no board and a diagram with more than one board; the multi-board message lists the board ids. A host that needs several boards opens one single-board session per board, either from the same runtime or from separate runtimes.

The wrapper reuses two scratch buffers per runtime, one for input and one for output, and grows them when a call needs more room. Returned values are copies: `serial.take()` decodes into a new string, `display().pixels()` returns a new array and `scope()` returns a parsed object, so a later call does not overwrite them. Calls are synchronous, so two sessions on one runtime cannot interleave across an `await`.

`Session.close()` and `Litz.close()` are idempotent. A session that is used after close throws `LitzError` on members that assert an open session; `trace.vcd()` returns an empty document. A runtime that is used after close throws `LitzError` on `createSession`. `close()` on a session calls `litz_drop` once and leaves the handle unchanged, so a second close is a no-op.

## Determinism

The simulator is deterministic by construction. The same diagram text, the same image bytes and the same sequence of session calls produce the same console output, part states, pin levels, probe samples, decoder frames and conflict list. The module reads no wall clock, no environment variable and no random source, and the wrapper adds no scheduling of its own; it copies bytes across linear memory and calls the module synchronously. The default MAC is fixed, and passing `mac` replaces it with a fixed value for the run.

`run(steps)` advances by at most `steps` scheduling steps. A step is a unit of the scheduler, so the amount of simulated time per call depends on what the firmware does; a firmware that is idle can consume its budget while the clock moves little, and a firmware that is busy can move the clock further. `advance(ms)` is the time-based interface: it repeatedly calls `run(chunkSteps)` until the integer millisecond clock reaches the target, and throws when `maxSteps` is exhausted. The `ms` clock is simulated milliseconds since reset and saturates at the 32-bit maximum.

Inputs are applied at the simulated position where the host makes the call. `serial.write`, `press`, `setControl`, `setNamedControl` and `touch` take effect before the next `run`, so two hosts that issue the same calls at the same `ms` values observe the same run. Changing the chunk size in `advance` does not change the simulated result, because chunking only decides how often control returns to the host; it changes how many `run` calls are made, not the machine's schedule.

The guarantee covers the built-in chips, parts and the module's own solvers. A custom part or chip that reads host time, uses `Math.random`, depends on object iteration order for its own outputs, or returns different values for the same inputs can break determinism; the bridge forwards what the guest returns and does not sandbox it.

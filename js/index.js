// Litz as a headless WebAssembly library.
//
// The module next to this file is built from `crates/litz-wasm`, which exposes
// a C ABI over linear memory: pointers are byte offsets, strings are UTF-8 and
// every call is synchronous. This file turns that into objects — a session, its
// serial line, its parts, its display — and nothing else. There is no DOM code
// here and no host dependency, so the same import runs in Node, in a browser
// worker and in any bundler that understands `import.meta.url`.
//
// A session owns one board, its flash and its peripherals. `session.run(steps)`
// advances it and returns the simulated clock; the host decides how much time
// to simulate per call.

/** Scheduling steps per call when the host does not choose. */
export const DEFAULT_STEPS = 400_000;

/** Where the wasm file sits when the package is loaded as a module. */
export const wasmUrl = new URL("./litz_wasm.wasm", import.meta.url).href;

const REQUIRED_EXPORTS = [
  "memory",
  "litz_alloc",
  "litz_free",
  "litz_new",
  "litz_drop",
  "litz_last_error",
  "litz_run",
  "litz_ms",
  "litz_conflict_count",
  "litz_conflict",
  "litz_serial_take",
  "litz_serial_write",
  "litz_display_info",
  "litz_display_version",
  "litz_display_rgba",
  "litz_touch",
  "litz_trace_start",
  "litz_trace_len",
  "litz_trace_vcd",
  "litz_probe_add",
  "litz_scope_len",
  "litz_scope_json",
  "litz_decode_add",
  "litz_part_state",
  "litz_part_value",
  "litz_press",
  "litz_set_control",
  "litz_part_count",
  "litz_part_id",
];

// State codes from `litz_part_state`; `-1` means the diagram has no such id.
// The code carries both what the part is and what it is doing, which is why
// the wrapper reports a kind as well as a word.
const STATE_CODES = [
  { kind: "none", state: "none" },
  { kind: "led", state: "off" },
  { kind: "led", state: "on" },
  { kind: "button", state: "up" },
  { kind: "button", state: "down" },
  { kind: "switch", state: "off" },
  { kind: "switch", state: "on" },
  { kind: "tone", state: "silent" },
  { kind: "tone", state: "sounding" },
  { kind: "servo", state: "position" },
  { kind: "segments", state: "mask" },
  { kind: "knob", state: "position" },
  { kind: "radio", state: "state" },
];

const ENCODER = new TextEncoder();
const DECODER = new TextDecoder();

export class LitzError extends Error {
  constructor(message, options) {
    super(message, options);
    this.name = "LitzError";
  }
}

/**
 * Load the module and return a runtime that can open sessions.
 *
 * `wasm` takes bytes, a URL, a `Response`, an `ArrayBuffer` or an already
 * instantiated `WebAssembly.Module`; without it the file shipped next to this
 * module is used. `imports` is passed to `WebAssembly.instantiate` unchanged —
 * the release build imports nothing, so it is there for a host that wraps the
 * module with its own glue.
 */
export async function loadLitz({ wasm = null, imports = {} } = {}) {
  const instance = await instantiate(wasm ?? wasmUrl, imports);
  return new Litz(instance);
}

async function instantiate(source, imports) {
  if (source instanceof WebAssembly.Module) {
    const missing = missingExports(source);
    if (missing.length > 0) throw new LitzError(`the wasm module is missing ${missing.join(", ")}`);
    const instance = await WebAssembly.instantiate(source, imports);
    return instance instanceof WebAssembly.Instance ? instance : instance.instance;
  }
  const bytes = await toBytes(source);
  let module;
  try {
    module = await WebAssembly.compile(bytes);
  } catch (error) {
    throw new LitzError("the wasm module could not be compiled", { cause: error });
  }
  const missing = missingExports(module);
  if (missing.length > 0) {
    throw new LitzError(`the wasm module is missing ${missing.join(", ")}`);
  }
  const result = await WebAssembly.instantiate(module, imports);
  return result instanceof WebAssembly.Instance ? result : result.instance;
}

function missingExports(module) {
  const present = new Set(WebAssembly.Module.exports(module).map((entry) => entry.name));
  return REQUIRED_EXPORTS.filter((name) => !present.has(name));
}

async function toBytes(source) {
  if (source instanceof ArrayBuffer) return new Uint8Array(source.slice(0));
  if (ArrayBuffer.isView(source)) {
    return new Uint8Array(
      source.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength),
    );
  }
  if (typeof Response !== "undefined" && source instanceof Response) {
    if (!source.ok) throw new LitzError(`fetching the wasm module failed: ${source.status}`);
    return new Uint8Array(await source.arrayBuffer());
  }
  const url = source instanceof URL ? source : new URL(String(source), import.meta.url);
  if (url.protocol === "file:") {
    const { readFile } = await import("node:fs/promises");
    return new Uint8Array(await readFile(url));
  }
  if (typeof fetch !== "function") {
    throw new LitzError("this host has no fetch; pass `wasm` as bytes or a URL");
  }
  const response = await fetch(url);
  if (!response.ok) {
    throw new LitzError(`fetching ${url.href} failed: ${response.status}`);
  }
  return new Uint8Array(await response.arrayBuffer());
}

/** The loaded module. One of these serves any number of sessions. */
export class Litz {
  #instance;
  #exports;
  #closed = false;
  #scratchIn = 0;
  #scratchInSize = 0;
  #scratchOut = 0;
  #scratchOutSize = 0;
  #io;

  constructor(instance) {
    this.#instance = instance;
    this.#exports = instance.exports;
    const litz = this;
    // The memory boundary, handed to sessions so they never reach into this
    // object's state. Buffers are reused between calls and grow on demand.
    this.#io = {
      get exports() {
        return litz.#exports;
      },
      view(ptr, len) {
        return new Uint8Array(litz.#exports.memory.buffer, ptr, len);
      },
      writeIn(bytes) {
        if (bytes.length > litz.#scratchInSize) {
          litz.#release({ ptr: litz.#scratchIn, len: litz.#scratchInSize });
          litz.#scratchInSize = Math.max(bytes.length, 4096);
          litz.#scratchIn = litz.#exports.litz_alloc(litz.#scratchInSize);
          if (litz.#scratchIn === 0) {
            litz.#scratchInSize = 0;
            throw new LitzError(`could not allocate ${bytes.length} bytes in the module`);
          }
        }
        litz.#view(litz.#scratchIn, bytes.length).set(bytes);
        return { ptr: litz.#scratchIn, len: bytes.length };
      },
      writeOut(len) {
        if (len > litz.#scratchOutSize) {
          litz.#release({ ptr: litz.#scratchOut, len: litz.#scratchOutSize });
          litz.#scratchOutSize = Math.max(len, 4096);
          litz.#scratchOut = litz.#exports.litz_alloc(litz.#scratchOutSize);
          if (litz.#scratchOut === 0) {
            litz.#scratchOutSize = 0;
            throw new LitzError(`could not allocate ${len} bytes in the module`);
          }
        }
        return { ptr: litz.#scratchOut, len };
      },
      copyOut(len) {
        return new Uint8Array(litz.#view(litz.#scratchOut, len));
      },
      release(buffer) {
        litz.#release(buffer);
      },
      error(fallback) {
        const cap = litz.#io.writeOut(1024);
        const written = litz.#exports.litz_last_error(cap.ptr, cap.len);
        const text = written ? DECODER.decode(litz.#view(cap.ptr, written)) : "";
        return new LitzError(text || fallback);
      },
      stored() {
        const cap = litz.#io.writeOut(1024);
        const written = litz.#exports.litz_last_error(cap.ptr, cap.len);
        return written ? DECODER.decode(litz.#view(cap.ptr, written)) : "";
      },
    };
  }

  /** The raw export object, for a host that needs an export this API omits. */
  get exports() {
    return this.#exports;
  }

  /** The instance this runtime was built from. */
  get instance() {
    return this.#instance;
  }

  /**
   * Open a session.
   *
   * `diagram` is the diagram.json text and `app` the application image.
   * `rom`, `boot` and `ptable` are optional; giving `boot` without `ptable`
   * (or the reverse) is an error, because the bootloader cannot find the
   * application without the table.
   */
  createSession({ diagram, app, rom = null, boot = null, ptable = null } = {}) {
    this.#assertOpen();
    if (typeof diagram !== "string" || diagram.length === 0) {
      throw new LitzError("createSession needs the diagram.json text as `diagram`");
    }
    const appBytes = bytesOf(app, "app");
    if (appBytes.length === 0) {
      throw new LitzError("createSession needs the application image as `app`");
    }

    const io = this.#io;
    const diagramBuf = io.writeIn(ENCODER.encode(diagram));
    const romBytes = optionalBytes(rom, "rom");
    const bootBytes = optionalBytes(boot, "boot");
    const ptableBytes = optionalBytes(ptable, "ptable");
    const buffers = [
      hold(io, romBytes),
      hold(io, bootBytes),
      hold(io, ptableBytes),
      hold(io, appBytes),
    ];
    const [romBuf, bootBuf, ptableBuf, appBuf] = buffers;
    let handle;
    try {
      handle = io.exports.litz_new(
        diagramBuf.ptr,
        diagramBuf.len,
        romBuf.ptr,
        romBuf.len,
        bootBuf.ptr,
        bootBuf.len,
        ptableBuf.ptr,
        ptableBuf.len,
        appBuf.ptr,
        appBuf.len,
      );
    } finally {
      for (const buffer of buffers) io.release(buffer);
    }
    if (handle === 0) throw io.error("the session could not be created");
    return new Session(io, handle);
  }

  /** Release the module memory this runtime holds. Sessions must be closed first. */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#release({ ptr: this.#scratchIn, len: this.#scratchInSize });
    this.#release({ ptr: this.#scratchOut, len: this.#scratchOutSize });
    this.#scratchIn = 0;
    this.#scratchOut = 0;
    this.#scratchInSize = 0;
    this.#scratchOutSize = 0;
  }

  #assertOpen() {
    if (this.#closed) throw new LitzError("this runtime has been closed");
  }

  #view(ptr, len) {
    return new Uint8Array(this.#exports.memory.buffer, ptr, len);
  }

  #release(buffer) {
    // The two reusable buffers belong to the runtime; a caller "releasing" one
    // of them is a no-op, not a free that the next call would then reuse.
    if (buffer.ptr === 0) return;
    if (buffer.ptr === this.#scratchIn || buffer.ptr === this.#scratchOut) return;
    this.#exports.litz_free(buffer.ptr, buffer.len);
  }
}

function hold(io, bytes) {
  if (bytes.length === 0) return { ptr: 0, len: 0 };
  const ptr = io.exports.litz_alloc(bytes.length);
  if (ptr === 0) throw new LitzError(`could not allocate ${bytes.length} bytes in the module`);
  io.view(ptr, bytes.length).set(bytes);
  return { ptr, len: bytes.length };
}

function bytesOf(value, label) {
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  throw new LitzError(`\`${label}\` must be a Uint8Array`);
}

function optionalBytes(value, label) {
  if (value === null || value === undefined) return new Uint8Array(0);
  return bytesOf(value, label);
}

function decodeChunks(chunks) {
  if (chunks.length === 0) return "";
  if (chunks.length === 1) return DECODER.decode(chunks[0]);
  const total = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const joined = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    joined.set(chunk, at);
    at += chunk.length;
  }
  return DECODER.decode(joined);
}

/** One running board. */
export class Session {
  #io;
  #handle;
  #closed = false;
  #serial;
  #trace;
  #probe;
  #decode;

  /** @internal Sessions come from `Litz.createSession`. */
  constructor(io, handle) {
    this.#io = io;
    this.#handle = handle;

    const session = this;
    this.#serial = {
      take: () => session.#takeSerial(),
      write: (data) => session.#writeSerial(data),
    };
    this.#trace = Object.freeze({
      start: () => {
        session.#assertOpen();
        session.#x.litz_trace_start(session.#handle);
      },
      get transitions() {
        session.#assertOpen();
        return session.#x.litz_trace_len(session.#handle);
      },
      vcd: () =>
        session.#readText(
          (ptr, cap) => session.#x.litz_trace_vcd(session.#handle, ptr, cap),
          64 * 1024,
        ),
    });
    this.#probe = Object.freeze({
      add: (name, point) => session.#addProbe(name, point),
    });
    this.#decode = Object.freeze({
      add: (spec) => session.#addDecode(spec),
    });
  }

  /** The session handle inside the module; 0 is never a session. */
  get handle() {
    return this.#handle;
  }

  get #x() {
    return this.#io.exports;
  }

  #assertOpen() {
    if (this.#closed) throw new LitzError("this session has been closed");
  }

  /** Advance by at most `steps` scheduling steps; returns the simulated ms. */
  run(steps = DEFAULT_STEPS) {
    this.#assertOpen();
    return this.#x.litz_run(this.#handle, steps >>> 0);
  }

  /**
   * Advance until `ms` of simulated time have passed. A scheduling step is not
   * a fixed unit of time, so this loops in chunks until the clock moves.
   */
  advance(ms, { chunkSteps = 20_000, maxSteps = 200_000_000 } = {}) {
    this.#assertOpen();
    const target = this.ms + Math.max(0, ms | 0);
    let spent = 0;
    while (this.ms < target && spent < maxSteps) {
      this.run(chunkSteps);
      spent += chunkSteps;
    }
    if (this.ms < target) {
      throw new LitzError(`the session did not reach ${target} ms within ${maxSteps} steps`);
    }
    return this.ms;
  }

  /** Simulated milliseconds since reset. */
  get ms() {
    this.#assertOpen();
    return this.#x.litz_ms(this.#handle);
  }

  /** The console. `take()` drains what the firmware printed. */
  get serial() {
    return this.#serial;
  }

  /** Wire recording for VCD export. */
  get trace() {
    return this.#trace;
  }

  /** Probe points the scope document reports. */
  get probe() {
    return this.#probe;
  }

  /** Protocol decoders over the probes. */
  get decode() {
    return this.#decode;
  }

  /** Electrical conflicts: the fault text of each, oldest first. */
  conflicts() {
    this.#assertOpen();
    const count = this.#x.litz_conflict_count(this.#handle);
    const out = [];
    for (let index = 0; index < count; index += 1) {
      out.push(this.#readText((ptr, cap) => this.#x.litz_conflict(this.#handle, index, ptr, cap)));
    }
    return out;
  }

  /** Every part the diagram declares, with its current state. */
  parts() {
    this.#assertOpen();
    const count = this.#x.litz_part_count(this.#handle);
    const out = [];
    for (let index = 0; index < count; index += 1) {
      const id = this.#readText((ptr, cap) => this.#x.litz_part_id(this.#handle, index, ptr, cap));
      // A board instance reports no runtime state of its own; it still belongs
      // in the list, with `state: null`.
      out.push(this.#state(id) ?? { id, code: -1, kind: "unknown", state: null, value: 0 });
    }
    return out;
  }

  /** One part by id, including board-expanded ids such as `esp/led_pwr`. */
  part(id) {
    this.#assertOpen();
    if (typeof id !== "string" || id.length === 0) {
      throw new LitzError("part() needs a part id");
    }
    return this.#state(id);
  }

  /** Press or release a pushbutton wired to a GPIO. */
  press(id, pressed = true) {
    this.#assertOpen();
    const buf = this.#io.writeIn(ENCODER.encode(String(id)));
    try {
      this.#x.litz_press(this.#handle, buf.ptr, buf.len, pressed ? 1 : 0);
    } finally {
      this.#io.release(buf);
    }
    this.#throwStored();
  }

  /** Move a continuous control: 0 to 1000 permille of its travel. */
  setControl(id, permille) {
    this.#assertOpen();
    const value = Math.round(Number(permille));
    if (!Number.isFinite(value)) throw new LitzError("setControl needs a number");
    const clamped = Math.min(1000, Math.max(0, value));
    const buf = this.#io.writeIn(ENCODER.encode(String(id)));
    try {
      this.#x.litz_set_control(this.#handle, buf.ptr, buf.len, clamped);
    } finally {
      this.#io.release(buf);
    }
    this.#throwStored();
  }

  /** The panel size and pixels, or `null` when the circuit has no display. */
  display() {
    this.#assertOpen();
    const info = this.#x.litz_display_info(this.#handle);
    if (info === 0) return null;
    const width = info >>> 16;
    const height = info & 0xffff;
    const session = this;
    return {
      width,
      height,
      get version() {
        session.#assertOpen();
        return session.#x.litz_display_version(session.#handle);
      },
      pixels() {
        session.#assertOpen();
        const needed = width * height * 4;
        const cap = session.#io.writeOut(needed);
        const written = session.#x.litz_display_rgba(session.#handle, cap.ptr, cap.len);
        if (written !== needed) {
          throw session.#io.error("the display frame could not be read");
        }
        return session.#io.copyOut(written);
      },
    };
  }

  /** Deliver a touch at panel pixels. False when the board has no panel. */
  touch(x, y, down = true) {
    this.#assertOpen();
    return this.#x.litz_touch(this.#handle, x >>> 0, y >>> 0, down ? 1 : 0) === 1;
  }

  /** The scope document: every probe's samples and every decoder's frames. */
  scope() {
    this.#assertOpen();
    const length = this.#x.litz_scope_len(this.#handle);
    const text = this.#readText(
      (ptr, cap) => this.#x.litz_scope_json(this.#handle, ptr, cap),
      length,
    );
    try {
      return JSON.parse(text);
    } catch (error) {
      throw new LitzError("the scope document was not valid JSON", { cause: error });
    }
  }

  /** Close the session and release its memory in the module. */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#x.litz_drop(this.#handle);
  }

  /** `using session = ...` support where explicit resource management exists. */
  [Symbol.dispose]() {
    this.close();
  }

  #state(id) {
    const buf = this.#io.writeIn(ENCODER.encode(id));
    let code;
    let value;
    try {
      code = this.#x.litz_part_state(this.#handle, buf.ptr, buf.len);
      value = this.#x.litz_part_value(this.#handle, buf.ptr, buf.len);
    } finally {
      this.#io.release(buf);
    }
    if (code < 0) return null;
    const entry = STATE_CODES[code] ?? { kind: "unknown", state: "unknown" };
    return { id, code, kind: entry.kind, state: entry.state, value };
  }

  #takeSerial() {
    this.#assertOpen();
    const chunks = [];
    for (;;) {
      const cap = this.#io.writeOut(64 * 1024);
      const written = this.#x.litz_serial_take(this.#handle, cap.ptr, cap.len);
      if (written === 0) break;
      chunks.push(this.#io.copyOut(written));
      if (written < cap.len) break;
    }
    return decodeChunks(chunks);
  }

  #writeSerial(data) {
    this.#assertOpen();
    const bytes = typeof data === "string" ? ENCODER.encode(data) : bytesOf(data, "serial data");
    if (bytes.length === 0) return;
    const buf = this.#io.writeIn(bytes);
    try {
      this.#x.litz_serial_write(this.#handle, buf.ptr, buf.len);
    } finally {
      this.#io.release(buf);
    }
    this.#throwStored();
  }

  #addProbe(name, point) {
    this.#assertOpen();
    // Two strings in one call cannot share the reusable input buffer.
    const nameBuf = hold(this.#io, ENCODER.encode(String(name)));
    const pointBuf = hold(this.#io, ENCODER.encode(String(point)));
    let ok;
    try {
      ok = this.#x.litz_probe_add(
        this.#handle,
        nameBuf.ptr,
        nameBuf.len,
        pointBuf.ptr,
        pointBuf.len,
      );
    } finally {
      this.#io.release(nameBuf);
      this.#io.release(pointBuf);
    }
    if (ok !== 1) throw this.#io.error("the probe point could not be added");
  }

  #addDecode(spec) {
    this.#assertOpen();
    const buf = this.#io.writeIn(ENCODER.encode(String(spec)));
    let ok;
    try {
      ok = this.#x.litz_decode_add(this.#handle, buf.ptr, buf.len);
    } finally {
      this.#io.release(buf);
    }
    if (ok !== 1) throw this.#io.error("the decoder could not be added");
  }

  #throwStored() {
    const message = this.#io.stored();
    if (message) throw new LitzError(message);
  }

  #readText(fill, initial = 256) {
    let capacity = Math.max(64, initial + 1);
    for (;;) {
      const cap = this.#io.writeOut(capacity);
      const written = fill(cap.ptr, cap.len);
      if (written < capacity) return DECODER.decode(this.#io.copyOut(written));
      capacity *= 2;
      if (capacity > 64 * 1024 * 1024) {
        throw new LitzError("a value from the module did not fit in 64 MiB");
      }
    }
  }
}

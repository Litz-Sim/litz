// Loading the module and opening sessions.

import { DECODER, ENCODER, REQUIRED_EXPORTS } from "./constants.js";
import { bytesOf, optionalBytes, hold, toBytes } from "./bytes.js";
import { LitzError } from "./errors.js";
import { HostRuntime } from "./host.js";
import { Session } from "./session.js";

/** Where the wasm file sits when the package is loaded as a module. */
export const wasmUrl = new URL("../litz_wasm.wasm", import.meta.url).href;

/**
 * Load the module and return a runtime that can open sessions.
 *
 * `wasm` takes bytes, a URL, a `Response`, an `ArrayBuffer` or an already
 * instantiated `WebAssembly.Module`; without it the file shipped next to this
 * module is used. `imports` is merged into the module's import object: the
 * `litz_host` namespace is this package's custom-part bridge, and anything the
 * caller supplies is added alongside it.
 */
export async function loadLitz({ wasm = null, imports = {} } = {}) {
  const host = new HostRuntime();
  const merged = {
    ...imports,
    litz_host: { ...(imports.litz_host ?? {}), ...host.imports },
  };
  const instance = await instantiate(wasm ?? wasmUrl, merged);
  host.attach(instance.exports.memory, instance.exports);
  return new Litz(instance, host);
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

export class Litz {
  #instance;
  #exports;
  #closed = false;
  #scratchIn = 0;
  #scratchInSize = 0;
  #scratchOut = 0;
  #scratchOutSize = 0;
  #io;
  #host;

  constructor(instance, host) {
    this.#instance = instance;
    this.#exports = instance.exports;
    this.#host = host;
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
   * `diagram` is the diagram.json text and `app` the application image. The
   * chip is chosen by the board the diagram names: any builtin board boots —
   * the ESP32 family, the ATmega328P and the RP2040.
   *
   * `rom`, `boot` and `ptable` are optional; giving `boot` without `ptable`
   * (or the reverse) is an error, because the bootloader cannot find the
   * application without the table. `flashSize`, `mac` and `analog` override
   * the board manifest and the module defaults.
   *
   * `manifests` adds catalog manifests as TOML text (custom parts and boards),
   * and `parts` maps a wasm module name to a factory that returns one
   * instance of the component's `part` interface. The factory is called once
   * per part instance while the board is built; the session keeps the
   * instance afterwards.
   */
  createSession({
    diagram,
    app,
    rom = null,
    boot = null,
    ptable = null,
    flashSize = null,
    mac = null,
    analog = false,
    manifests = [],
    parts = {},
    boards = {},
  } = {}) {
    this.#assertOpen();
    if (typeof diagram !== "string" || diagram.length === 0) {
      throw new LitzError("createSession needs the diagram.json text as `diagram`");
    }
    const appBytes = bytesOf(app, "app");
    if (appBytes.length === 0) {
      throw new LitzError("createSession needs the application image as `app`");
    }
    if (flashSize !== null && !(Number.isInteger(flashSize) && flashSize > 0)) {
      throw new LitzError("`flashSize` must be a positive whole number of bytes");
    }
    if (mac !== null && typeof mac !== "string") {
      throw new LitzError("`mac` must be a string such as 24:0a:c4:12:34:56");
    }
    if (
      !Array.isArray(manifests) ||
      manifests.some(
        (entry) =>
          typeof entry !== "string" &&
          (typeof entry !== "object" || entry === null || typeof entry.toml !== "string"),
      )
    ) {
      throw new LitzError("`manifests` must be an array of TOML texts or {toml, schematic}");
    }

    const io = this.#io;
    const romBytes = optionalBytes(rom, "rom");
    const bootBytes = optionalBytes(boot, "boot");
    const ptableBytes = optionalBytes(ptable, "ptable");
    const buffers = [
      hold(io, appBytes),
      hold(io, romBytes),
      hold(io, bootBytes),
      hold(io, ptableBytes),
    ];
    const [appBuf, romBuf, bootBuf, ptableBuf] = buffers;
    const spec = {
      diagram,
      app: { ptr: appBuf.ptr, len: appBuf.len },
      rom: romBuf.len > 0 ? { ptr: romBuf.ptr, len: romBuf.len } : null,
      boot: bootBuf.len > 0 ? { ptr: bootBuf.ptr, len: bootBuf.len } : null,
      ptable: ptableBuf.len > 0 ? { ptr: ptableBuf.ptr, len: ptableBuf.len } : null,
    };
    if (flashSize !== null) spec.flashSize = flashSize;
    if (mac !== null) spec.mac = mac;
    if (analog) spec.analog = true;
    if (manifests.length > 0) spec.manifests = manifests;
    const jsonBuf = hold(io, ENCODER.encode(JSON.stringify(spec)));
    const previous = this.#host.register({ parts, boards });
    let handle;
    try {
      handle = io.exports.litz_new_json(jsonBuf.ptr, jsonBuf.len);
    } finally {
      this.#host.restore(previous);
      io.release(jsonBuf);
      for (const buffer of buffers) io.release(buffer);
    }
    if (handle === 0) throw io.error("the session could not be created");
    return new Session(io, handle);
  }

  /** Release the module memory this runtime holds. Sessions must be closed first. */
  close() {
    if (this.#closed) return;
    this.#closed = true;
    this.#host.release();
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

export { Session };

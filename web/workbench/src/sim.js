// The run controller: loads the wasm module next to the workbench page, owns
// one simulator session, steps it from requestAnimationFrame and exposes the
// C ABI in terms the UI can use. It is DOM-free apart from the optional
// default URL; `node --test` can inject a fake exports object.
//
// The module is deliberately separate from app.js: the page decides what to
// draw and how to map pointer input, this module only steps, drains serial and
// reads part state back.

export const WASM_URL = "../litz_wasm.wasm";
export const SCRATCH_BYTES = 64 * 1024;
export const MAX_SERIAL_CHARS = 200_000;
export const SPEED_OPTIONS = [100_000, 400_000, 1_600_000];
export const DEFAULT_SPEED = 400_000;

const REQUIRED_EXPORTS = [
  "litz_alloc",
  "litz_free",
  "litz_new",
  "litz_drop",
  "litz_last_error",
  "litz_run",
  "litz_serial_take",
  "litz_serial_write",
  "litz_part_state",
  "litz_press",
  "litz_set_control",
  "litz_part_count",
  "litz_part_id",
  "litz_probe_add",
  "litz_decode_add",
  "litz_scope_len",
  "litz_scope_json",
];

export class SimError extends Error {
  constructor(message) {
    super(message);
    this.name = "SimError";
  }
}

/** Where the wasm file sits when the workbench page is the base URL. */
export function resolveWasmUrl(base = null) {
  const url = base ?? (typeof document !== "undefined" ? document.baseURI : null);
  if (!url) return WASM_URL;
  try {
    return new URL(WASM_URL, url).href;
  } catch {
    return WASM_URL;
  }
}

/** Keep a bounded tail of serial text; older bytes are dropped first. */
export function appendSerial(text, chunk, cap = MAX_SERIAL_CHARS) {
  const incoming = String(chunk ?? "");
  if (!incoming) return String(text ?? "");
  const combined = String(text ?? "") + incoming;
  return combined.length > cap ? combined.slice(combined.length - cap) : combined;
}

/** A positive integer number of steps per frame; the ABI takes a u32. */
export function stepsPerFrameFor(value, fallback = DEFAULT_SPEED) {
  const steps = Number(value);
  if (!Number.isFinite(steps) || steps <= 0) return fallback;
  return Math.min(0xffffffff, Math.max(1, Math.floor(steps)));
}

export function formatMs(ms) {
  const value = Math.max(0, Number(ms) || 0);
  if (value < 1000) return `${Math.floor(value)} ms`;
  const seconds = value / 1000;
  if (seconds < 60) return `${seconds.toFixed(3)} s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${(seconds - minutes * 60).toFixed(2)}s`;
}

export function formatRate(rate) {
  const value = Number(rate) || 0;
  if (!Number.isFinite(value) || value <= 0) return "0";
  if (value >= 1e6) return `${(value / 1e6).toFixed(2)}M`;
  if (value >= 1e3) return `${Math.round(value / 1e3)}k`;
  return `${Math.round(value)}`;
}

function toBytes(value, label) {
  if (value === null || value === undefined) return new Uint8Array(0);
  if (value instanceof Uint8Array) return new Uint8Array(value);
  if (value instanceof ArrayBuffer) return new Uint8Array(value.slice(0));
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength));
  }
  if (Array.isArray(value)) return Uint8Array.from(value);
  throw new SimError(`${label} is not an ArrayBuffer, typed array or byte array`);
}

function readBytes(value) {
  if (value === null || value === undefined) return new Uint8Array(0);
  if (value instanceof Uint8Array) return value;
  if (value instanceof ArrayBuffer) return new Uint8Array(value);
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  if (Array.isArray(value)) return Uint8Array.from(value);
  return new Uint8Array(value);
}

const nowDefault = () =>
  typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();

/**
 * Build a controller. All seams are injectable so tests need no browser:
 *
 *   wasm        an already-instantiated exports object
 *   fetchImpl   fetch replacement
 *   instantiate async (ArrayBuffer) => exports object
 *   requestFrame / cancelFrame   requestAnimationFrame pair
 *   now         clock for the steps-per-second readout
 */
export function createRunController({
  wasmUrl = resolveWasmUrl(),
  wasm = null,
  fetchImpl = typeof fetch === "function" ? fetch.bind(globalThis) : null,
  instantiate = null,
  requestFrame = typeof requestAnimationFrame === "function"
    ? requestAnimationFrame.bind(globalThis)
    : null,
  cancelFrame = typeof cancelAnimationFrame === "function"
    ? cancelAnimationFrame.bind(globalThis)
    : null,
  now = nowDefault,
  scratchBytes = SCRATCH_BYTES,
  maxSerialChars = MAX_SERIAL_CHARS,
  onFrame = null,
  onError = null,
} = {}) {
  const encoder = new TextEncoder();
  const decoder = new TextDecoder();

  const controller = {
    wasmUrl,
    scratchBytes,
    maxSerialChars,
    onFrame,
    onError,

    wasm: wasm ?? null,
    scratch: 0,
    handle: 0,
    /// Last display counter handed to the page, so a still frame is not redrawn.
    displayVersion: -1,
    inputs: null,
    running: false,
    loaded: false,
    fatal: false,
    error: "",
    simMs: 0,
    conflicts: 0,
    firstConflict: "",
    serial: "",
    pendingSerial: "",
    stepsPerFrame: DEFAULT_SPEED,
    rate: 0,
    accSteps: 0,
    rateAt: null,
    frameHandle: null,
    initPromise: null,
    disposed: false,

    hasSession() {
      return controller.handle !== 0;
    },

    state() {
      if (controller.fatal) return "error";
      if (controller.initPromise && !controller.wasm) return "loading";
      if (controller.handle) return controller.running ? "running" : "paused";
      return "empty";
    },

    status() {
      return {
        state: controller.state(),
        running: controller.running,
        loaded: controller.loaded,
        hasSession: controller.hasSession(),
        simMs: controller.simMs,
        conflicts: controller.conflicts,
        firstConflict: controller.firstConflict,
        serial: controller.serial,
        rate: controller.rate,
        stepsPerFrame: controller.stepsPerFrame,
        error: controller.error,
      };
    },

    /**
     * Fetch and instantiate the module. Idempotent; a failed attempt clears
     * the cached promise so a later click can retry.
     */
    async init() {
      if (controller.disposed) throw new SimError("the run controller was disposed");
      if (controller.wasm) {
        ensureExports(controller.wasm);
        reserveScratch();
        return controller.wasm;
      }
      if (controller.initPromise) return controller.initPromise;

      controller.initPromise = (async () => {
        let exports;
        if (instantiate) {
          const buffer = await readWasmBytes();
          exports = await instantiate(buffer);
        } else {
          if (!fetchImpl) throw new SimError("no fetch is available to load the wasm module");
          let response;
          try {
            response = await fetchImpl(controller.wasmUrl, { cache: "no-store" });
          } catch (error) {
            throw new SimError(
              `Could not fetch ${controller.wasmUrl}. Run ./web/build.sh and serve this folder over HTTP. (${error.message})`,
            );
          }
          if (!response.ok) {
            throw new SimError(
              `${controller.wasmUrl} was not found (HTTP ${response.status}). Run ./web/build.sh, then reload.`,
            );
          }
          const buffer = await response.arrayBuffer();
          try {
            const result = await WebAssembly.instantiate(buffer, {});
            exports = result.instance ? result.instance.exports : result.exports;
          } catch (error) {
            throw new SimError(`Could not instantiate ${controller.wasmUrl}: ${error.message}`);
          }
        }
        exports = exports?.instance?.exports ?? exports?.exports ?? exports;
        ensureExports(exports);
        controller.wasm = exports;
        reserveScratch();
        return exports;
      })();

      try {
        return await controller.initPromise;
      } catch (error) {
        controller.initPromise = null;
        throw error;
      }
    },

    /**
     * Build a session from a diagram document and firmware images.
     * `app` is required; `rom`, `boot` and `ptable` are optional, and the
     * bootloader and partition table must arrive together.
     */
    async load(input = {}) {
      if (controller.disposed) throw new SimError("the run controller was disposed");
      await controller.init();

      const diagramText =
        typeof input.diagramText === "string"
          ? input.diagramText
          : input.diagram
            ? JSON.stringify(input.diagram)
            : "";
      if (!diagramText) throw new SimError("load() needs a diagram document");

      const app = toBytes(input.app, "the application image");
      const rom = toBytes(input.rom, "the ROM image");
      const boot = toBytes(input.boot, "the bootloader image");
      const ptable = toBytes(input.ptable, "the partition table image");

      if (app.length === 0) throw new SimError("no application image: choose a .bin or .hex file");
      if ((boot.length > 0) !== (ptable.length > 0)) {
        throw new SimError("the bootloader and partition table must be supplied together");
      }

      controller.pause();
      controller._dropSession();
      controller.loaded = false;
      controller.fatal = false;
      controller.error = "";
      controller.inputs = { diagramText, app, rom, boot, ptable };

      const blocks = [];
      let handle = 0;
      try {
        blocks.push(controller._alloc(encoder.encode(diagramText)));
        blocks.push(controller._alloc(rom));
        blocks.push(controller._alloc(boot));
        blocks.push(controller._alloc(ptable));
        blocks.push(controller._alloc(app));
        const [diagram, romBlock, bootBlock, ptableBlock, appBlock] = blocks;
        handle = Number(
          controller.wasm.litz_new(
            diagram.ptr,
            diagram.len,
            romBlock.ptr,
            romBlock.len,
            bootBlock.ptr,
            bootBlock.len,
            ptableBlock.ptr,
            ptableBlock.len,
            appBlock.ptr,
            appBlock.len,
          ),
        );
      } catch (error) {
        throw new SimError(error.message);
      } finally {
        blocks.forEach((block) => controller._free(block));
      }

      if (!handle) {
        throw new SimError(controller.lastError() || "litz_new returned 0");
      }

      controller.handle = handle;
      controller.loaded = true;
      controller.simMs = 0;
      controller.conflicts = 0;
      controller.firstConflict = "";
      controller.serial = "";
      controller.pendingSerial = "";
      controller.rate = 0;
      controller.accSteps = 0;
      controller.rateAt = now();
      controller._emit();
      return controller.status();
    },

    /** Drop the session but keep the instantiated module ready. */
    unload() {
      controller.pause();
      controller._dropSession();
      controller.inputs = null;
      controller.loaded = false;
      controller.fatal = false;
      controller.error = "";
      controller.simMs = 0;
      controller.conflicts = 0;
      controller.firstConflict = "";
      controller.serial = "";
      controller.pendingSerial = "";
      controller.rate = 0;
      controller.accSteps = 0;
      controller._emit();
      return controller.status();
    },

    /** Rebuild the session from the images it was loaded with. */
    async reset() {
      if (!controller.inputs) throw new SimError("there is no session to reset");
      const inputs = controller.inputs;
      controller._dropSession();
      await controller.load(inputs);
      return controller.status();
    },

    /** Drop the session and release every buffer the controller owns. */
    dispose() {
      if (controller.disposed) return;
      controller.pause();
      controller._dropSession();
      if (controller.scratch && controller.wasm) {
        controller.wasm.litz_free(controller.scratch, controller.scratchBytes);
      }
      controller.scratch = 0;
      controller.wasm = null;
      controller.inputs = null;
      controller.disposed = true;
      controller._emit();
    },

    run() {
      if (!controller.handle || controller.fatal) return controller.status();
      if (controller.running) return controller.status();
      controller.running = true;
      controller.accSteps = 0;
      controller.rateAt = now();
      controller._schedule();
      controller._emit();
      return controller.status();
    },

    pause() {
      controller.running = false;
      controller.rate = 0;
      controller._cancelFrame();
      controller._emit();
      return controller.status();
    },

    toggle() {
      return controller.running ? controller.pause() : controller.run();
    },

    setSpeed(value) {
      controller.stepsPerFrame = stepsPerFrameFor(value);
      controller.accSteps = 0;
      controller.rateAt = now();
      return controller.stepsPerFrame;
    },

    /** Advance one animation frame. Public so a test can drive it directly. */
    tick(frameTime = now()) {
      if (!controller.handle) return controller.status();
      if (controller.running && !controller.fatal) {
        try {
          controller.simMs = Number(
            controller.wasm.litz_run(controller.handle, controller.stepsPerFrame),
          );
          controller.accSteps += controller.stepsPerFrame;
        } catch (error) {
          controller._fail(error);
          return controller.status();
        }
      }

      try {
        controller._pumpSerial();
        controller._pollConflicts();
      } catch (error) {
        controller._fail(error);
        return controller.status();
      }

      if (frameTime - (controller.rateAt ?? frameTime) >= 500) {
        const elapsed = Math.max(1, frameTime - controller.rateAt);
        controller.rate = controller.running ? (controller.accSteps * 1000) / elapsed : 0;
        controller.accSteps = 0;
        controller.rateAt = frameTime;
      }

      controller._emit();
      return controller.status();
    },

    /** Forget the retained scrollback and anything not yet handed to the page. */
    clearSerial() {
      controller.serial = "";
      controller.pendingSerial = "";
    },

    /** New serial text since the last call, decoded as UTF-8. */
    takeSerial() {
      const text = controller.pendingSerial;
      controller.pendingSerial = "";
      return text;
    },

    /** Type into UART0. Returns false when there is no session. */
    writeSerial(text) {
      const bytes = encoder.encode(String(text ?? ""));
      if (!controller.handle || bytes.length === 0) return false;
      const block = controller._alloc(bytes);
      try {
        controller.wasm.litz_serial_write(controller.handle, block.ptr, block.len);
        const error = controller.lastError();
        if (error) {
          controller.error = error;
          return false;
        }
        return true;
      } catch (error) {
        controller.error = error.message;
        return false;
      } finally {
        controller._free(block);
      }
    },

    partState(id) {
      if (!controller.handle || typeof controller.wasm.litz_part_state !== "function") return -1;
      try {
        return Number(
          controller._withId(id, (ptr, len) =>
            controller.wasm.litz_part_state(controller.handle, ptr, len),
          ),
        );
      } catch (error) {
        controller.error = error.message;
        return -1;
      }
    },

    partValue(id) {
      if (!controller.handle || typeof controller.wasm.litz_part_value !== "function") return 0;
      try {
        return Number(
          controller._withId(id, (ptr, len) =>
            controller.wasm.litz_part_value(controller.handle, ptr, len),
          ),
        );
      } catch {
        return 0;
      }
    },

    press(id, down) {
      if (!controller.handle || typeof controller.wasm.litz_press !== "function") return false;
      try {
        controller._withId(id, (ptr, len) =>
          controller.wasm.litz_press(controller.handle, ptr, len, down ? 1 : 0),
        );
        const error = controller.lastError();
        if (error) {
          controller.error = error;
          return false;
        }
        controller.error = "";
        return true;
      } catch (error) {
        controller.error = error.message;
        return false;
      }
    },

    setControl(id, permille) {
      if (!controller.handle || typeof controller.wasm.litz_set_control !== "function") {
        return false;
      }
      const value = Math.max(0, Math.min(1000, Math.round(Number(permille) || 0)));
      try {
        controller._withId(id, (ptr, len) =>
          controller.wasm.litz_set_control(controller.handle, ptr, len, value),
        );
        const error = controller.lastError();
        if (error) {
          controller.error = error;
          return false;
        }
        controller.error = "";
        return true;
      } catch (error) {
        controller.error = error.message;
        return false;
      }
    },

    /**
     * Add a scope probe on `point` (`esp:GPIO8` or `led1:A`) under `name`.
     * A name is used once per session; a refusal carries the ABI's reason.
     */
    addProbe(name, point) {
      if (controller.disposed) throw new SimError("the run controller was disposed");
      if (!controller.handle) throw new SimError("there is no session to add a probe to");
      const label = String(name ?? "");
      const target = String(point ?? "");
      if (!target.trim()) throw new SimError("a probe needs a point such as esp:GPIO8");
      if (typeof controller.wasm.litz_probe_add !== "function") {
        throw new SimError("the wasm module is missing: litz_probe_add");
      }

      let nameBlock = { ptr: 0, len: 0 };
      let pointBlock = { ptr: 0, len: 0 };
      try {
        nameBlock = controller._alloc(encoder.encode(label));
        pointBlock = controller._alloc(encoder.encode(target));
        const added = Number(
          controller.wasm.litz_probe_add(
            controller.handle,
            nameBlock.ptr,
            nameBlock.len,
            pointBlock.ptr,
            pointBlock.len,
          ),
        );
        if (!added) {
          const message = controller.lastError() || "litz_probe_add was refused";
          controller.error = message;
          throw new SimError(message);
        }
        controller.error = "";
        return true;
      } finally {
        controller._free(nameBlock);
        controller._free(pointBlock);
      }
    },

    /**
     * Add a decoder from `spec` (`uart:rx=tx,baud=115200`). The module parses
     * the spec; a refusal carries the ABI's reason.
     */
    addDecode(spec) {
      if (controller.disposed) throw new SimError("the run controller was disposed");
      if (!controller.handle) throw new SimError("there is no session to add a decoder to");
      const text = typeof spec === "string" ? spec : "";
      if (!text.trim()) {
        throw new SimError("a decoder needs a spec such as uart:rx=tx,baud=115200");
      }
      if (typeof controller.wasm.litz_decode_add !== "function") {
        throw new SimError("the wasm module is missing: litz_decode_add");
      }

      let block = { ptr: 0, len: 0 };
      try {
        block = controller._alloc(encoder.encode(text));
        const added = Number(
          controller.wasm.litz_decode_add(controller.handle, block.ptr, block.len),
        );
        if (!added) {
          const message = controller.lastError() || "litz_decode_add was refused";
          controller.error = message;
          throw new SimError(message);
        }
        controller.error = "";
        return true;
      } finally {
        controller._free(block);
      }
    },

    /** The current scope document, or null when there is no session or data. */
    scope() {
      if (!controller.handle || typeof controller.wasm?.litz_scope_len !== "function") {
        return null;
      }
      try {
        const length = Number(controller.wasm.litz_scope_len(controller.handle));
        if (!length) return null;
        const block = controller._alloc(new Uint8Array(length));
        try {
          const written = Number(
            controller.wasm.litz_scope_json(controller.handle, block.ptr, block.len),
          );
          if (!written) return null;
          return JSON.parse(decoder.decode(controller._view(block.ptr, Math.min(written, block.len))));
        } finally {
          controller._free(block);
        }
      } catch (error) {
        controller.error = error.message;
        return null;
      }
    },

    /** The board display's size, or null when the circuit has none. */
    displayInfo() {
      if (!controller.handle || typeof controller.wasm?.litz_display_info !== "function") {
        return null;
      }
      try {
        const packed = Number(controller.wasm.litz_display_info(controller.handle));
        if (!packed) return null;
        return { width: packed >>> 16, height: packed & 0xffff };
      } catch (error) {
        controller.error = error.message;
        return null;
      }
    },

    /**
     * The display as RGBA, or null when it did not change since the last call.
     * A changed frame is `{width, height, version, data}`; `data` is a copy,
     * so the caller may keep it after the next run step.
     */
    display() {
      if (!controller.handle || typeof controller.wasm?.litz_display_rgba !== "function") {
        return null;
      }
      try {
        const info = this.displayInfo();
        if (!info) return null;
        const version = Number(controller.wasm.litz_display_version(controller.handle));
        if (version === controller.displayVersion) return null;
        const bytes = info.width * info.height * 4;
        const block = controller._alloc(new Uint8Array(bytes));
        try {
          const written = Number(
            controller.wasm.litz_display_rgba(controller.handle, block.ptr, block.len),
          );
          if (written !== bytes) return null;
          controller.displayVersion = version;
          return {
            width: info.width,
            height: info.height,
            version,
            data: controller._view(block.ptr, bytes).slice(),
          };
        } finally {
          controller._free(block);
        }
      } catch (error) {
        controller.error = error.message;
        return null;
      }
    },

    /** Send a touch at panel pixels. Returns false without a session. */
    touch(x, y, down) {
      if (!controller.handle || typeof controller.wasm?.litz_touch !== "function") return false;
      try {
        return Boolean(
          controller.wasm.litz_touch(
            controller.handle,
            Math.max(0, Math.round(Number(x) || 0)),
            Math.max(0, Math.round(Number(y) || 0)),
            down ? 1 : 0,
          ),
        );
      } catch (error) {
        controller.error = error.message;
        return false;
      }
    },

    /** Decode the ABI's last error, or an empty string. */
    lastError() {
      if (!controller.wasm || !controller.scratch) return "";
      try {
        const cap = controller.scratchBytes;
        const written = Number(
          controller.wasm.litz_last_error(controller.scratch, cap),
        );
        if (!written) return "";
        const take = Math.min(written, cap);
        return decoder.decode(controller._view(controller.scratch, take)).trim();
      } catch {
        return "";
      }
    },

    partCount() {
      if (!controller.handle) return 0;
      try {
        return Number(controller.wasm.litz_part_count(controller.handle));
      } catch {
        return 0;
      }
    },

    partIds() {
      const ids = [];
      if (!controller.handle) return ids;
      let count = 0;
      try {
        count = Number(controller.wasm.litz_part_count(controller.handle));
      } catch {
        return ids;
      }
      for (let index = 0; index < count; index += 1) {
        try {
          const written = Number(
            controller.wasm.litz_part_id(
              controller.handle,
              index,
              controller.scratch,
              controller.scratchBytes,
            ),
          );
          if (written > 0) {
            ids.push(decoder.decode(controller._view(controller.scratch, Math.min(written, controller.scratchBytes))));
          }
        } catch {
          break;
        }
      }
      return ids;
    },

    // ------------------------------------------------------------- internals

    _view(ptr, len) {
      return new Uint8Array(controller.wasm.memory.buffer, ptr, len);
    },

    _alloc(bytes) {
      const data = readBytes(bytes);
      if (data.length === 0) return { ptr: 0, len: 0 };
      const ptr = Number(controller.wasm.litz_alloc(data.length));
      if (!ptr) throw new SimError(`litz_alloc(${data.length}) failed`);
      controller._view(ptr, data.length).set(data);
      return { ptr, len: data.length };
    },

    _free(block) {
      if (!block || !block.ptr) return;
      try {
        controller.wasm.litz_free(block.ptr, block.len);
      } catch {
        // A session that died still owns its own memory; freeing is best-effort.
      }
    },

    _withId(id, call) {
      const bytes = encoder.encode(String(id ?? ""));
      if (bytes.length === 0 || bytes.length > controller.scratchBytes) return call(0, 0);
      controller._view(controller.scratch, bytes.length).set(bytes);
      return call(controller.scratch, bytes.length);
    },

    _pumpSerial() {
      if (!controller.handle || typeof controller.wasm.litz_serial_take !== "function") return;
      let text = "";
      const cap = controller.scratchBytes;
      for (let round = 0; round < 32; round += 1) {
        const written = Number(
          controller.wasm.litz_serial_take(controller.handle, controller.scratch, cap),
        );
        if (!written) break;
        const take = Math.min(written, cap);
        text += decoder.decode(controller._view(controller.scratch, take), { stream: true });
        if (take < cap) break;
      }
      if (!text) return;
      // New output is handed to the page in full; only the retained scrollback
      // is capped, so a UI that reads every frame never loses a chunk.
      controller.pendingSerial = appendSerial(controller.pendingSerial, text, MAX_SERIAL_CHARS);
      controller.serial = appendSerial(controller.serial, text, controller.maxSerialChars);
    },

    _pollConflicts() {
      if (!controller.handle || typeof controller.wasm.litz_conflict_count !== "function") return;
      const count = Number(controller.wasm.litz_conflict_count(controller.handle));
      if (count !== controller.conflicts) {
        controller.conflicts = count;
        controller.firstConflict = "";
      }
      if (count > 0 && !controller.firstConflict && typeof controller.wasm.litz_conflict === "function") {
        const written = Number(
          controller.wasm.litz_conflict(controller.handle, 0, controller.scratch, controller.scratchBytes),
        );
        if (written > 0) {
          const take = Math.min(written, controller.scratchBytes);
          controller.firstConflict =
            decoder.decode(controller._view(controller.scratch, take)).replace(/\s+/g, " ").trim() ||
            "electrical conflict";
        } else {
          controller.firstConflict = "electrical conflict";
        }
      }
    },

    _dropSession() {
      if (controller.handle && controller.wasm) {
        try {
          controller.wasm.litz_drop(controller.handle);
        } catch {
          // The handle is gone either way; never keep stepping a dead session.
        }
      }
      controller.handle = 0;
      controller.displayVersion = -1;
      controller.running = false;
      controller._cancelFrame();
    },

    _schedule() {
      if (controller.frameHandle !== null || !requestFrame || !controller.running) return;
      controller.frameHandle = requestFrame((frameTime) => controller._frame(frameTime));
    },

    _cancelFrame() {
      if (controller.frameHandle === null) return;
      if (cancelFrame) cancelFrame(controller.frameHandle);
      controller.frameHandle = null;
    },

    _frame(frameTime) {
      controller.frameHandle = null;
      controller.tick(frameTime);
      if (controller.running && !controller.fatal) controller._schedule();
    },

    _fail(error) {
      if (controller.fatal) return;
      controller.fatal = true;
      controller.running = false;
      controller.error = error?.message ? error.message : String(error);
      controller._cancelFrame();
      if (typeof controller.onError === "function") controller.onError(controller.error);
      controller._emit();
    },

    _emit() {
      if (typeof controller.onFrame === "function") controller.onFrame(controller.status());
    },
  };

  function ensureExports(exports) {
    if (!exports) throw new SimError("the wasm module produced no exports");
    const missing = REQUIRED_EXPORTS.filter((name) => typeof exports[name] !== "function");
    if (missing.length) throw new SimError(`the wasm module is missing: ${missing.join(", ")}`);
    if (!exports.memory) throw new SimError("the wasm module does not export memory");
  }

  function reserveScratch() {
    if (controller.scratch) return;
    const ptr = Number(controller.wasm.litz_alloc(controller.scratchBytes));
    if (!ptr) throw new SimError("litz_alloc failed while reserving a scratch buffer");
    controller.scratch = ptr;
  }

  async function readWasmBytes() {
    if (!fetchImpl) throw new SimError("no fetch is available to load the wasm module");
    const response = await fetchImpl(controller.wasmUrl, { cache: "no-store" });
    if (!response.ok) {
      throw new SimError(
        `${controller.wasmUrl} was not found (HTTP ${response.status}). Run ./web/build.sh, then reload.`,
      );
    }
    return response.arrayBuffer();
  }

  return controller;
}
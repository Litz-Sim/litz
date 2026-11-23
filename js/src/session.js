// One running board: the public surface over the ABI.

import {
  DEFAULT_STEPS,
  DECODER,
  ENCODER,
  LEVELS,
  STATE_CODES,
  STOPS,
} from "./constants.js";
import { bytesOf, decodeChunks, hold } from "./bytes.js";
import { LitzError } from "./errors.js";

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
      takeBoard: (board) => session.#takeBoardSerial(board),
      write: (data) => session.#writeSerial(data),
      writeBoard: (board, data) => session.#writeBoardSerial(board, data),
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

  /**
   * The chip the diagram booted: `{ name, cpuHz, flash: { size, bootloader,
   * partitionTable, app } }`.
   */
  chip() {
    this.#assertOpen();
    const text = this.#readText(
      (ptr, cap) => this.#x.litz_chip_info(this.#handle, ptr, cap),
      128,
    );
    try {
      return JSON.parse(text);
    } catch (error) {
      throw new LitzError("the chip description was not valid JSON", { cause: error });
    }
  }

  /** Every development board the session booted, in diagram order. */
  boards() {
    this.#assertOpen();
    const count = this.#x.litz_board_count(this.#handle);
    const out = [];
    for (let index = 0; index < count; index += 1) {
      const id = this.#readText(
        (ptr, cap) => this.#x.litz_board_id(this.#handle, index, ptr, cap),
        32,
      );
      const info = this.#readText(
        (ptr, cap) => this.#x.litz_board_chip_info(this.#handle, index, ptr, cap),
        128,
      );
      let chip;
      try {
        chip = JSON.parse(info);
      } catch (error) {
        throw new LitzError("the chip description was not valid JSON", { cause: error });
      }
      out.push({ index, id, ...chip });
    }
    return out;
  }

  /** One board by list index or `diagram.json` id. */
  board(board = 0) {
    this.#assertOpen();
    if (typeof board === "number") {
      const entry = this.boards()[board];
      if (!entry) throw new LitzError(`there is no board at index ${board}`);
      return entry;
    }
    const entry = this.boards().find((candidate) => candidate.id === board);
    if (!entry) throw new LitzError(`there is no board ${board}`);
    return entry;
  }

  /** The level one board sees on one of its own pads. */
  gpioOn(board, pin) {
    this.#assertOpen();
    const index = this.#boardIndex(board);
    const code = this.#x.litz_board_gpio_level(this.#handle, index, pin >>> 0);
    return code < 0 ? null : LEVELS[code] ?? null;
  }

  /** How the last `run` ended: budget, idle, halted, trap, reset or breakpoint. */
  get stop() {
    this.#assertOpen();
    return STOPS[this.#x.litz_stop(this.#handle)] ?? "unknown";
  }

  /** The backend's explanation for a trap, or `null` when there is none. */
  get fault() {
    this.#assertOpen();
    return (
      this.#readText((ptr, cap) => this.#x.litz_fault(this.#handle, ptr, cap), 64) || null
    );
  }

  /** The firmware's own free heap in bytes, or `null` when it cannot be asked. */
  heap() {
    this.#assertOpen();
    const bytes = this.#x.litz_heap(this.#handle);
    return bytes < 0 ? null : bytes;
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

  /**
   * The level on one of a part's pins: `low`, `high` or `high-z`, or `null`
   * when the part or pin is not in the diagram. Omit `pin` for the part's
   * first pin.
   */
  partPin(id, pin = "") {
    this.#assertOpen();
    const idBuf = hold(this.#io, ENCODER.encode(String(id)));
    const pinBuf = hold(this.#io, ENCODER.encode(String(pin)));
    let code;
    try {
      code = this.#x.litz_part_pin_level(
        this.#handle,
        idBuf.ptr,
        idBuf.len,
        pinBuf.ptr,
        pinBuf.len,
      );
    } finally {
      this.#io.release(idBuf);
      this.#io.release(pinBuf);
    }
    return code < 0 ? null : LEVELS[code] ?? null;
  }

  /** The level the chip sees on one of its own pads, by GPIO number. */
  gpio(pin) {
    this.#assertOpen();
    const code = this.#x.litz_gpio_level(this.#handle, pin >>> 0);
    return code < 0 ? null : LEVELS[code] ?? null;
  }

  /**
   * Set a named control on a part, such as a sensor's `temperature` or `lux`.
   * Throws when the part has no such control.
   */
  setNamedControl(id, name, value) {
    this.#assertOpen();
    const idBuf = hold(this.#io, ENCODER.encode(String(id)));
    const nameBuf = hold(this.#io, ENCODER.encode(String(name)));
    const valueBuf = hold(this.#io, ENCODER.encode(String(value)));
    let code;
    try {
      code = this.#x.litz_set_named_control(
        this.#handle,
        idBuf.ptr,
        idBuf.len,
        nameBuf.ptr,
        nameBuf.len,
        valueBuf.ptr,
        valueBuf.len,
      );
    } finally {
      this.#io.release(idBuf);
      this.#io.release(nameBuf);
      this.#io.release(valueBuf);
    }
    if (code !== 1) throw this.#io.error(`part ${id} has no control ${name}`);
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

  /**
   * One part's panel by diagram id, such as an e-paper display, or `null` when
   * that part has no framebuffer. The shape is the same as `display()`.
   */
  partDisplay(part) {
    this.#assertOpen();
    if (typeof part !== "string" || part.length === 0) {
      throw new LitzError("partDisplay() needs a part id");
    }
    const infoBuf = this.#io.writeIn(ENCODER.encode(part));
    let info;
    try {
      info = this.#x.litz_part_display_info(this.#handle, infoBuf.ptr, infoBuf.len);
    } finally {
      this.#io.release(infoBuf);
    }
    if (info === 0) return null;
    const width = info >>> 16;
    const height = info & 0xffff;
    const session = this;
    return {
      width,
      height,
      get version() {
        session.#assertOpen();
        const buf = session.#io.writeIn(ENCODER.encode(part));
        try {
          return session.#x.litz_part_display_version(session.#handle, buf.ptr, buf.len);
        } finally {
          session.#io.release(buf);
        }
      },
      pixels() {
        session.#assertOpen();
        const needed = width * height * 4;
        const buf = session.#io.writeIn(ENCODER.encode(part));
        const cap = session.#io.writeOut(needed);
        let written;
        try {
          written = session.#x.litz_part_display_rgba(
            session.#handle,
            buf.ptr,
            buf.len,
            cap.ptr,
            cap.len,
          );
        } finally {
          session.#io.release(buf);
        }
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

  /** One board's display, addressed by list index or board id. */
  displayOn(board = 0) {
    this.#assertOpen();
    const index = this.#boardIndex(board);
    const info = this.#x.litz_board_display_info(this.#handle, index);
    if (info === 0) return null;
    const width = info >>> 16;
    const height = info & 0xffff;
    const session = this;
    return {
      width,
      height,
      get version() {
        session.#assertOpen();
        return session.#x.litz_board_display_version(session.#handle, index);
      },
      pixels() {
        session.#assertOpen();
        const needed = width * height * 4;
        const cap = session.#io.writeOut(needed);
        const written = session.#x.litz_board_display_rgba(
          session.#handle,
          index,
          cap.ptr,
          cap.len,
        );
        if (written !== needed) {
          throw session.#io.error("the display frame could not be read");
        }
        return session.#io.copyOut(written);
      },
    };
  }

  /** The browser-facing debugger, one board at a time. */
  get debug() {
    const session = this;
    return Object.freeze({
      available: (board = 0) => {
        session.#assertOpen();
        return session.#x.litz_debug_available(session.#handle, session.#boardIndex(board)) === 1;
      },
      pc: (board = 0) => {
        session.#assertOpen();
        const pc = session.#x.litz_debug_pc(session.#handle, session.#boardIndex(board));
        return pc < 0 ? null : pc >>> 0;
      },
      registers: (board = 0) => {
        session.#assertOpen();
        const index = session.#boardIndex(board);
        const cap = session.#io.writeOut(33 * 4);
        const written = session.#x.litz_debug_registers(
          session.#handle,
          index,
          cap.ptr,
          cap.len,
        );
        if (written !== 33 * 4) return null;
        const bytes = session.#io.copyOut(written);
        const values = [];
        for (let offset = 0; offset < written; offset += 4) {
          values.push(
            (bytes[offset] |
              (bytes[offset + 1] << 8) |
              (bytes[offset + 2] << 16) |
              (bytes[offset + 3] << 24)) >>>
              0,
          );
        }
        return { values: values.slice(0, 32), pc: values[32] };
      },
      memory: (board, addr, length) => {
        session.#assertOpen();
        const index = session.#boardIndex(board);
        const cap = session.#io.writeOut(Math.max(1, length >>> 0));
        const written = session.#x.litz_debug_memory(
          session.#handle,
          index,
          addr >>> 0,
          cap.ptr,
          cap.len,
        );
        return session.#io.copyOut(written | 0);
      },
      disassemble: (board, addr, count = 8) => {
        session.#assertOpen();
        const index = session.#boardIndex(board);
        return session.#readText(
          (ptr, cap) =>
            session.#x.litz_debug_disassemble(
              session.#handle,
              index,
              addr >>> 0,
              count >>> 0,
              ptr,
              cap,
            ),
          4096,
        );
      },
      setBreakpoints: (board, pcs) => {
        session.#assertOpen();
        const index = session.#boardIndex(board);
        const list = Array.from(pcs, (pc) => pc >>> 0);
        const bytes = new Uint8Array(list.length * 4);
        list.forEach((pc, slot) => {
          bytes[slot * 4] = pc & 0xff;
          bytes[slot * 4 + 1] = (pc >>> 8) & 0xff;
          bytes[slot * 4 + 2] = (pc >>> 16) & 0xff;
          bytes[slot * 4 + 3] = (pc >>> 24) & 0xff;
        });
        const buf = session.#io.writeIn(bytes);
        let code;
        try {
          code = session.#x.litz_debug_breakpoints(
            session.#handle,
            index,
            buf.ptr,
            list.length,
          );
        } finally {
          session.#io.release(buf);
        }
        if (code !== 1) throw session.#io.error("the breakpoints could not be set");
      },
      step: (board = 0) => {
        session.#assertOpen();
        const code = session.#x.litz_debug_step(
          session.#handle,
          session.#boardIndex(board),
        );
        return code < 0 ? "unsupported" : STOPS[code] ?? "unknown";
      },
      run: (board = 0, steps = DEFAULT_STEPS) => {
        session.#assertOpen();
        const code = session.#x.litz_debug_run(
          session.#handle,
          session.#boardIndex(board),
          steps >>> 0,
        );
        return code < 0 ? "unsupported" : STOPS[code] ?? "unknown";
      },
    });
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

  #boardIndex(board) {
    if (typeof board === "number") {
      if (!Number.isInteger(board) || board < 0) {
        throw new LitzError(`there is no board ${board}`);
      }
      return board;
    }
    return this.board(board).index;
  }

  #takeBoardSerial(board) {
    this.#assertOpen();
    const index = this.#boardIndex(board);
    const chunks = [];
    for (;;) {
      const cap = this.#io.writeOut(64 * 1024);
      const written = this.#x.litz_serial_take_board(
        this.#handle,
        index,
        cap.ptr,
        cap.len,
      );
      if (written === 0) break;
      chunks.push(this.#io.copyOut(written));
      if (written < cap.len) break;
    }
    return decodeChunks(chunks);
  }

  #writeBoardSerial(board, data) {
    this.#assertOpen();
    const index = this.#boardIndex(board);
    const bytes = typeof data === "string" ? ENCODER.encode(data) : bytesOf(data, "serial data");
    if (bytes.length === 0) return;
    const buf = this.#io.writeIn(bytes);
    try {
      this.#x.litz_serial_write_board(this.#handle, index, buf.ptr, buf.len);
    } finally {
      this.#io.release(buf);
    }
    this.#throwStored();
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


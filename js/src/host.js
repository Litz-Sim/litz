// The JavaScript half of the `litz_host` import module: custom parts and
// chips run here, the simulator drives them through imports.

import {
  DECODER,
  ENCODER,
  IMAGE_KINDS,
  LEVEL_WORDS,
  PART_STATE_KINDS,
  STOP_CODES,
} from "./constants.js";
import { LitzError } from "./errors.js";

class HostRuntime {
  #memory = null;
  #exports = null;
  #factories = new Map();
  #chipFactories = new Map();
  #instances = new Map();
  #chips = new Map();
  #nextId = 1;
  #lastError = "";
  #callInPtr = 0;
  #callInSize = 0;
  #callOutPtr = 0;
  #callOutSize = 0;

  constructor() {
    const host = this;
    this.imports = {
      part_create: (namePtr, nameLen) => host.#create(namePtr, nameLen),
      part_drop: (id) => host.#instances.delete(id),
      part_set_pin: (id, pin, level, atNs) =>
        host.#part(id)?.setPin(pin, LEVEL_WORDS[level] ?? "floating", Number(atNs)),
      part_advance: (id, toNs) => host.#part(id)?.advance(Number(toNs)),
      part_next_event: (id) => {
        const at = host.#part(id)?.nextEvent();
        return at === null || at === undefined ? -1n : BigInt(Math.round(at));
      },
      part_drive: (id, pin) => {
        const drive = host.#part(id)?.drive(pin);
        return packDrive(drive);
      },
      part_state: (id, ptr, cap) => {
        const [kind, value] = packState(host.#part(id)?.state());
        if (cap < 8) return 0;
        const view = host.#view();
        new DataView(view.buffer, ptr, 8).setUint32(0, kind, true);
        new DataView(view.buffer, ptr, 8).setUint32(4, value, true);
        return 8;
      },
      part_press: (id, pressed) => (host.#part(id)?.press(pressed === 1) ? 1 : 0),
      part_set_control: (id, permille) => (host.#part(id)?.setControl(permille) ? 1 : 0),
      part_analog: (id, pin) => {
        const volts = host.#part(id)?.analog(pin);
        return volts === null || volts === undefined ? Number.NaN : volts;
      },
      part_i2c_connect: (id, address, read) => host.#part(id)?.i2cConnect(address, read === 1),
      part_i2c_read: (id) => host.#part(id)?.i2cRead() ?? 0xff,
      part_i2c_write: (id, byte) => (host.#part(id)?.i2cWrite(byte) ? 1 : 0),
      part_i2c_stop: (id) => host.#part(id)?.i2cStop(),
      part_spi_select: (id, selected) => host.#part(id)?.spiSelect(selected === 1),
      part_spi_transfer: (id, txPtr, txLen, rxLen, outPtr, outCap) => {
        const tx = host.#read(txPtr, txLen);
        const rx = host.#part(id)?.spiTransfer(tx, rxLen) ?? new Uint8Array(0);
        return host.#write(outPtr, outCap, rx);
      },
      part_uart_receive: (id, ptr, len, atNs) =>
        host.#part(id)?.uartReceive(host.#read(ptr, len), Number(atNs)),
      part_uart_poll: (id, outPtr, outCap) => {
        const bytes = host.#part(id)?.uartPoll() ?? new Uint8Array(0);
        return host.#write(outPtr, outCap, bytes);
      },
      host_error: (ptr, cap) => host.#write(ptr, cap, ENCODER.encode(host.#lastError)),

      // ------------------------------------------------------------ chips
      chip_create: (namePtr, nameLen) => host.#createChip(namePtr, nameLen),
      chip_drop: (id) => host.#chips.delete(id),
      chip_info: (id, ptr, cap) => {
        const chip = host.#chip(id);
        if (!chip) return 0;
        try {
          return host.#write(ptr, cap, ENCODER.encode(JSON.stringify(chip.info())));
        } catch (error) {
          host.#lastError = `chip info: ${error?.message ?? error}`;
          return 0;
        }
      },
      chip_load_image: (id, kind, offset, ptr, len) => {
        const chip = host.#chip(id);
        if (!chip) return 0;
        try {
          chip.loadImage(IMAGE_KINDS[kind] ?? "firmware", Number(offset), host.#read(ptr, len));
          return 1;
        } catch (error) {
          host.#lastError = `chip load-image: ${error?.message ?? error}`;
          return 0;
        }
      },
      chip_boot: (id) => {
        const chip = host.#chip(id);
        if (!chip) return -1n;
        try {
          return BigInt(chip.boot() >>> 0);
        } catch (error) {
          host.#lastError = `chip boot: ${error?.message ?? error}`;
          return -1n;
        }
      },
      chip_set_inputs: (id, ptr, count) => {
        const chip = host.#chip(id);
        if (!chip) return;
        const inputs = [];
        for (let index = 0; index < count; index += 1) {
          const base = ptr + index * 8;
          inputs.push({
            pin: host.#u32(base),
            level: LEVEL_WORDS[host.#u32(base + 4)] ?? "floating",
          });
        }
        try {
          chip.setInputs(inputs);
        } catch (error) {
          host.#lastError = `chip set-inputs: ${error?.message ?? error}`;
        }
      },
      chip_run: (id, budget, outPtr) => {
        const chip = host.#chip(id);
        if (!chip) return 0;
        let result;
        try {
          result = chip.run(Number(budget));
        } catch (error) {
          host.#lastError = `chip run: ${error?.message ?? error}`;
          result = { cycles: 0, stop: "trap" };
        }
        const view = new DataView(host.#view().buffer, outPtr, 16);
        view.setBigUint64(0, BigInt(Math.round(result.cycles ?? 0)), true);
        view.setUint32(8, STOP_CODES[result.stop] ?? 0, true);
        return 16;
      },
      chip_take_outputs: (id, ptr, cap) => {
        const chip = host.#chip(id);
        if (!chip) return 0;
        let outputs;
        try {
          outputs = chip.takeOutputs();
        } catch (error) {
          host.#lastError = `chip take-outputs: ${error?.message ?? error}`;
          return 0;
        }
        const count = Math.min(outputs.length, Math.floor(cap / 12));
        const view = new DataView(host.#view().buffer);
        for (let index = 0; index < count; index += 1) {
          const output = outputs[index];
          const base = ptr + index * 12;
          view.setUint32(base, output.pin, true);
          view.setUint32(base + 4, LEVEL_WORDS.indexOf(output.level) < 0 ? 2 : LEVEL_WORDS.indexOf(output.level), true);
          view.setUint32(base + 8, output.enabled === false ? 0 : 1, true);
        }
        return count * 12;
      },
      chip_write_console: (id, ptr, len) => {
        try {
          host.#chip(id)?.writeConsole(host.#read(ptr, len));
        } catch (error) {
          host.#lastError = `chip write-console: ${error?.message ?? error}`;
        }
      },
    };
  }

  /** The memory and exports of the module that imports this host. */
  attach(memory, exports) {
    this.#memory = memory;
    this.#exports = exports;
  }

  /** Release the buffers the 0.2 host-call bridge keeps between calls. */
  release() {
    if (!this.#exports) return;
    if (this.#callInPtr !== 0) this.#exports.litz_free(this.#callInPtr, this.#callInSize);
    if (this.#callOutPtr !== 0) this.#exports.litz_free(this.#callOutPtr, this.#callOutSize);
    this.#callInPtr = 0;
    this.#callInSize = 0;
    this.#callOutPtr = 0;
    this.#callOutSize = 0;
  }

  /**
   * Make module names loadable for one session. Returns what it replaced, so
   * a failed boot can put the previous maps back.
   */
  register({ parts = {}, boards = {} } = {}) {
    const previous = { parts: new Map(), boards: new Map() };
    for (const [name, factory] of Object.entries(parts)) {
      if (typeof factory !== "function") {
        throw new LitzError(`the factory for ${name} must be a function`);
      }
      previous.parts.set(name, this.#factories.get(name));
      this.#factories.set(name, factory);
    }
    for (const [name, factory] of Object.entries(boards)) {
      if (typeof factory !== "function") {
        throw new LitzError(`the factory for ${name} must be a function`);
      }
      previous.boards.set(name, this.#chipFactories.get(name));
      this.#chipFactories.set(name, factory);
    }
    return previous;
  }

  /** Put back the factories `register` replaced. Live instances are kept:
   * sessions drop their own through `part_drop` and `chip_drop`. */
  restore(previous) {
    for (const [name, factory] of previous.parts) {
      if (factory === undefined) this.#factories.delete(name);
      else this.#factories.set(name, factory);
    }
    for (const [name, factory] of previous.boards) {
      if (factory === undefined) this.#chipFactories.delete(name);
      else this.#chipFactories.set(name, factory);
    }
  }

  #part(id) {
    return this.#instances.get(id);
  }

  #chip(id) {
    return this.#chips.get(id);
  }

  #u32(ptr) {
    return new DataView(this.#view().buffer).getUint32(ptr, true);
  }

  /**
   * The services a chip adapter calls while it runs. `console` and `fault`
   * are the guest's two host imports; both are buffered inside the module,
   * which is where the session reads them from.
   */
  #chipHost(id) {
    return {
      console: (bytes) => {
        const data = typeof bytes === "string" ? ENCODER.encode(bytes) : bytes;
        if (data.length === 0) return;
        const ptr = this.#exports.litz_alloc(data.length);
        if (ptr === 0) return;
        try {
          this.#view().set(data, ptr);
          this.#exports.litz_guest_console(id, ptr, data.length);
        } finally {
          this.#exports.litz_free(ptr, data.length);
        }
      },
      fault: (detail) => {
        const data = ENCODER.encode(String(detail));
        const ptr = this.#exports.litz_alloc(data.length);
        if (ptr === 0) return;
        try {
          this.#view().set(data, ptr);
          this.#exports.litz_guest_fault(id, ptr, data.length);
        } finally {
          this.#exports.litz_free(ptr, data.length);
        }
      },
      /** A 0.2 host call, answered by the circuit inside the module. */
      call: (op, args) => this.#guestCall(id, op, args),
    };
  }

  /**
   * Ask the module to answer one `litz:mcu@0.2.0` host call. The module
   * returns the reply's full length; when the buffer was too small, it is
   * grown and the call is repeated.
   */
  #guestCall(id, op, args) {
    const exports = this.#exports;
    if (!exports || typeof exports.litz_guest_call !== "function") {
      throw new LitzError("this module was built without the host-call bridge");
    }
    const opBytes = ENCODER.encode(op);
    const argBytes = ENCODER.encode(JSON.stringify(args ?? {}));
    const need = opBytes.length + argBytes.length;
    if (this.#callInSize < need) {
      if (this.#callInPtr !== 0) exports.litz_free(this.#callInPtr, this.#callInSize);
      this.#callInSize = Math.max(need, 256);
      this.#callInPtr = exports.litz_alloc(this.#callInSize);
      if (this.#callInPtr === 0) {
        this.#callInSize = 0;
        throw new LitzError(`could not allocate ${need} bytes for a host call`);
      }
    }
    const input = this.#view();
    input.set(opBytes, this.#callInPtr);
    input.set(argBytes, this.#callInPtr + opBytes.length);
    for (;;) {
      if (this.#callOutSize === 0) {
        this.#callOutSize = 4096;
        this.#callOutPtr = exports.litz_alloc(this.#callOutSize);
        if (this.#callOutPtr === 0) {
          this.#callOutSize = 0;
          throw new LitzError("could not allocate a host-call reply buffer");
        }
      }
      const length = exports.litz_guest_call(
        id,
        this.#callInPtr,
        opBytes.length,
        this.#callInPtr + opBytes.length,
        argBytes.length,
        this.#callOutPtr,
        this.#callOutSize,
      );
      if (length > this.#callOutSize) {
        exports.litz_free(this.#callOutPtr, this.#callOutSize);
        this.#callOutSize = length;
        this.#callOutPtr = exports.litz_alloc(length);
        if (this.#callOutPtr === 0) {
          this.#callOutSize = 0;
          throw new LitzError(`could not allocate ${length} bytes for a host-call reply`);
        }
        continue;
      }
      const text = DECODER.decode(
        this.#view().subarray(this.#callOutPtr, this.#callOutPtr + length),
      );
      let reply;
      try {
        reply = JSON.parse(text);
      } catch (error) {
        throw new LitzError(`host call ${op} returned invalid JSON: ${error.message}`);
      }
      if (!reply.ok) throw new LitzError(reply.error);
      return reply.value;
    }
  }

  #createChip(namePtr, nameLen) {
    this.#lastError = "";
    const name = DECODER.decode(this.#read(namePtr, nameLen));
    const factory = this.#chipFactories.get(name);
    if (!factory) {
      this.#lastError = `the host has no runtime for module ${name}`;
      return 0;
    }
    const id = this.#nextId;
    this.#nextId += 1;
    let chip;
    try {
      chip = factory(this.#chipHost(id));
    } catch (error) {
      this.#lastError = `instantiating ${name} failed: ${error?.message ?? error}`;
      return 0;
    }
    if (!chip || typeof chip !== "object") {
      this.#lastError = `the factory for ${name} did not return a chip`;
      return 0;
    }
    this.#chips.set(id, chip);
    return id;
  }

  #create(namePtr, nameLen) {
    this.#lastError = "";
    const name = DECODER.decode(this.#read(namePtr, nameLen));
    const factory = this.#factories.get(name);
    if (!factory) {
      this.#lastError = `the host has no runtime for module ${name}`;
      return 0;
    }
    let instance;
    try {
      instance = factory();
    } catch (error) {
      this.#lastError = `instantiating ${name} failed: ${error?.message ?? error}`;
      return 0;
    }
    if (!instance || typeof instance !== "object") {
      this.#lastError = `the factory for ${name} did not return a part`;
      return 0;
    }
    const id = this.#nextId;
    this.#nextId += 1;
    this.#instances.set(id, instance);
    return id;
  }

  #view() {
    if (!this.#memory) throw new LitzError("the host bridge is not attached to a module");
    return new Uint8Array(this.#memory.buffer);
  }

  #read(ptr, len) {
    return new Uint8Array(this.#view().slice(ptr, ptr + len));
  }

  #write(ptr, cap, bytes) {
    const n = Math.min(cap, bytes.length);
    if (n > 0) this.#view().set(bytes.subarray(0, n), ptr);
    return n;
  }
}

function packDrive(drive) {
  if (!drive) return 2 | (1 << 8);
  if (typeof drive === "string") {
    const level = LEVEL_WORDS.indexOf(drive);
    return (level < 0 ? 2 : level) | (1 << 8);
  }
  const level = LEVEL_WORDS.indexOf(drive.level);
  const strength = drive.strength === "weak" ? 1 : 0;
  return (level < 0 ? 2 : level) | (strength << 8);
}

function packState(state) {
  if (!state) return [0, 0];
  const kind = PART_STATE_KINDS[state.kind] ?? 0;
  return [kind, state.value ?? 0];
}

export { HostRuntime };

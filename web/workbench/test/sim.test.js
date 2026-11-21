import assert from "node:assert/strict";
import test from "node:test";

import {
  appendSerial,
  createRunController,
  DEFAULT_SPEED,
  formatMs,
  formatRate,
  resolveWasmUrl,
  SimError,
  stepsPerFrameFor,
} from "../src/sim.js";

const ENCODER = new TextEncoder();
const DECODER = new TextDecoder();

/**
 * A stand-in for the module's export table. It keeps a real linear-memory
 * buffer so the controller's pointer arithmetic is exercised, not mocked.
 */
function fakeWasm(options = {}) {
  const memory = { buffer: new ArrayBuffer(1024 * 1024) };
  let next = 4096;
  let handle = 0;
  let error = "";
  const state = {
    newCalls: 0,
    drops: 0,
    runCalls: 0,
    lastSteps: 0,
    ms: 0,
    diagram: "",
    appLen: 0,
    romLen: 0,
    bootLen: 0,
    ptableLen: 0,
    serialQueue: new Uint8Array(0),
    written: [],
    presses: [],
    controls: [],
    parts: new Map(options.parts ?? []),
    values: new Map(options.values ?? []),
    conflicts: 0,
    conflictText: "short between GPIO8 and GND",
    probes: [],
    failProbe: options.failProbe ?? "",
    decodes: [],
    failDecode: options.failDecode ?? "",
    scopeText: options.scopeText ?? '{"ms":0,"probes":[]}',
    failNew: Boolean(options.failNew),
    failPress: options.failPress ?? null,
    throwOnRun: Boolean(options.throwOnRun),
    frees: [],
    allocations: [],
  };

  const bytesAt = (ptr, len) => new Uint8Array(memory.buffer, ptr, len);
  const write = (ptr, bytes) => bytesAt(ptr, bytes.length).set(bytes);
  const readString = (ptr, len) => DECODER.decode(bytesAt(ptr, len).slice());

  const exports = {
    memory,
    litz_alloc(len) {
      const ptr = next;
      next += Math.max(1, len);
      state.allocations.push({ ptr, len });
      return ptr;
    },
    litz_free(ptr, len) {
      state.frees.push({ ptr, len });
    },
    litz_new(dp, dl, rp, rl, bp, bl, pp, pl, ap, al) {
      state.newCalls += 1;
      state.diagram = readString(dp, dl);
      state.romLen = rl;
      state.bootLen = bl;
      state.ptableLen = pl;
      state.appLen = al;
      if (state.failNew) {
        error = "no application image: app_len is 0";
        return 0;
      }
      handle = 1;
      return handle;
    },
    litz_drop(target) {
      state.drops += 1;
      if (target === handle) handle = 0;
    },
    litz_last_error(dst, cap) {
      const bytes = ENCODER.encode(error);
      const take = Math.min(bytes.length, cap);
      write(dst, bytes.subarray(0, take));
      return take;
    },
    litz_run(target, steps) {
      if (state.throwOnRun) throw new Error("board exploded");
      state.runCalls += 1;
      state.lastSteps = steps;
      state.ms += 1;
      error = "";
      return state.ms;
    },
    litz_serial_take(target, dst, cap) {
      const take = Math.min(cap, state.serialQueue.length);
      const chunk = state.serialQueue.subarray(0, take);
      write(dst, chunk);
      state.serialQueue = state.serialQueue.subarray(take);
      return take;
    },
    litz_serial_write(target, ptr, len) {
      state.written.push(readString(ptr, len));
      error = "";
    },
    litz_part_state(target, ptr, len) {
      return state.parts.get(readString(ptr, len)) ?? -1;
    },
    litz_part_value(target, ptr, len) {
      return state.values.get(readString(ptr, len)) ?? 0;
    },
    litz_press(target, ptr, len, down) {
      const id = readString(ptr, len);
      state.presses.push([id, down]);
      if (state.failPress === id) {
        error = `part ${JSON.stringify(id)} does not take a press; it is not a switch`;
        return;
      }
      error = "";
    },
    litz_set_control(target, ptr, len, permille) {
      const id = readString(ptr, len);
      state.controls.push([id, permille]);
      error = "";
    },
    litz_part_count() {
      return state.parts.size;
    },
    litz_part_id(target, index, dst, cap) {
      const id = [...state.parts.keys()][index];
      if (id === undefined) return 0;
      const bytes = ENCODER.encode(id);
      write(dst, bytes.subarray(0, cap));
      return Math.min(bytes.length, cap);
    },
    litz_conflict_count() {
      return state.conflicts;
    },
    litz_conflict(target, index, dst, cap) {
      const bytes = ENCODER.encode(state.conflictText);
      write(dst, bytes.subarray(0, cap));
      return Math.min(bytes.length, cap);
    },
    litz_probe_add(target, namePtr, nameLen, pointPtr, pointLen) {
      state.probes.push({
        name: readString(namePtr, nameLen),
        point: readString(pointPtr, pointLen),
      });
      if (state.failProbe) {
        error = state.failProbe;
        return 0;
      }
      error = "";
      return 1;
    },
    litz_decode_add(target, ptr, len) {
      state.decodes.push(readString(ptr, len));
      if (state.failDecode) {
        error = state.failDecode;
        return 0;
      }
      error = "";
      return 1;
    },
    litz_scope_len() {
      return ENCODER.encode(state.scopeText).length;
    },
    litz_scope_json(target, dst, cap) {
      const bytes = ENCODER.encode(state.scopeText);
      const take = Math.min(bytes.length, cap);
      write(dst, bytes.subarray(0, take));
      return take;
    },
  };

  return { exports, state };
}

function framePair() {
  const pending = new Map();
  let next = 1;
  return {
    requestFrame(callback) {
      const id = next++;
      pending.set(id, callback);
      return id;
    },
    cancelFrame(id) {
      pending.delete(id);
    },
    pending,
  };
}

async function loaded(options = {}) {
  const fake = fakeWasm(options);
  const frames = framePair();
  const controller = createRunController({
    wasm: fake.exports,
    requestFrame: frames.requestFrame,
    cancelFrame: frames.cancelFrame,
    now: () => 0,
    maxSerialChars: options.maxSerialChars ?? 64,
    onError: options.onError,
    onFrame: options.onFrame,
  });
  await controller.load({
    diagramText: '{"version":1,"parts":[],"connections":[]}',
    app: new Uint8Array([1, 2, 3]),
  });
  return { controller, fake, frames };
}

test("sim: load passes the diagram and image lengths through the ABI", async () => {
  const fake = fakeWasm();
  const controller = createRunController({ wasm: fake.exports });
  await controller.load({
    diagramText: '{"version":1,"parts":[],"connections":[]}',
    app: new Uint8Array([1, 2, 3, 4]),
    rom: new Uint8Array([0xaa]),
  });
  assert.equal(fake.state.newCalls, 1);
  assert.equal(fake.state.diagram, '{"version":1,"parts":[],"connections":[]}');
  assert.equal(fake.state.appLen, 4);
  assert.equal(fake.state.romLen, 1);
  assert.equal(fake.state.bootLen, 0);
  assert.equal(controller.hasSession(), true);
  assert.equal(controller.state(), "paused");
});

test("sim: load refuses a missing application and an unpaired bootloader", async () => {
  const fake = fakeWasm();
  const controller = createRunController({ wasm: fake.exports });
  await assert.rejects(
    () => controller.load({ diagramText: "{}", app: new Uint8Array(0) }),
    (error) => error instanceof SimError && /no application image/.test(error.message),
  );
  await assert.rejects(
    () => controller.load({
      diagramText: "{}",
      app: new Uint8Array([1]),
      boot: new Uint8Array([2]),
    }),
    /supplied together/,
  );
  assert.equal(fake.state.newCalls, 0);
});

test("sim: litz_new returning zero surfaces litz_last_error", async () => {
  const fake = fakeWasm({ failNew: true });
  const controller = createRunController({ wasm: fake.exports });
  await assert.rejects(
    () => controller.load({ diagramText: "{}", app: new Uint8Array([1]) }),
    /no application image/,
  );
  assert.equal(controller.hasSession(), false);
});

test("sim: run and pause change state without rebuilding the session", async () => {
  const { controller, fake, frames } = await loaded();
  controller.run();
  assert.equal(controller.state(), "running");
  assert.equal(frames.pending.size, 1, "one frame is scheduled");

  controller.tick(1000);
  assert.equal(fake.state.runCalls, 1);
  assert.equal(controller.simMs, 1);

  controller.pause();
  assert.equal(controller.state(), "paused");
  assert.equal(frames.pending.size, 0, "pausing cancels the frame");
  controller.tick(2000);
  assert.equal(fake.state.runCalls, 1, "a paused controller does not step");

  controller.toggle();
  assert.equal(controller.state(), "running");
  controller.tick(3000);
  assert.equal(fake.state.runCalls, 2);
  assert.equal(fake.state.newCalls, 1, "no reload on resume");
});

test("sim: every frame steps exactly the selected speed", async () => {
  const { controller, fake } = await loaded();
  assert.equal(controller.setSpeed(1_600_000), 1_600_000);
  controller.run();
  controller.tick(500);
  assert.equal(fake.state.lastSteps, 1_600_000);

  controller.setSpeed(100_000);
  controller.tick(1000);
  assert.equal(fake.state.lastSteps, 100_000);
  assert.equal(controller.status().stepsPerFrame, 100_000);
});

test("sim: speed values normalise to positive integers", () => {
  assert.equal(stepsPerFrameFor(100_000), 100_000);
  assert.equal(stepsPerFrameFor(400_000), 400_000);
  assert.equal(stepsPerFrameFor(1_600_000), 1_600_000);
  assert.equal(stepsPerFrameFor(-1), DEFAULT_SPEED);
  assert.equal(stepsPerFrameFor("nope"), DEFAULT_SPEED);
  assert.equal(stepsPerFrameFor(2.9), 2);
});

test("sim: serial is delivered once and the retained log is bounded", async () => {
  const { controller, fake } = await loaded({ maxSerialChars: 8 });
  fake.state.serialQueue = ENCODER.encode("hello world");
  controller.tick(100);
  assert.equal(controller.takeSerial(), "hello world");
  assert.equal(controller.takeSerial(), "", "a drained buffer reports nothing new");
  assert.equal(controller.serial, "lo world", "only the tail is retained");
});

test("sim: appendSerial keeps the newest characters", () => {
  assert.equal(appendSerial("", "abc", 10), "abc");
  assert.equal(appendSerial("abc", "", 10), "abc");
  assert.equal(appendSerial("abcdef", "gh", 4), "efgh");
  assert.equal(appendSerial("abc", "def", 6), "abcdef");
});

test("sim: typing into UART0 reaches litz_serial_write", async () => {
  const { controller, fake } = await loaded();
  assert.equal(controller.writeSerial("PING\n"), true);
  assert.deepEqual(fake.state.written, ["PING\n"]);
  assert.equal(controller.writeSerial(""), false);
});

test("sim: part state, value, press and control go through the ABI", async () => {
  const { controller, fake } = await loaded({
    parts: [["led1", 2], ["btn1", 3], ["pot1", 11]],
    values: [["pot1", 750]],
  });
  assert.equal(controller.partState("led1"), 2);
  assert.equal(controller.partState("missing"), -1);
  assert.equal(controller.partValue("pot1"), 750);

  assert.equal(controller.press("btn1", true), true);
  assert.equal(controller.press("btn1", false), true);
  assert.deepEqual(fake.state.presses, [["btn1", 1], ["btn1", 0]]);

  assert.equal(controller.setControl("pot1", 1250), true);
  assert.deepEqual(fake.state.controls, [["pot1", 1000]], "permille is clamped");
  assert.deepEqual(controller.partIds(), ["led1", "btn1", "pot1"]);
});

test("sim: a rejected press reports the ABI message and does not stop the run", async () => {
  const { controller, fake } = await loaded({ parts: [["led1", 2]], failPress: "led1" });
  controller.run();
  assert.equal(controller.press("led1", true), false);
  assert.match(controller.error, /does not take a press/);
  assert.equal(controller.state(), "running", "input errors are not fatal");
  controller.tick(1000);
  assert.equal(fake.state.runCalls, 1);
});

test("sim: addProbe writes the name and point through the ABI", async () => {
  const { controller, fake } = await loaded();
  assert.equal(controller.addProbe("led", "esp:GPIO8"), true);
  assert.deepEqual(fake.state.probes, [{ name: "led", point: "esp:GPIO8" }]);
  assert.ok(
    fake.state.frees.some(({ len }) => len === "led".length),
    "the name buffer is released",
  );
  assert.ok(
    fake.state.frees.some(({ len }) => len === "esp:GPIO8".length),
    "the point buffer is released",
  );
});

test("sim: a refused probe throws the wasm message", async () => {
  const { controller, fake } = await loaded({
    failProbe: 'probe "led" is already in use',
  });
  assert.throws(
    () => controller.addProbe("led", "esp:GPIO8"),
    (error) => error instanceof SimError && /already in use/.test(error.message),
  );
  assert.match(controller.error, /already in use/);
  assert.equal(fake.state.probes.length, 1, "the call still reached the module");
});

test("sim: addDecode writes the spec through the ABI", async () => {
  const { controller, fake } = await loaded();
  const spec = "uart:rx=tx,baud=115200";
  assert.equal(controller.addDecode(spec), true);
  assert.deepEqual(fake.state.decodes, [spec]);
  assert.ok(
    fake.state.frees.some(({ len }) => len === spec.length),
    "the spec buffer is released",
  );
});

test("sim: a refused decoder throws the wasm message", async () => {
  const { controller, fake } = await loaded({
    failDecode: 'decoder "i2c" is not known',
  });
  assert.throws(
    () => controller.addDecode("i2c:sda=a"),
    (error) => error instanceof SimError && /not known/.test(error.message),
  );
  assert.match(controller.error, /not known/);
  assert.equal(fake.state.decodes.length, 1, "the call still reached the module");
});

test("sim: the module must export litz_decode_add", async () => {
  const fake = fakeWasm();
  delete fake.exports.litz_decode_add;
  const controller = createRunController({ wasm: fake.exports });
  await assert.rejects(
    () => controller.init(),
    (error) => error instanceof SimError && /missing: litz_decode_add/.test(error.message),
  );
});

test("sim: addProbe and scope need a session", async () => {
  const fake = fakeWasm();
  const controller = createRunController({ wasm: fake.exports });
  await controller.init();
  assert.throws(
    () => controller.addProbe("led", "esp:GPIO8"),
    (error) => error instanceof SimError && /no session/.test(error.message),
  );
  assert.equal(controller.scope(), null);
  assert.equal(fake.state.probes.length, 0);
});

test("sim: scope parses the document the module returns", async () => {
  const text = JSON.stringify({
    ms: 5,
    probes: [
      {
        name: "led",
        point: "esp:GPIO8",
        net: 3,
        edges: 2,
        high_ns: 1_000_000,
        low_ns: 1_000_000,
        frequency_hz: 500,
        duty: 0.5,
        min_volts: 0,
        max_volts: 3.3,
        digital: [[0, 2], [1_000_000, 1]],
        volts: [],
      },
    ],
  });
  const { controller } = await loaded({ scopeText: text });
  const document = controller.scope();
  assert.equal(document.ms, 5);
  assert.equal(document.probes[0].name, "led");
  assert.deepEqual(document.probes[0].digital, [[0, 2], [1_000_000, 1]]);
});

test("sim: an exception while stepping stops the loop and reports the error", async () => {
  const errors = [];
  const { controller, fake } = await loaded({
    throwOnRun: true,
    onError: (message) => errors.push(message),
  });
  controller.run();
  controller.tick(1000);
  assert.equal(controller.state(), "error");
  assert.equal(controller.running, false);
  assert.match(controller.error, /board exploded/);
  assert.deepEqual(errors, ["board exploded"]);
  controller.tick(2000);
  assert.equal(fake.state.runCalls, 0, "a failed controller does not step again");
});

test("sim: reset drops the old handle and boots the same images again", async () => {
  const { controller, fake } = await loaded();
  controller.run();
  controller.tick(1000);
  assert.equal(controller.simMs, 1);

  await controller.reset();
  assert.equal(fake.state.drops, 1);
  assert.equal(fake.state.newCalls, 2);
  assert.equal(controller.handle, 1);
  assert.equal(controller.simMs, 0);
  assert.equal(controller.state(), "paused");
  assert.equal(fake.state.appLen, 3);
});

test("sim: unload drops the session but keeps the module", async () => {
  const { controller, fake } = await loaded();
  controller.unload();
  assert.equal(controller.hasSession(), false);
  assert.equal(fake.state.drops, 1);
  assert.equal(controller.state(), "empty");
  await assert.rejects(() => controller.reset(), /no session/);
});

test("sim: dispose frees the session and the scratch buffer once", async () => {
  const { controller, fake } = await loaded();
  const scratch = controller.scratch;
  controller.dispose();
  assert.equal(fake.state.drops, 1);
  assert.ok(fake.state.frees.some(({ ptr, len }) => ptr === scratch && len === controller.scratchBytes));
  assert.equal(controller.wasm, null);
  controller.dispose();
  assert.equal(fake.state.drops, 1, "dispose is idempotent");
});

test("sim: conflict text is read once and cleared when the count changes", async () => {
  const { controller, fake } = await loaded();
  fake.state.conflicts = 1;
  controller.tick(100);
  assert.equal(controller.conflicts, 1);
  assert.equal(controller.firstConflict, "short between GPIO8 and GND");
  fake.state.conflicts = 0;
  controller.tick(200);
  assert.equal(controller.conflicts, 0);
  assert.equal(controller.firstConflict, "");
});

test("sim: the wasm URL is resolved against the workbench page", () => {
  assert.equal(
    resolveWasmUrl("http://127.0.0.1:8000/workbench/index.html"),
    "http://127.0.0.1:8000/litz_wasm.wasm",
  );
  assert.equal(resolveWasmUrl(), "../litz_wasm.wasm");
});

test("sim: status formatting is stable", () => {
  assert.equal(formatMs(0), "0 ms");
  assert.equal(formatMs(999), "999 ms");
  assert.equal(formatMs(1234), "1.234 s");
  assert.equal(formatMs(61_500), "1m 1.50s");
  assert.equal(formatRate(0), "0");
  assert.equal(formatRate(400_000), "400k");
  assert.equal(formatRate(1_600_000), "1.60M");
});
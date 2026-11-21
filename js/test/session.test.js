// The library against the real module and real firmware images. Every test
// loads the shipped `litz_wasm.wasm`, so a broken build fails here first.

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";

import { DEFAULT_STEPS, LitzError, loadLitz, wasmUrl } from "../index.js";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

async function fixture(relative) {
  return new Uint8Array(await readFile(path.join(repoRoot, relative)));
}

async function blinky(litz) {
  return litz.createSession({
    diagram: new TextDecoder().decode(await fixture("examples/blinky/diagram.json")),
    app: await fixture("examples/blinky/firmware/blinky.bin"),
  });
}

async function echo(litz) {
  return litz.createSession({
    diagram: new TextDecoder().decode(await fixture("examples/echo/diagram.json")),
    app: await fixture("examples/echo/firmware/echo.bin"),
  });
}

let litz;

before(async () => {
  litz = await loadLitz();
});

after(() => {
  litz?.close();
});

describe("loading", () => {
  test("the shipped module is the one the package points at", () => {
    assert.match(wasmUrl, /litz_wasm\.wasm$/);
  });

  test("the runtime exposes the ABI it was built from", () => {
    for (const name of ["litz_new", "litz_run", "litz_serial_take", "litz_probe_add"]) {
      assert.equal(typeof litz.exports[name], "function", name);
    }
    assert.ok(litz.exports.memory instanceof WebAssembly.Memory);
  });

  test("a module without the ABI is rejected before it runs", async () => {
    const empty = new WebAssembly.Module(
      new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]),
    );
    await assert.rejects(loadLitz({ wasm: empty }), LitzError);
  });
});

describe("a session", () => {
  test("runs a firmware and collects its console", async () => {
    const session = await blinky(litz);
    try {
      let serial = "";
      for (let i = 0; i < 20 && !serial.includes("litz: blinky"); i += 1) {
        session.run(DEFAULT_STEPS);
        serial += session.serial.take();
      }
      assert.match(serial, /litz: blinky/);
      assert.ok(session.ms > 0, "the clock advanced");
    } finally {
      session.close();
    }
  });

  test("reports the parts the diagram declares", async () => {
    const session = await blinky(litz);
    try {
      session.advance(300);
      const parts = session.parts();
      assert.deepEqual(
        parts.map((part) => part.id),
        ["esp", "r1", "led1"],
      );
      assert.equal(session.part("esp"), null, "a board has no runtime state to report");
      assert.equal(parts.find((part) => part.id === "esp")?.state, null);
      const led = session.part("led1");
      assert.equal(led?.kind, "led");
      assert.ok(["on", "off"].includes(led.state), `unexpected LED state ${led.state}`);
    } finally {
      session.close();
    }
  });

  test("blinks: the LED is seen on and off", async () => {
    const session = await blinky(litz);
    try {
      const seen = new Set();
      for (let i = 0; i < 40 && seen.size < 2; i += 1) {
        session.advance(25);
        seen.add(session.part("led1")?.state);
      }
      assert.deepEqual([...seen].sort(), ["off", "on"]);
    } finally {
      session.close();
    }
  });

  test("takes console input and the firmware echoes it", async () => {
    const session = await echo(litz);
    try {
      session.advance(200);
      assert.match(session.serial.take(), /litz: echo/);
      session.serial.write("hello\r");
      let serial = "";
      for (let i = 0; i < 20 && !serial.includes("hello"); i += 1) {
        session.advance(20);
        serial += session.serial.take();
      }
      assert.match(serial, /hello/);
    } finally {
      session.close();
    }
  });

  test("a probe records samples that the scope document carries", async () => {
    const session = await blinky(litz);
    try {
      session.probe.add("led", "led1:A");
      session.trace.start();
      session.advance(400);
      const scope = session.scope();
      assert.ok(Array.isArray(scope.probes), "scope.probes is an array");
      const probe = scope.probes.find((entry) => entry.name === "led");
      assert.ok(probe, "the probe is in the document");
      assert.ok(probe.digital.length > 0, "the probe recorded level transitions");
    } finally {
      session.close();
    }
  });

  test("has no display when the circuit has none", async () => {
    const session = await blinky(litz);
    try {
      assert.equal(session.display(), null);
      assert.equal(session.touch(10, 10, true), false, "no panel, no touch");
    } finally {
      session.close();
    }
  });

  test("an unknown part is null, not an error", async () => {
    const session = await blinky(litz);
    try {
      assert.equal(session.part("nope"), null);
    } finally {
      session.close();
    }
  });

  test("pressing a part that is not a button is rejected with the reason", async () => {
    const session = await blinky(litz);
    try {
      assert.throws(() => session.press("led1", true), LitzError);
    } finally {
      session.close();
    }
  });

  test("a closed session refuses further calls", async () => {
    const session = await blinky(litz);
    session.close();
    assert.throws(() => session.run(1), LitzError);
  });
});

describe("creating sessions", () => {
  test("rejects an empty diagram with a usable message", () => {
    assert.throws(() => litz.createSession({ diagram: "", app: new Uint8Array([1]) }), LitzError);
  });

  test("reports why a diagram cannot be flattened", async () => {
    const app = await fixture("examples/blinky/firmware/blinky.bin");
    assert.throws(
      () => litz.createSession({ diagram: '{"parts": "nope"}', app }),
      (error) => error instanceof LitzError && error.message.length > 0,
    );
  });
});

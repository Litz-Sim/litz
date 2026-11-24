// The library against the real module and real firmware images. Every test
// loads the shipped `litz_wasm.wasm`, so a broken build fails here first.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, describe, test } from "node:test";

import {
  chipFromJco,
  chipImports,
  DEFAULT_STEPS,
  LitzError,
  loadLitz,
  partFromJco,
  partImports,
  wasmUrl,
} from "../index.js";
import { instantiate as instantiate74hc595 } from "./fixtures/74hc595/74hc595.js";
import { instantiate as instantiateBoardV2 } from "./fixtures/board-v2/board-v2.js";

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

async function pico(litz) {
  return litz.createSession({
    diagram: new TextDecoder().decode(await fixture("examples/pico-blink/diagram.json")),
    app: await fixture("examples/pico-blink/firmware/pico-blink.bin"),
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
    for (const name of [
      "litz_new",
      "litz_new_json",
      "litz_run",
      "litz_serial_take",
      "litz_probe_add",
      "litz_chip_info",
      "litz_stop",
      "litz_fault",
      "litz_part_pin_level",
      "litz_gpio_level",
      "litz_set_named_control",
      "litz_heap",
      "litz_guest_call",
    ]) {
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

  test("reads a part's own panel, which the board-level display does not cover", async () => {
    const diagram = JSON.stringify({
      version: 1,
      parts: [
        { id: "esp", type: "board-esp32-c3-devkitm-1", attrs: {} },
        { id: "paper", type: "litz-epaper-1in54", attrs: {} },
      ],
      connections: [
        ["esp:GPIO7", "paper:CS", "", []],
        ["esp:GPIO8", "paper:CLK", "", []],
        ["esp:GPIO9", "paper:DIN", "", []],
        ["esp:3V3", "paper:VCC", "", []],
        ["esp:GND.1", "paper:GND", "", []],
      ],
    });
    const session = litz.createSession({
      diagram,
      app: await fixture("examples/blinky/firmware/blinky.bin"),
    });
    try {
      session.run(1000);
      assert.equal(session.display(), null, "the board has no first display here");
      const paper = session.partDisplay("paper");
      assert.ok(paper, "the e-paper part is a whole-frame panel");
      assert.equal(paper.width, 200);
      assert.equal(paper.height, 200);
      assert.equal(typeof paper.version, "number");
      const pixels = paper.pixels();
      assert.equal(pixels.length, 200 * 200 * 4);
      // The panel boots all white, so the first pixel is opaque white.
      assert.deepEqual(Array.from(pixels.slice(0, 4)), [255, 255, 255, 255]);
      assert.equal(session.partDisplay("esp"), null, "a board is not a panel");
      assert.equal(session.partDisplay("nope"), null, "an unknown id has no panel");
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

  test("reports the chip and its flash layout", async () => {
    const session = await blinky(litz);
    try {
      const chip = session.chip();
      assert.equal(chip.name, "esp32c3");
      assert.ok(chip.cpuHz > 0, "the clock is reported");
      assert.equal(chip.flash.app, 0x10000);
      assert.ok(chip.flash.size > 0, "the flash has a size");
    } finally {
      session.close();
    }
  });

  test("reports how the last run ended and can read pins", async () => {
    const session = await blinky(litz);
    try {
      session.run(20_000);
      assert.ok(["budget", "idle"].includes(session.stop), session.stop);
      assert.equal(session.fault, null, "a healthy run has no fault");
      assert.match(session.partPin("led1", "A"), /^(low|high|high-z)$/);
      assert.match(session.gpio(8), /^(low|high|high-z)$/);
      assert.equal(session.partPin("nope", "A"), null);
      assert.equal(session.gpio(200), null, "an ESP32-C3 has no GPIO200");
      const heap = session.heap();
      assert.ok(heap === null || Number.isInteger(heap), "heap is a number or null");
    } finally {
      session.close();
    }
  });

  test("a Pico diagram boots the RP2040 backend", async () => {
    const session = await pico(litz);
    try {
      assert.equal(session.chip().name, "rp2040");
      let serial = "";
      for (let i = 0; i < 40 && !serial.includes("litz: pico"); i += 1) {
        session.advance(20);
        serial += session.serial.take();
      }
      assert.match(serial, /litz: pico/);
      assert.equal(session.part("led1")?.kind, "led");
    } finally {
      session.close();
    }
  });
});

describe("jco adapters", () => {
  test("a transpiled part's BigInt times and tagged state are translated", () => {
    const calls = [];
    const part = {
      info: () => ({ name: "x", description: "", pins: [] }),
      setPin: (pin, level, at) => calls.push(["setPin", pin, level, at]),
      advance: (at) => calls.push(["advance", at]),
      nextEvent: () => undefined,
      drive: (pin) => ({ level: "high", strength: "strong" }),
      state: () => ({ tag: "tone", val: { sounding: true, hz: 440 } }),
      press: () => true,
      setControl: () => false,
    };
    const wrapped = partFromJco({ part });
    wrapped.setPin(3, "high", 1500);
    wrapped.advance(2500);
    assert.deepEqual(calls[0], ["setPin", 3, "high", 1500n]);
    assert.deepEqual(calls[1], ["advance", 2500n]);
    assert.equal(wrapped.nextEvent(), null);
    assert.deepEqual(wrapped.drive(0), { level: "high", strength: "strong" });
    assert.deepEqual(wrapped.state(), { kind: "tone-sounding", value: 440 });
  });
});

describe("custom parts", () => {
  const INVERTER = `[part]
id = "test-inverter"
name = "Test inverter"
version = "1.0.0"
description = "A gate the JS host implements"

[pins.IN]
kind = "digital-in"
number = 1

[pins.OUT]
kind = "digital-out"
number = 2

[simulation]
backend = "wasm"
module = "test-inverter.wasm"
abi = "litz-part-2"
`;

  const DIAGRAM = JSON.stringify({
    version: 1,
    parts: [
      { id: "esp", type: "board-esp32-c3-devkitm-1", attrs: {} },
      { id: "inv", type: "test-inverter", attrs: {} },
      { id: "r1", type: "litz-resistor", attrs: { value: "220" } },
      { id: "led1", type: "litz-led", attrs: { color: "green" } },
    ],
    connections: [
      ["esp:GPIO8", "inv:IN", "", []],
      ["inv:OUT", "r1:1", "", []],
      ["r1:2", "led1:A", "", []],
      ["led1:C", "esp:GND.1", "", []],
    ],
  });

  function inverter() {
    let level = "floating";
    return {
      setPin(pin, next) {
        if (pin === 0) level = next;
      },
      drive(pin) {
        if (pin !== 1) return "floating";
        return level === "high" ? "low" : "high";
      },
      state() {
        return { kind: level === "high" ? "led-off" : "led-on" };
      },
      nextEvent() {
        return null;
      },
      advance() {},
      press() {
        return false;
      },
      setControl() {
        return false;
      },
      analog() {
        return null;
      },
      i2cConnect() {},
      i2cRead() {
        return 0;
      },
      i2cWrite() {
        return false;
      },
      i2cStop() {},
      spiSelect() {},
      spiTransfer(_tx, rxLen) {
        return new Uint8Array(rxLen);
      },
      uartReceive() {},
      uartPoll() {
        return new Uint8Array(0);
      },
    };
  }

  test("a host part is instantiated and drives its net", async () => {
    const factoryCalls = [];
    const session = litz.createSession({
      diagram: DIAGRAM,
      app: await fixture("examples/blinky/firmware/blinky.bin"),
      manifests: [INVERTER],
      parts: {
        "test-inverter.wasm": () => {
          factoryCalls.push("created");
          return inverter();
        },
      },
    });
    try {
      assert.ok(session.parts().some((part) => part.id === "inv"));
      const seen = new Set();
      for (let i = 0; i < 60 && seen.size < 2; i += 1) {
        session.advance(25);
        const led = session.part("led1");
        if (led) seen.add(led.state);
      }
      assert.deepEqual([...seen].sort(), ["off", "on"], "the inverter follows the pin");
      assert.equal(factoryCalls.length, 1, "one guest per part instance");
    } finally {
      session.close();
    }
  });

  const RV32_DIAGRAM = JSON.stringify({
    version: 1,
    parts: [
      { id: "rv", type: "board-litz-rv32", attrs: {} },
      { id: "r1", type: "litz-resistor", attrs: { value: "220" } },
      { id: "led1", type: "litz-led", attrs: { color: "green" } },
    ],
    connections: [
      ["rv:GPIO8", "r1:1", "green", []],
      ["r1:2", "led1:A", "green", []],
      ["led1:C", "rv:GND.1", "black", []],
    ],
  });

  test("a host chip boots, prints and drives a board pin", async () => {
    const session = litz.createSession({
      diagram: RV32_DIAGRAM,
      app: new Uint8Array([0xe9, 0, 0, 0]),
      boards: {
        "litz-rv32.wasm": (host) => {
          let cycles = 0;
          let high = false;
          let pending = [];
          return {
            info() {
              return {
                name: "js-test-chip",
                description: "a chip the JS host runs",
                cpuHz: 16_000_000,
                flash: { size: 4 * 1024 * 1024, bootloader: 0, partitionTable: 0, app: 0 },
              };
            },
            loadImage() {},
            boot() {
              host.console("litz: host chip\n");
              return 0;
            },
            setInputs() {},
            run(budget) {
              cycles += budget;
              high = !high;
              pending.push({ pin: 8, level: high ? "high" : "low", enabled: true });
              return { cycles: budget, stop: "budget" };
            },
            takeOutputs() {
              const outputs = pending;
              pending = [];
              return outputs;
            },
            writeConsole() {},
          };
        },
      },
    });
    try {
      assert.equal(session.chip().name, "js-test-chip");
      const seen = new Set();
      let serial = "";
      for (let i = 0; i < 20 && seen.size < 2; i += 1) {
        session.run(100_000);
        serial += session.serial.take();
        seen.add(session.part("led1")?.state);
      }
      assert.match(serial, /litz: host chip/);
      assert.deepEqual([...seen].sort(), ["off", "on"], "the board sees both levels");
    } finally {
      session.close();
    }
  });

  test("a host chip that faults stops the run", async () => {
    const session = litz.createSession({
      diagram: RV32_DIAGRAM,
      app: new Uint8Array([0xe9, 0, 0, 0]),
      boards: {
        "litz-rv32.wasm": (host) => ({
          info() {
            return {
              name: "faulty",
              cpuHz: 1_000_000,
              flash: { size: 1024, bootloader: 0, partitionTable: 0, app: 0 },
            };
          },
          loadImage() {},
          boot() {
            host.fault("the guest gave up");
            return 0;
          },
          setInputs() {},
          run() {
            return { cycles: 0, stop: "trap" };
          },
          takeOutputs() {
            return [];
          },
          writeConsole() {},
        }),
      },
    });
    try {
      const stopped = session.run(10_000);
      assert.equal(typeof stopped, "number");
      assert.equal(session.stop, "trap");
      assert.match(session.fault ?? "", /the guest gave up/);
    } finally {
      session.close();
    }
  });

  test("a real transpiled component drives the circuit", async () => {
    const core = new WebAssembly.Module(
      await readFile(path.join(path.dirname(fileURLToPath(import.meta.url)), "fixtures/74hc595/74hc595.core.wasm")),
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
    const session = litz.createSession({
      diagram,
      app: await fixture("examples/blinky/firmware/blinky.bin"),
      parts: {
        "74hc595.wasm": () => partFromJco(instantiate74hc595(() => core, partImports())),
      },
    });
    try {
      let lit = false;
      for (let i = 0; i < 60 && !lit; i += 1) {
        session.advance(25);
        lit = session.part("led1")?.state === "on";
      }
      assert.ok(lit, "the shift register drives Q0 and lights the LED");
      assert.deepEqual(session.conflicts(), []);
    } finally {
      session.close();
    }
  });

  test("a missing host runtime is reported with the module name", async () => {
    assert.throws(
      () =>
        litz.createSession({
          diagram: DIAGRAM,
          app: new Uint8Array([1, 2, 3, 4]),
          manifests: [INVERTER],
        }),
      (error) =>
        error instanceof LitzError && /test-inverter\.wasm/.test(error.message),
    );
  });
});

describe("litz:mcu@0.2.0 host calls", () => {
  const V2_DIAGRAM = JSON.stringify({
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

  // The fixture's register machine: print "hi", then wifi-scan, an I2C read
  // of the BMP280, ble-scan, an analog read, a UART write, join "lab", and a
  // TCP echo on port 7, then halt.
  const PROGRAM = new Uint8Array([
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

  const fixtureDir = path.join(
    path.dirname(fileURLToPath(import.meta.url)),
    "fixtures/board-v2",
  );
  const cores = new Map();
  const getCoreModule = (name) => {
    if (!cores.has(name)) {
      cores.set(name, new WebAssembly.Module(readFileSync(path.join(fixtureDir, name))));
    }
    return cores.get(name);
  };

  function createV2(program, diagram = V2_DIAGRAM) {
    return litz.createSession({
      diagram,
      app: program,
      manifests: [
        {
          toml: readFileSync(
            path.join(repoRoot, "crates/litz-mcu-wasm/tests/fixtures/board-v2/board.toml"),
            "utf8",
          ),
          schematic: readFileSync(
            path.join(repoRoot, "crates/litz-mcu-wasm/tests/fixtures/board-v2/board-v2.onboard.json"),
            "utf8",
          ),
        },
      ],
      boards: {
        "board-v2.wasm": (host) =>
          chipFromJco(instantiateBoardV2(getCoreModule, chipImports(host))),
      },
    });
  }

  function runToHalt(session) {
    let serial = "";
    for (let i = 0; i < 64 && session.stop !== "halted"; i += 1) {
      session.run(100_000);
      serial += session.serial.take();
    }
    return serial;
  }

  test("a transpiled chip reaches the buses, Wi-Fi and BLE through the bridge", () => {
    const session = createV2(PROGRAM);
    try {
      const serial = runToHalt(session);
      assert.match(serial, /^hi\n/);
      assert.match(serial, /wifi lab ch6 -40dBm/);
      assert.match(serial, /i2c 76 58/);
      assert.match(serial, /ble .*litz/);
      assert.match(serial, /adc 0 none/);
      assert.match(serial, /uart 0 ok/);
      assert.match(serial, /joined lab at 10\.0\.2\.15/);
      assert.match(serial, /tcp 7 pin/);
      assert.equal(session.fault, null);
    } finally {
      session.close();
    }
  });

  test("a guest serves a GATT table and advertises it", () => {
    // One 0x180f service with a readable 0x2a19 and a writable 0x2a1a, then
    // advertise the complete name "node".
    const program = new Uint8Array([
      4, 9, 1, 0x18, 0x0f, 2,
      0x2a, 0x19, 0x05, 1, 0x64,
      0x2a, 0x1a, 0x02, 0,
      4, 7, 9, 2, 1, 6, 5, 9, 0x6e, 0x6f, 0x64, 0x65, 0,
      0,
    ]);
    const session = createV2(program);
    try {
      const serial = runToHalt(session);
      assert.match(serial, /gatt 1/);
      assert.match(serial, /ble adv /);
      assert.equal(session.fault, null);
    } finally {
      session.close();
    }
  });

  test("a guest puts a raw frame on the shared radio medium", () => {
    // Network "lab", channel 15, address 01; broadcast "radiogram" with
    // 1000 ns of airtime.
    const program = new Uint8Array([
      4, 19, 3, 0x6c, 0x61, 0x62, 0, 15, 1, 0x01,
      4, 20, 9, ...new TextEncoder().encode("radiogram"),
      0, 0, 0, 0x03, 0xe8,
      0,
    ]);
    const session = createV2(program);
    try {
      const serial = runToHalt(session);
      assert.match(serial, /radio lab ch15/);
      assert.match(serial, /radio tx 9/);
      assert.equal(session.fault, null);
    } finally {
      session.close();
    }
  });
});

describe("storage volumes and board power", () => {
  const diagram = JSON.stringify({
    version: 1,
    parts: [
      { id: "esp", type: "board-esp32-c3-devkitm-1", left: 0, top: 0, attrs: {} },
      { id: "sd", type: "litz-microsd-card", left: 400, top: 0, attrs: { size: "1MB" } },
    ],
    connections: [
      ["sd:CS", "esp:GPIO4", "", []],
      ["sd:SCK", "esp:GPIO5", "", []],
      ["sd:DI", "esp:GPIO6", "", []],
      ["sd:DO", "esp:GPIO7", "", []],
      ["sd:VCC", "esp:3V3", "", []],
      ["sd:GND", "esp:GND.1", "", []],
    ],
  });

  async function withCard(files) {
    return litz.createSession({
      diagram,
      app: await fixture("examples/blinky/firmware/blinky.bin"),
      storage: { sd: files },
    });
  }

  test("formats host files onto the card and reads them back", async () => {
    const session = await withCard([
      { path: "boot.py", data: new TextEncoder().encode('print("hi")\n') },
      { path: "logs/run.txt", data: new TextEncoder().encode("line\n") },
    ]);
    try {
      const files = session.storageFiles("sd");
      assert.deepEqual(
        files.map((file) => file.path),
        ["boot.py", "logs/run.txt"],
      );
      assert.equal(new TextDecoder().decode(files[0].data), 'print("hi")\n');
    } finally {
      session.close();
    }
  });

  test("a part that is not storage is refused", async () => {
    const session = await withCard([]);
    try {
      assert.throws(() => session.storageFiles("esp"), LitzError);
    } finally {
      session.close();
    }
  });

  test("returns the flash the run left behind", async () => {
    const session = await withCard([]);
    try {
      const image = session.flashImage();
      assert.ok(image instanceof Uint8Array);
      assert.equal(image.length, 4 * 1024 * 1024);
    } finally {
      session.close();
    }
  });

  test("the board power switch stops and restarts the core", async () => {
    const session = await withCard([]);
    try {
      session.advance(200);
      assert.match(session.serial.takeBoard("esp"), /litz: blinky/);
      session.setNamedControl("esp", "power", "off");
      session.advance(200);
      assert.equal(session.serial.takeBoard("esp"), "");
      session.setNamedControl("esp", "power", "on");
      session.advance(200);
      assert.match(session.serial.takeBoard("esp"), /litz: blinky/);
    } finally {
      session.close();
    }
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

  test("takes flash size, MAC and analog settings", async () => {
    const session = litz.createSession({
      diagram: new TextDecoder().decode(await fixture("examples/blinky/diagram.json")),
      app: await fixture("examples/blinky/firmware/blinky.bin"),
      flashSize: 2 * 1024 * 1024,
      mac: "02:00:00:00:00:01",
      analog: true,
    });
    try {
      assert.equal(session.chip().flash.size, 2 * 1024 * 1024);
    } finally {
      session.close();
    }
  });

  test("rejects a flash size that is not a positive whole number", async () => {
    const app = await fixture("examples/blinky/firmware/blinky.bin");
    const diagram = new TextDecoder().decode(await fixture("examples/blinky/diagram.json"));
    assert.throws(() => litz.createSession({ diagram, app, flashSize: 0 }), LitzError);
    assert.throws(() => litz.createSession({ diagram, app, flashSize: 1.5 }), LitzError);
  });

  test("says why an Xtensa board cannot boot in this module", async () => {
    const app = await fixture("examples/blinky/firmware/blinky.bin");
    const diagram = JSON.stringify({
      version: 1,
      parts: [{ id: "esp", type: "board-esp32-s3-devkitc-1", attrs: {} }],
      connections: [],
    });
    assert.throws(
      () => litz.createSession({ diagram, app }),
      (error) => error instanceof LitzError && /Xtensa/.test(error.message),
    );
  });
});

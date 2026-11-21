// Litz browser front end.
//
// Plain ES module, no dependencies. It loads the C-ABI WebAssembly build,
// drives it from requestAnimationFrame, draws diagram.json as SVG, and polls
// the wasm for serial bytes, conflicts and part state.

const WASM_URL = "./litz_wasm.wasm";
// Each demo is a diagram plus the firmware generated for it by `web/build.sh`.
// They exist so the page has something running before the visitor loads
// anything of their own.
const DEMOS = {
  blinky: { diagram: "./demo/diagram.json", app: "./demo/blinky.bin" },
  button: { diagram: "./demo/button/diagram.json", app: "./demo/button/button.bin" },
  switch: { diagram: "./demo/switch/diagram.json", app: "./demo/switch/hello.bin" },
  pot: { diagram: "./demo/pot/diagram.json", app: "./demo/pot/pot.bin" },
};
const SCRATCH_BYTES = 64 * 1024;
const MAX_SERIAL_CHARS = 200_000;
const SVG_NS = "http://www.w3.org/2000/svg";

const REQUIRED_EXPORTS = (
  "litz_alloc litz_free litz_new litz_drop litz_last_error litz_run " +
  "litz_conflict_count litz_conflict litz_serial_take litz_part_state " +
  "litz_press litz_part_count litz_part_id litz_set_control litz_serial_write"
).split(" ");

const ENCODER = new TextEncoder();
const DECODER = new TextDecoder();

// Sizes are SVG units. `pins` is "right" for a board, a table of normalized
// positions for a known part, or absent for the generic 60x40 unknown part.
const GEOMETRY = {
  "board-esp32-c3-devkitm-1": { width: 250, height: 200, pins: "right" },
  // The browser builds one chip today; the other boards are drawn so a
  // diagram written for them still opens, and the session refuses politely.
  "board-esp32-devkit-c-v4": { width: 250, height: 220, pins: "right" },
  "board-esp32-s3-devkitc-1": { width: 250, height: 220, pins: "right" },
  "board-esp32-c6-devkitc-1": { width: 250, height: 220, pins: "right" },
  "board-esp32-p4-function-ev": { width: 250, height: 220, pins: "right" },
  "board-esp32-s31-coreboard": { width: 250, height: 220, pins: "right" },
  "litz-led": { width: 96, height: 52, pins: {
    A: { side: "left", at: 0.35 }, C: { side: "right", at: 0.65 } } },
  "litz-resistor": { width: 108, height: 38, pins: {
    1: { side: "left", at: 0.5 }, 2: { side: "right", at: 0.5 } } },
  "litz-pushbutton": { width: 88, height: 70, pins: {
    "1.l": { side: "left", at: 0.3 }, "2.l": { side: "left", at: 0.7 },
    "1.r": { side: "right", at: 0.3 }, "2.r": { side: "right", at: 0.7 } } },
  "litz-slide-switch": { width: 84, height: 44, pins: {
    1: { side: "left", at: 0.3 }, 3: { side: "left", at: 0.7 }, 2: { side: "right", at: 0.5 } } },
  "litz-7segment": { width: 84, height: 124, pins: {
    a: { side: "left", at: 0.12 }, b: { side: "left", at: 0.24 },
    c: { side: "left", at: 0.36 }, d: { side: "left", at: 0.48 },
    e: { side: "left", at: 0.6 }, f: { side: "left", at: 0.72 },
    g: { side: "left", at: 0.84 }, dp: { side: "left", at: 0.94 },
    com: { side: "right", at: 0.5 } } },
  "litz-buzzer": { width: 74, height: 74, pins: {
    1: { side: "left", at: 0.35 }, 2: { side: "left", at: 0.7 } } },
  "litz-potentiometer": { width: 76, height: 96, pins: {
    1: { side: "left", at: 0.25 }, 2: { side: "right", at: 0.5 },
    3: { side: "left", at: 0.75 } } },
  "litz-servo": { width: 96, height: 74, pins: {
    GND: { side: "left", at: 0.3 }, "V+": { side: "left", at: 0.55 },
    PWM: { side: "left", at: 0.8 } } },
};
const GENERIC_GEOMETRY = { width: 60, height: 40 };
const GENERIC_BOARD_GEOMETRY = { width: 60, height: 40, pins: "right" };

const LED_COLORS = { red: "#ff5a5a", green: "#48e07a", blue: "#5aa8ff",
  yellow: "#ffd84a", white: "#f5f7fa", orange: "#ff9b45" };
const WIRE_COLORS = { green: "#3fb950", red: "#f85149", black: "#6b7280",
  blue: "#58a6ff", yellow: "#d29922", white: "#c9d1d9", orange: "#db6d28",
  purple: "#bc8cff", gray: "#8b949e", grey: "#8b949e", brown: "#a0522d" };

const $ = (id) => document.getElementById(id);
const ui = {
  run: $("run"), reset: $("reset"), waveform: $("waveform"), speed: $("speed"), demo: $("demo"),
  simTime: $("sim-time"), stepsRate: $("steps-rate"),
  conflicts: $("conflicts"), conflictText: $("conflict-text"),
  notice: $("notice"), noticeTitle: $("notice-title"), noticeBody: $("notice-body"),
  schematic: $("schematic"), schematicNote: $("schematic-note"),
  serial: $("serial"), serialClear: $("serial-clear"),
  serialForm: $("serial-form"), serialInput: $("serial-input"),
  loader: $("loader"), loaderNote: $("loader-note"),
  loaderStatus: $("loader-status"), start: $("start"),
  fDiagram: $("f-diagram"), fApp: $("f-app"), fRom: $("f-rom"),
  fBoot: $("f-boot"), fPtable: $("f-ptable"),
};

const sim = {
  wasm: null, scratch: 0, handle: 0, diagram: null, inputs: null,
  running: false, crashed: false, held: null, stepsPerFrame: 400000,
  simMs: 0, rate: 0, accSteps: 0, rateAt: 0,
  conflicts: 0, firstConflict: "", serial: "", decoder: null,
  partIds: [], nodes: new Map(), // nodes maps a part id to its SVG group
};

// Wasm memory helpers -------------------------------------------------------

// Rust can grow linear memory on any allocation. Growing detaches the old
// ArrayBuffer, so a cached Uint8Array would read stale memory or throw. Every
// access therefore takes a fresh view from memory.buffer.
const view = (ptr, len) => new Uint8Array(sim.wasm.memory.buffer, ptr, len);
const readBytes = (ptr, len) => view(ptr, len).slice();

function alloc(bytes) {
  if (!bytes || bytes.length === 0) return { ptr: 0, len: 0 };
  const ptr = sim.wasm.litz_alloc(bytes.length);
  if (!ptr) throw new Error(`litz_alloc(${bytes.length}) failed`);
  view(ptr, bytes.length).set(bytes);
  return { ptr, len: bytes.length };
}

const free = (block) => { if (block && block.ptr) sim.wasm.litz_free(block.ptr, block.len); };

function lastErrorText() {
  const n = Number(sim.wasm.litz_last_error(sim.scratch, SCRATCH_BYTES));
  return n ? DECODER.decode(readBytes(sim.scratch, Math.min(n, SCRATCH_BYTES))).trim() : "";
}

// Session lifecycle ---------------------------------------------------------

async function initWasm() {
  let response;
  try {
    response = await fetch(WASM_URL, { cache: "no-store" });
  } catch (err) {
    throw new Error(`Could not fetch ${WASM_URL}. Serve this directory over HTTP ` +
      `(for example: python3 -m http.server) instead of opening index.html from disk.`);
  }
  if (!response.ok) {
    throw new Error(`${WASM_URL} was not found (HTTP ${response.status}).\n` +
      `Build it with web/build.sh, or run:\n` +
      `  cargo build -p litz-wasm --target wasm32-unknown-unknown --release\n` +
      `  cp target/wasm32-unknown-unknown/release/litz_wasm.wasm web/`);
  }

  let instance;
  try {
    ({ instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {}));
  } catch (err) {
    throw new Error(`Could not instantiate ${WASM_URL}: ${errorText(err)}`);
  }

  const exports = instance.exports;
  const missing = REQUIRED_EXPORTS.filter((name) => typeof exports[name] !== "function");
  if (missing.length) throw new Error(`Wasm module is missing: ${missing.join(", ")}`);
  if (!exports.memory) throw new Error("Wasm module does not export memory");

  sim.wasm = exports;
  sim.scratch = exports.litz_alloc(SCRATCH_BYTES);
  if (!sim.scratch) throw new Error("litz_alloc failed while reserving a scratch buffer");
}

function startSession(inputs) {
  if (!sim.wasm) { showNotice("Simulator unavailable", "The wasm module is not loaded."); return; }

  let diagram;
  try {
    diagram = JSON.parse(inputs.diagramText);
  } catch (err) {
    showNotice("diagram.json is not valid JSON", errorText(err));
    return;
  }
  diagram.parts = Array.isArray(diagram.parts) ? diagram.parts : [];
  diagram.connections = Array.isArray(diagram.connections) ? diagram.connections : [];
  sim.diagram = diagram;
  sim.nodes = renderSchematic(diagram);
  ui.schematicNote.textContent =
    `${diagram.parts.length} parts, ${diagram.connections.length} connections`;

  if (sim.handle) {
    try { sim.wasm.litz_drop(sim.handle); } catch (err) { /* the old session is gone */ }
    sim.handle = 0;
  }
  Object.assign(sim, {
    running: false, crashed: false, held: null, simMs: 0, rate: 0,
    accSteps: 0, rateAt: performance.now(),
    conflicts: 0, firstConflict: "", serial: "", partIds: [],
  });
  sim.decoder = new TextDecoder();
  ui.serial.textContent = "";
  updateStatus();

  const blocks = [];
  let handle = 0;
  try {
    for (const bytes of [ENCODER.encode(inputs.diagramText),
      inputs.rom, inputs.boot, inputs.ptable, inputs.app]) {
      blocks.push(alloc(bytes));
    }
    const [d, r, b, p, a] = blocks;
    handle = sim.wasm.litz_new(
      d.ptr, d.len, r.ptr, r.len, b.ptr, b.len, p.ptr, p.len, a.ptr, a.len);
  } catch (err) {
    showNotice("Could not start the simulator", errorText(err));
    updateControls();
    return;
  } finally {
    // Construction takes (ptr, len); the caller owns and frees its buffers.
    blocks.forEach(free);
  }

  if (!handle) {
    showNotice("Simulator refused to start",
      lastErrorText() || "litz_new returned 0 and litz_last_error produced no text.");
    updateControls();
    return;
  }

  sim.handle = handle;
  // Recording is opt-in and starts with the session, so time 0 of the VCD is
  // the board as the chip first sees it.
  if (sim.wasm.litz_trace_start) sim.wasm.litz_trace_start(handle);
  sim.inputs = inputs;
  sim.partIds = collectPartIds();
  sim.partBytes = new Map(sim.partIds.map((entry) => [entry.id, entry]));
  sim.running = true;
  clearNotice();
  ui.loader.hidden = true;
  updateControls();
}

function resetSession() {
  if (!sim.inputs || !sim.wasm) return;
  releaseHeld();
  startSession(sim.inputs);
}

function toggleRun() {
  if (!sim.handle) return;
  sim.running = !sim.running;
  sim.accSteps = 0;
  sim.rateAt = performance.now();
  if (!sim.running) sim.rate = 0;
  updateControls();
  updateStatus();
}

function collectPartIds() {
  const entries = [];
  const seen = new Set();
  const add = (id) => {
    const bytes = id ? ENCODER.encode(id) : null;
    if (!bytes || bytes.length > SCRATCH_BYTES || seen.has(id)) return;
    seen.add(id);
    entries.push({ id, bytes });
  };
  for (const part of sim.diagram.parts) {
    if (part && typeof part.id === "string") add(part.id);
  }
  const count = Number(sim.wasm.litz_part_count(sim.handle));
  for (let i = 0; i < count; i++) {
    const n = Number(sim.wasm.litz_part_id(sim.handle, i, sim.scratch, SCRATCH_BYTES));
    if (n > 0 && n <= SCRATCH_BYTES) add(DECODER.decode(readBytes(sim.scratch, n)));
  }
  return entries;
}

// Frame loop ----------------------------------------------------------------

function tick(now) {
  if (sim.handle && sim.running && !sim.crashed) {
    sim.simMs = Number(sim.wasm.litz_run(sim.handle, sim.stepsPerFrame));
    sim.accSteps += sim.stepsPerFrame;
  }

  // The ABI has no change notification, so state is sampled every frame; it is
  // one integer per part. Serial and conflict text are fetched only on change.
  if (sim.handle && !sim.crashed) {
    pumpSerial();
    pollPartStates();
    pollConflicts();
  }

  // Steps per wall-clock second, including time the browser spends drawing and
  // idle between frames, not just time spent inside litz_run.
  if (now - sim.rateAt >= 500) {
    const elapsed = Math.max(1, now - sim.rateAt);
    sim.rate = sim.running ? (sim.accSteps * 1000) / elapsed : 0;
    sim.accSteps = 0;
    sim.rateAt = now;
  }
  updateStatus();
}

function frame(now) {
  try {
    tick(now);
  } catch (err) {
    if (!sim.crashed) {
      sim.crashed = true;
      sim.running = false;
      updateControls();
      updateStatus();
      showNotice("Simulation stopped", errorText(err));
    }
  }
  requestAnimationFrame(frame);
}

function pumpSerial() {
  let got = false;
  for (let i = 0; i < 16; i++) {
    const n = Number(sim.wasm.litz_serial_take(sim.handle, sim.scratch, SCRATCH_BYTES));
    if (!n) break;
    const take = Math.min(n, SCRATCH_BYTES);
    sim.serial += sim.decoder.decode(view(sim.scratch, take), { stream: true });
    got = true;
    if (take < SCRATCH_BYTES) break;
  }
  if (!got) return;
  // Bound the scrollback so a chatty firmware cannot exhaust the tab.
  if (sim.serial.length > MAX_SERIAL_CHARS) sim.serial = sim.serial.slice(-MAX_SERIAL_CHARS);
  ui.serial.textContent = sim.serial;
  ui.serial.scrollTop = ui.serial.scrollHeight;
}

function pollPartStates() {
  const states = new Map();
  for (const entry of sim.partIds) {
    if (!entry.bytes.length) continue;
    view(sim.scratch, entry.bytes.length).set(entry.bytes);
    states.set(entry.id, sim.wasm.litz_part_state(sim.handle, sim.scratch, entry.bytes.length));
  }
  const bytesById = sim.partBytes;
  for (const [id, g] of sim.nodes) {
    const state = states.get(id);
    g.classList.toggle("led-on", state === 2);
    g.classList.toggle("button-down", state === 4);
    g.classList.toggle("switch-on", state === 6);
    g.classList.toggle("buzzing", state === 8);
    if (g.__knob) {
      g.__knob.setAttribute("x", state === 6 ? 38 : 18);
    }
    if ((state === 9 || state === 10 || state === 11) && sim.wasm.litz_part_value) {
      const entry = bytesById.get(id);
      if (entry) {
        view(sim.scratch, entry.bytes.length).set(entry.bytes);
        const value = Number(sim.wasm.litz_part_value(sim.handle, sim.scratch, entry.bytes.length));
        if (state === 10 && g.__segments) {
          const mask = value | 0;
          g.__segments.forEach((el, bit) => el.classList.toggle("seg-on", (mask >> bit) & 1));
        } else if (state === 11 && g.__needle) {
          g.__needle.setAttribute("transform",
            `rotate(${potAngle(value)} ${g.__potCx} ${g.__potCy})`);
        } else if (state === 9 && g.__horn) {
          // 0° points left, 180° right, which is how a hobby servo sweeps.
          g.__horn.setAttribute("transform",
            `translate(${g.__cx ?? 0} ${g.__cy ?? 0}) rotate(${180 - value})`);
        }
      }
    }
  }
}

function pollConflicts() {
  const count = Number(sim.wasm.litz_conflict_count(sim.handle));
  if (count !== sim.conflicts) { sim.conflicts = count; sim.firstConflict = ""; }
  if (count > 0 && !sim.firstConflict) {
    const n = Number(sim.wasm.litz_conflict(sim.handle, 0, sim.scratch, SCRATCH_BYTES));
    const text = n > 0 ? DECODER.decode(readBytes(sim.scratch, Math.min(n, SCRATCH_BYTES))) : "";
    sim.firstConflict = text.replace(/\s+/g, " ").trim() || "electrical conflict";
  }
}

function updateStatus() {
  ui.simTime.textContent = formatMs(sim.simMs);
  ui.stepsRate.textContent = formatRate(sim.running ? sim.rate : 0);
  ui.conflicts.textContent = String(sim.conflicts);
  ui.conflicts.classList.toggle("bad", sim.conflicts > 0);
  const text = sim.conflicts > 0 ? sim.firstConflict || "electrical conflict" : "";
  ui.conflictText.textContent = text;
  ui.conflictText.title = text;
}

function updateControls() {
  ui.run.disabled = !sim.handle || sim.crashed;
  ui.reset.disabled = !sim.inputs || !sim.wasm;
  ui.run.textContent = sim.running ? "Pause" : "Run";
  ui.waveform.disabled = !sim.handle || !sim.wasm.litz_trace_vcd;
}

// Waveform ----------------------------------------------------------------

// The board records every net transition from session start; this hands the
// VCD to the browser, where PulseView and GTKWave can open it. A canvas view
// is a later layer over the same data.
function saveVcd() {
  if (!sim.handle || !sim.wasm.litz_trace_vcd) return;
  const len = Number(sim.wasm.litz_trace_len(sim.handle));
  const cap = Math.max(4096, len + 4096);
  const ptr = sim.wasm.litz_alloc(cap);
  if (!ptr) return;
  try {
    const written = Number(sim.wasm.litz_trace_vcd(sim.handle, ptr, cap));
    const blob = new Blob([readBytes(ptr, written)], { type: "text/vcd" });
    const link = document.createElement("a");
    link.href = URL.createObjectURL(blob);
    link.download = "litz.vcd";
    link.click();
    URL.revokeObjectURL(link.href);
  } finally {
    sim.wasm.litz_free(ptr, cap);
  }
}

// Schematic ----------------------------------------------------------------

function parseEndpoint(ref) {
  if (typeof ref !== "string" || !ref) return null;
  if (ref.startsWith("$") && !ref.includes(":")) return { part: ref, pin: ref };
  const colon = ref.indexOf(":");
  return colon > 0 ? { part: ref.slice(0, colon), pin: ref.slice(colon + 1) } : null;
}

function geometryFor(type) {
  if (GEOMETRY[type]) return GEOMETRY[type];
  return type.startsWith("board-") ? GENERIC_BOARD_GEOMETRY : GENERIC_GEOMETRY;
}

function toCoord(value) {
  if (value === null || value === undefined || value === "") return NaN;
  const n = Number(value);
  return Number.isFinite(n) ? n : NaN;
}

function makeNode(part, usedPins) {
  const type = typeof part.type === "string" ? part.type : "";
  const geo = geometryFor(type);
  const names = [...usedPins].filter(Boolean).sort();
  const node = {
    id: String(part.id), type,
    attrs: part.attrs && typeof part.attrs === "object" ? part.attrs : {},
    x: toCoord(part.left), y: toCoord(part.top),
    rotate: Number.isFinite(Number(part.rotate)) ? Number(part.rotate) : 0,
    w: geo.width, h: geo.height, pins: [], virtual: false,
  };

  if (geo.pins === "right") {
    // Board headers sit on the right edge; grow the box for long headers.
    node.h = Math.max(geo.height, names.length * 22 + 40);
    names.forEach((name, i) =>
      node.pins.push({ name, side: "right", at: (i + 1) / (names.length + 1) }));
  } else if (geo.pins) {
    const extra = [];
    for (const name of names) {
      const def = geo.pins[name];
      if (def) node.pins.push({ name, side: def.side, at: def.at });
      else extra.push(name); // in the diagram but not in our geometry table
    }
    extra.forEach((name, i) =>
      node.pins.push({ name, side: "right", at: (i + 1) / (extra.length + 1) }));
  } else {
    // Unknown 60x40 part: one marker per distinct endpoint, alternating sides.
    const perSide = Math.ceil(names.length / 2);
    names.forEach((name, i) => node.pins.push({ name,
      side: i % 2 === 0 ? "left" : "right",
      at: perSide === 1 ? 0.5 : (Math.floor(i / 2) + 1) / (perSide + 1) }));
  }
  positionPins(node);
  return node;
}

function makeVirtualNode(id, usedPins) {
  const names = [...usedPins].filter(Boolean).sort();
  const node = { id, type: "$external", attrs: {}, x: NaN, y: NaN, rotate: 0,
    w: 118, h: Math.max(34, names.length * 22 + 14), pins: [], virtual: true,
    pseudo: id.startsWith("$") };
  names.forEach((name, i) =>
    node.pins.push({ name, side: "left", at: (i + 1) / (names.length + 1) }));
  positionPins(node);
  return node;
}

function positionPins(node) {
  for (const pin of node.pins) {
    if (pin.side === "top") { pin.lx = node.w * pin.at; pin.ly = 0; }
    else if (pin.side === "bottom") { pin.lx = node.w * pin.at; pin.ly = node.h; }
    else if (pin.side === "right") { pin.lx = node.w; pin.ly = node.h * pin.at; }
    else { pin.lx = 0; pin.ly = node.h * pin.at; }
  }
}

function layoutNodes(nodes) {
  const real = nodes.filter((n) => !n.virtual);
  const virtual = nodes.filter((n) => n.virtual);
  const positioned = (n) => Number.isFinite(n.x) && Number.isFinite(n.y);

  // Auto layout: boards on the left, everything else in a column to the right.
  const autoBoards = real.filter((n) => !positioned(n) && n.type.startsWith("board-"));
  const autoOthers = real.filter((n) => !positioned(n) && !n.type.startsWith("board-"));
  let y = 40;
  for (const n of autoBoards) { n.x = 40; n.y = y; y += n.h + 70; }

  let right = 0;
  for (const n of real) if (positioned(n)) right = Math.max(right, n.x + n.w);
  for (const n of autoBoards) right = Math.max(right, n.x + n.w);
  const columnX = right > 0 ? right + 140 : 40;
  y = 40;
  for (const n of autoOthers) { n.x = columnX; n.y = y; y += n.h + 55; }

  // Rails and $host endpoints are not parts, but drawing them on the far right
  // keeps both ends of every wire visible.
  let maxRight = 0;
  for (const n of real) maxRight = Math.max(maxRight, n.x + n.w);
  let virtualY = 40;
  for (const n of virtual) { n.x = maxRight + 130; n.y = virtualY; virtualY += n.h + 40; }
}

function svgEl(name, attrs, text) {
  const el = document.createElementNS(SVG_NS, name);
  if (attrs) {
    for (const [key, value] of Object.entries(attrs)) {
      if (value !== undefined && value !== null) el.setAttribute(key, value);
    }
  }
  if (text !== undefined) el.textContent = text;
  return el;
}

function wireColor(value) {
  if (typeof value !== "string" || !value) return "#5b6b7f";
  const key = value.toLowerCase();
  if (WIRE_COLORS[key]) return WIRE_COLORS[key];
  return /^#[0-9a-f]{3,8}$/.test(key) ? key : "#5b6b7f";
}

function subtitle(node) {
  if (node.virtual) return node.pseudo ? "external" : "not placed";
  if (node.type === "litz-resistor" && node.attrs.value !== undefined) {
    return `R ${node.attrs.value}`;
  }
  if (node.type === "litz-led" && node.attrs.color !== undefined) {
    return `LED ${node.attrs.color}`;
  }
  return node.type.replace(/^litz-/, "").replace(/^board-/, "");
}

function worldLocal(node, lx, ly) {
  if (!node.rotate) return { x: node.x + lx, y: node.y + ly };
  const cx = node.w / 2;
  const cy = node.h / 2;
  const rad = (node.rotate * Math.PI) / 180;
  const cos = Math.cos(rad);
  const sin = Math.sin(rad);
  const dx = lx - cx;
  const dy = ly - cy;
  return { x: node.x + cx + dx * cos - dy * sin,
    y: node.y + cy + dx * sin + dy * cos };
}

function pinWorld(node, name) {
  const pin = node.pins.find((candidate) => candidate.name === name);
  return pin ? worldLocal(node, pin.lx, pin.ly) : null;
}

function nodeBounds(node) {
  const points = [[0, 0], [node.w, 0], [0, node.h], [node.w, node.h]]
    .map(([lx, ly]) => worldLocal(node, lx, ly));
  return {
    minX: Math.min(...points.map((p) => p.x)),
    minY: Math.min(...points.map((p) => p.y)),
    maxX: Math.max(...points.map((p) => p.x)),
    maxY: Math.max(...points.map((p) => p.y)),
  };
}

function renderSchematic(diagram) {
  const container = ui.schematic;
  container.textContent = "";
  const partById = new Map();
  for (const part of diagram.parts) {
    if (part && typeof part.id === "string") partById.set(part.id, part);
  }

  const uses = new Map();
  const wires = [];
  const noteUse = (endpoint) => {
    if (!uses.has(endpoint.part)) uses.set(endpoint.part, new Set());
    if (endpoint.pin) uses.get(endpoint.part).add(endpoint.pin);
  };
  for (const raw of diagram.connections) {
    if (!Array.isArray(raw) || raw.length < 2) continue;
    const from = parseEndpoint(raw[0]);
    const to = parseEndpoint(raw[1]);
    if (!from || !to) continue;
    wires.push({ from, to, color: raw[2] });
    noteUse(from);
    noteUse(to);
  }

  const nodes = [];
  for (const part of diagram.parts) {
    if (part && typeof part.id === "string" && part.id) {
      nodes.push(makeNode(part, uses.get(part.id) || new Set()));
    }
  }
  for (const [id, pins] of uses) {
    if (!partById.has(id)) nodes.push(makeVirtualNode(id, pins));
  }
  layoutNodes(nodes);

  if (!nodes.length) {
    container.appendChild(emptyDiv("diagram.json has no parts"));
    return new Map();
  }

  const byId = new Map();
  for (const node of nodes) byId.set(node.id, node);
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  for (const node of nodes) {
    const b = nodeBounds(node);
    minX = Math.min(minX, b.minX); minY = Math.min(minY, b.minY);
    maxX = Math.max(maxX, b.maxX); maxY = Math.max(maxY, b.maxY);
  }
  const pad = 48;
  const svg = svgEl("svg", {
    viewBox: `${minX - pad} ${minY - pad} ${maxX - minX + pad * 2} ${maxY - minY + pad * 2}`,
    preserveAspectRatio: "xMidYMid meet", role: "img",
    "aria-label": "Circuit schematic",
  });

  const defs = svgEl("defs");
  const glow = svgEl("filter", { id: "led-glow", x: "-80%", y: "-80%",
    width: "260%", height: "260%" });
  glow.appendChild(svgEl("feGaussianBlur", { stdDeviation: "6" }));
  defs.appendChild(glow);
  svg.appendChild(defs);

  // Wires are painted last: a part box must not hide the middle of a
  // connection and leave it looking detached from one of its pins.
  const wireGroup = svgEl("g", { class: "wires" });
  for (const wire of wires) {
    const a = byId.get(wire.from.part);
    const b = byId.get(wire.to.part);
    if (!a || !b) continue;
    const p1 = pinWorld(a, wire.from.pin);
    const p2 = pinWorld(b, wire.to.pin);
    if (!p1 || !p2) continue;
    const mid = (p1.x + p2.x) / 2;
    const points = Math.abs(p1.x - p2.x) < 1 || Math.abs(p1.y - p2.y) < 1
      ? [p1, p2]
      : [p1, { x: mid, y: p1.y }, { x: mid, y: p2.y }, p2];
    const polyline = svgEl("polyline", {
      class: "wire", stroke: wireColor(wire.color),
      points: points.map((p) => `${p.x},${p.y}`).join(" "),
    });
    polyline.appendChild(svgEl("title", null,
      `${wire.from.part}:${wire.from.pin} to ${wire.to.part}:${wire.to.pin}`));
    wireGroup.appendChild(polyline);
  }

  const nodeMap = new Map();
  const partGroup = svgEl("g", { class: "parts" });
  const labelGroup = svgEl("g", { class: "labels" });
  for (const node of nodes) {
    const { g, labels } = drawNode(node);
    partGroup.appendChild(g);
    labelGroup.appendChild(labels);
    if (!node.virtual) nodeMap.set(node.id, g);
  }
  svg.appendChild(partGroup);
  svg.appendChild(wireGroup);
  svg.appendChild(labelGroup);
  container.appendChild(svg);
  return nodeMap;
}

/// The seven bars plus the decimal point, in mask order `a`..`g`, `dp`.
/// Returned as a list so the poll loop can light them from a bitmask without
/// walking the DOM.
function drawSegments(body, node) {
  const pad = 16, thick = 9;
  const w = node.w - pad * 2, h = node.h;
  const bar = (x, y, bw, bh) => {
    const el = svgEl("rect", { class: "seg", x, y, width: bw, height: bh, rx: 2 });
    body.appendChild(el);
    return el;
  };
  const top = 18, mid = h / 2 - thick / 2, bottom = h - 18 - thick;
  const upper = mid - top - thick, lower = bottom - mid - thick;
  const leftX = pad, rightX = pad + w - thick;
  return [
    bar(pad + thick, top, w - 2 * thick, thick),
    bar(rightX, top + thick, thick, upper),
    bar(rightX, mid + thick, thick, lower),
    bar(pad + thick, bottom, w - 2 * thick, thick),
    bar(leftX, mid + thick, thick, lower),
    bar(leftX, top + thick, thick, upper),
    bar(pad + thick, mid, w - 2 * thick, thick),
    body.appendChild(svgEl("circle", { class: "seg seg-dp", cx: rightX + 4, cy: h - 22, r: 4 })),
  ];
}

function drawNode(node) {
  const classes = ["part"];
  if (node.virtual) classes.push("virtual");
  else if (node.type.startsWith("board-")) classes.push("board");
  else if (node.type === "litz-led") classes.push("led");
  else if (node.type === "litz-pushbutton") classes.push("button");
  else if (node.type === "litz-slide-switch") classes.push("switch");
  else if (node.type === "litz-7segment") classes.push("segments");
  else if (node.type === "litz-buzzer") classes.push("buzzer");
  else if (node.type === "litz-servo") classes.push("servo");
  else if (node.type === "litz-potentiometer") classes.push("pot");

  const g = svgEl("g", {
    class: classes.join(" "),
    "data-part-id": node.id, "data-part-type": node.type,
  });
  // The body rotates with the part; the labels stay upright outside it so a
  // rotated resistor or LED remains readable.
  const body = svgEl("g", {
    transform: `translate(${node.x} ${node.y}) rotate(${node.rotate} ${node.w / 2} ${node.h / 2})`,
  });
  g.appendChild(body);
  body.appendChild(svgEl("rect", { class: "part-box", x: 0, y: 0,
    width: node.w, height: node.h, rx: 6 }));

  if (node.type === "litz-led") {
    const color = LED_COLORS[String(node.attrs.color || "red").toLowerCase()] || LED_COLORS.red;
    const cx = node.w - 20, cy = node.h / 2;
    body.appendChild(svgEl("circle", { class: "led-halo", cx, cy, r: 16,
      fill: color, filter: "url(#led-glow)" }));
    body.appendChild(svgEl("circle", { class: "led-core", cx, cy, r: 7, fill: color }));
  } else if (node.type === "litz-pushbutton") {
    body.appendChild(svgEl("rect", { class: "button-cap",
      x: node.w / 2 - 18, y: node.h / 2 - 9, width: 36, height: 18, rx: 3 }));
  } else if (node.type === "litz-slide-switch") {
    body.appendChild(svgEl("rect", { class: "switch-track",
      x: 14, y: node.h / 2 - 9, width: node.w - 28, height: 18, rx: 9 }));
    // The knob moves with the throw, so the switch reads as a switch and not
    // as a button that happens to be drawn differently.
    g.__knob = svgEl("rect", { class: "switch-knob",
      x: 18, y: node.h / 2 - 7, width: 28, height: 14, rx: 7 });
    body.appendChild(g.__knob);
  } else if (node.type === "litz-7segment") {
    g.__segments = drawSegments(body, node);
  } else if (node.type === "litz-potentiometer") {
    // A knob seen from above: the needle shows where the wiper sits, and the
    // whole body is the slider a pointer drags.
    const cx = node.w / 2, cy = node.h / 2 + 4;
    body.appendChild(svgEl("circle", { class: "pot-body", cx, cy, r: 26 }));
    g.__needle = svgEl("line", { class: "pot-needle",
      x1: cx, y1: cy, x2: cx, y2: cy - 22 });
    g.__needle.setAttribute("transform", `rotate(${potAngle(500)} ${cx} ${cy})`);
    body.appendChild(g.__needle);
    g.__potCx = cx;
    g.__potCy = cy;
  } else if (node.type === "litz-buzzer") {
    const cx = node.w / 2, cy = node.h / 2;
    body.appendChild(svgEl("circle", { class: "buzzer-body", cx, cy, r: 22 }));
    for (const r of [30, 38]) {
      body.appendChild(svgEl("path", { class: "wave",
        d: `M ${cx + r - 8} ${cy - 14} A ${r} ${r} 0 0 1 ${cx + r - 8} ${cy + 14}` }));
    }
  } else if (node.type === "litz-servo") {
    const cx = node.w / 2, cy = node.h / 2;
    body.appendChild(svgEl("rect", { class: "servo-body",
      x: 12, y: cy - 22, width: node.w - 24, height: 44, rx: 4 }));
    g.__cx = cx;
    g.__cy = cy;
    g.__horn = svgEl("g", {
      class: "servo-horn", transform: `translate(${cx} ${cy}) rotate(180)`,
    });
    g.__horn.appendChild(svgEl("rect", { x: 0, y: -4, width: 24, height: 8, rx: 3 }));
    body.appendChild(g.__horn);
  }

  for (const pin of node.pins) {
    const circle = svgEl("circle", { class: "pin", cx: pin.lx, cy: pin.ly, r: 4 });
    circle.appendChild(svgEl("title", null, pin.name));
    body.appendChild(circle);
    if (!node.virtual) {
      const left = pin.side === "left";
      body.appendChild(svgEl("text", { class: "pin-label",
        x: pin.lx + (left ? 7 : -7), y: pin.ly + 3,
        "text-anchor": left ? "start" : "end" }, pin.name));
    }
  }

  const bounds = nodeBounds(node);
  const labels = svgEl("g", { class: "part-labels" });
  if (node.rotate) {
    // Rotated bodies put their labels beside the part, unrotated.
    const x = bounds.maxX + 8;
    const cy = (bounds.minY + bounds.maxY) / 2;
    labels.appendChild(svgEl("text", { class: "part-label", x, y: cy - 2,
      "text-anchor": "start" }, node.id));
    labels.appendChild(svgEl("text", { class: "part-sub", x, y: cy + 12,
      "text-anchor": "start" }, subtitle(node)));
  } else if (node.h < 90) {
    // A small part has no room for text inside it, and the LED's glow sits
    // where the name would go, so the labels live just outside the body.
    const cx = (bounds.minX + bounds.maxX) / 2;
    labels.appendChild(svgEl("text", { class: "part-label", x: cx,
      y: bounds.minY - 6, "text-anchor": "middle" }, node.id));
    labels.appendChild(svgEl("text", { class: "part-sub", x: cx,
      y: bounds.maxY + 13, "text-anchor": "middle" }, subtitle(node)));
  } else {
    const cx = (bounds.minX + bounds.maxX) / 2;
    labels.appendChild(svgEl("text", { class: "part-label", x: cx,
      y: bounds.minY + 16, "text-anchor": "middle" }, node.id));
    labels.appendChild(svgEl("text", { class: "part-sub", x: cx,
      y: bounds.maxY - 7, "text-anchor": "middle" }, subtitle(node)));
  }

  // pointerleave does not bubble; release from the part the pointer left.
  if (node.type === "litz-pushbutton") {
    g.addEventListener("pointerleave", () => {
      if (sim.held === node.id) releaseHeld();
    });
  }
  return { g, labels };
}

// Interaction ---------------------------------------------------------------

// A knob's travel spans 270°: --135° at one end, +135° at the other, which is
// how a panel potentiometer is drawn.
const potAngle = (permille) => -135 + (270 * Math.min(1000, Math.max(0, permille))) / 1000;

function setControl(id, permille) {
  if (!sim.handle || !sim.wasm.litz_set_control) return;
  const bytes = ENCODER.encode(id);
  if (!bytes.length || bytes.length > SCRATCH_BYTES) return;
  view(sim.scratch, bytes.length).set(bytes);
  sim.wasm.litz_set_control(sim.handle, sim.scratch, bytes.length, permille | 0);
}

/// Where along the knob a pointer is, as permille. The travel is measured
/// across the body's left-to-right span, so dragging past the edge clamps.
function potFromPointer(g, event) {
  const box = g.getBoundingClientRect();
  const t = (event.clientX - box.left) / (box.width || 1);
  return Math.round(Math.min(1, Math.max(0, t)) * 1000);
}

function setPress(id, down) {
  if (!sim.handle) return;
  const bytes = ENCODER.encode(id);
  if (!bytes.length || bytes.length > SCRATCH_BYTES) return;
  view(sim.scratch, bytes.length).set(bytes);
  sim.wasm.litz_press(sim.handle, sim.scratch, bytes.length, down ? 1 : 0);
}

function releaseHeld() {
  const id = sim.held;
  sim.held = null;
  if (!id) return;
  const g = sim.nodes.get(id);
  if (g && g.getAttribute("data-part-type") === "litz-potentiometer") return;
  setPress(id, false);
}

function wireInteraction() {
  ui.schematic.addEventListener("pointerdown", (event) => {
    if (!sim.handle) return;
    const g = event.target.closest("[data-part-id]");
    const type = g && g.getAttribute("data-part-type");
    // A pushbutton is held; a slide switch latches, and its engine ignores the
    // release that follows, so both are driven the same way.
    if (type === "litz-potentiometer") {
      event.preventDefault();
      sim.held = g.getAttribute("data-part-id");
      setControl(sim.held, potFromPointer(g, event));
      return;
    }
    if (type !== "litz-pushbutton" && type !== "litz-slide-switch") return;
    event.preventDefault();
    releaseHeld();
    sim.held = g.getAttribute("data-part-id");
    setPress(sim.held, true);
  });
  // A knob follows the pointer while it is held, which is what makes it a
  // control and not a click target.
  ui.schematic.addEventListener("pointermove", (event) => {
    if (!sim.handle || !sim.held) return;
    const g = sim.nodes.get(sim.held);
    if (!g || g.getAttribute("data-part-type") !== "litz-potentiometer") return;
    setControl(sim.held, potFromPointer(g, event));
  });
  for (const name of ["pointerup", "pointercancel", "pointerleave"]) {
    ui.schematic.addEventListener(name, releaseHeld);
  }
  // A release outside the pane must not leave the button stuck down.
  window.addEventListener("pointerup", releaseHeld);
  window.addEventListener("pointercancel", releaseHeld);
}

// Loader and status ---------------------------------------------------------

const errorText = (err) => (err && err.message ? err.message : String(err));

function showNotice(title, body) {
  ui.notice.hidden = false;
  ui.noticeTitle.textContent = title;
  ui.noticeBody.textContent = body || "";
}

function clearNotice() {
  ui.notice.hidden = true;
  ui.noticeTitle.textContent = "";
  ui.noticeBody.textContent = "";
}

function emptyDiv(text) {
  const div = document.createElement("div");
  div.className = "empty";
  div.textContent = text;
  return div;
}

function showLoader(message) {
  ui.loader.hidden = false;
  ui.loaderNote.textContent = message;
  ui.schematic.textContent = "";
  ui.schematic.appendChild(emptyDiv("No circuit loaded. Use the file inputs below."));
  ui.schematicNote.textContent = "";
}

function loaderReady() {
  const ready = ui.fDiagram.files.length > 0 && ui.fApp.files.length > 0;
  ui.start.disabled = !ready;
  ui.loaderStatus.textContent = ready
    ? "Ready to start."
    : "Choose diagram.json and an application image.";
}

async function startFromFiles() {
  const diagramFile = ui.fDiagram.files[0];
  const appFile = ui.fApp.files[0];
  if (!diagramFile || !appFile) return;

  ui.start.disabled = true;
  ui.loaderStatus.textContent = "Reading files...";
  try {
    const diagramText = await diagramFile.text();
    const app = new Uint8Array(await appFile.arrayBuffer());
    const rom = ui.fRom.files[0] ? new Uint8Array(await ui.fRom.files[0].arrayBuffer()) : null;
    const boot = ui.fBoot.files[0] ? new Uint8Array(await ui.fBoot.files[0].arrayBuffer()) : null;
    const ptable = ui.fPtable.files[0]
      ? new Uint8Array(await ui.fPtable.files[0].arrayBuffer()) : null;
    startSession({ diagramText, app, rom, boot, ptable });
  } catch (err) {
    showNotice("Could not start the selected files", errorText(err));
    ui.loaderStatus.textContent = "Could not start the selected files.";
  } finally {
    loaderReady();
  }
}

async function boot() {
  try {
    await initWasm();
  } catch (err) {
    ui.schematic.textContent = "";
    ui.schematic.appendChild(emptyDiv("The simulator wasm could not be loaded."));
    showNotice("Simulator unavailable", errorText(err));
    return;
  }

  await loadDemo(ui.demo.value);
}

async function loadDemo(name) {
  const demo = DEMOS[name] || DEMOS.blinky;
  let diagramResponse, appResponse;
  try {
    [diagramResponse, appResponse] = await Promise.all([
      fetch(demo.diagram, { cache: "no-store" }),
      fetch(demo.app, { cache: "no-store" }),
    ]);
  } catch (err) {
    showLoader("The demo could not be fetched. Load a diagram and an application image below.");
    return;
  }

  if (!diagramResponse.ok || !appResponse.ok) {
    const missing = [];
    if (!diagramResponse.ok) missing.push(demo.diagram);
    if (!appResponse.ok) missing.push(demo.app);
    showLoader(`Missing ${missing.join(" and ")}. Load a diagram and firmware below.`);
    return;
  }

  try {
    const diagramText = await diagramResponse.text();
    const app = new Uint8Array(await appResponse.arrayBuffer());
    startSession({ diagramText, app, rom: null, boot: null, ptable: null });
  } catch (err) {
    showNotice("Could not load the demo", errorText(err));
  }
}

// Formatting ----------------------------------------------------------------

function formatMs(ms) {
  const value = Math.max(0, Number(ms) || 0);
  if (value < 1000) return `${Math.floor(value)} ms`;
  const seconds = value / 1000;
  if (seconds < 60) return `${seconds.toFixed(3)} s`;
  const minutes = Math.floor(seconds / 60);
  return `${minutes}m ${(seconds - minutes * 60).toFixed(2)}s`;
}

function formatRate(rate) {
  const value = Number(rate) || 0;
  if (!Number.isFinite(value) || value <= 0) return "0";
  if (value >= 1e6) return `${(value / 1e6).toFixed(2)}M`;
  if (value >= 1e3) return `${Math.round(value / 1e3)}k`;
  return `${Math.round(value)}`;
}

// Wiring --------------------------------------------------------------------

ui.run.addEventListener("click", toggleRun);
ui.reset.addEventListener("click", resetSession);
ui.waveform.addEventListener("click", saveVcd);
ui.demo.addEventListener("change", () => loadDemo(ui.demo.value));
ui.speed.addEventListener("change", () => {
  sim.stepsPerFrame = Number(ui.speed.value) || 400000;
  sim.accSteps = 0;
  sim.rateAt = performance.now();
});
ui.serialForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const text = ui.serialInput.value;
  if (!sim.handle || !sim.wasm.litz_serial_write || !text) return;
  const bytes = ENCODER.encode(`${text}\n`);
  const block = alloc(bytes);
  sim.wasm.litz_serial_write(sim.handle, block.ptr, block.len);
  free(block);
  ui.serialInput.value = "";
});
ui.serialClear.addEventListener("click", () => {
  sim.serial = "";
  ui.serial.textContent = "";
});
ui.fDiagram.addEventListener("change", loaderReady);
ui.fApp.addEventListener("change", loaderReady);
ui.start.addEventListener("click", startFromFiles);

sim.stepsPerFrame = Number(ui.speed.value) || 400000;
wireInteraction();
loaderReady();
updateControls();
updateStatus();
boot();
requestAnimationFrame(frame);
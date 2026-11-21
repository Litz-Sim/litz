// Litz workbench: a component picker, a wiring canvas, an inspector and a
// project document. Plain ES modules, no build step. The document is the only
// copy in memory; Save writes it as a .litz project, Export writes the
// diagram.json the runner reads.

import {
  defaultAttrs,
  findPin,
  groupedCatalog,
  indexCatalog,
  pinKindAccepts,
  searchCatalog,
} from "./catalog.js";
import {
  definitionOf,
  loadCustomParts,
  mergeCatalog,
  normalizeCustomParts,
  parseCustomParts,
  removeCustomPart,
  saveCustomParts,
  serializeCustomParts,
  upsertCustomPart,
} from "./custom.js";
import { polylinePath, routeWire, snap } from "./geometry.js";
import { firmwareImage } from "./hex.js";
import {
  controlAt,
  controlKind,
  controlPermille,
  nextPlacement,
  partBounds,
  partMetrics,
  partVisual,
  pinSlots,
  pinWorld,
} from "./layout.js";
import {
  connectionExists,
  createProject,
  diagramFromProject,
  formatPinRef,
  nameFromFile,
  parseDocument,
  parsePinRef,
  PROJECT_EXTENSION,
  projectFromDiagram,
  serializeProject,
  slugForType,
  uniquePartId,
  validateProject,
  withConnectionAdded,
  withConnectionRemoved,
  withPartAdded,
  withPartAttrs,
  withPartMoved,
  withPartRemoved,
  withPartRenamed,
} from "./project.js";
import { createRunController, formatMs } from "./sim.js";
import { openText, saveText, supportsFileSystemAccess } from "./storage.js";

const SVG_NS = "http://www.w3.org/2000/svg";
const $ = (id) => document.getElementById(id);

const WIRE_COLORS = {
  green: "#3fb950",
  red: "#f85149",
  black: "#6b7280",
  blue: "#58a6ff",
  yellow: "#d29922",
  orange: "#db6d28",
  purple: "#bc8cff",
  gray: "#8b949e",
  white: "#c9d1d9",
  brown: "#a0522d",
};
const WIRE_COLOR_NAMES = Object.keys(WIRE_COLORS);
const DEFAULT_WIRE_COLOR = "green";

const LED_COLORS = {
  red: "#ff5a5a",
  green: "#48e07a",
  blue: "#5aa8ff",
  yellow: "#ffd84a",
  white: "#f5f7fa",
  orange: "#ff9b45",
};

const SCOPE_COLORS = [
  "#58b3ff",
  "#4fd18b",
  "#e3b341",
  "#ff6b6b",
  "#bc8cff",
  "#ff9b45",
  "#5aa8ff",
  "#c9d1d9",
];
const SCOPE_PAD = { left: 6, right: 8, top: 6, bottom: 16 };
const SCOPE_DECODE_FRAMES = 200;

const PROJECT_TYPES = [
  { description: "Litz project", accept: { "application/json": [".litz"] } },
];
const DIAGRAM_TYPES = [
  { description: "Litz diagram", accept: { "application/json": [".json"] } },
];
const CUSTOM_TYPES = [
  { description: "Custom parts", accept: { "application/json": [".json", ".litz"] } },
];

const ui = {
  projectName: $("project-name"),
  dirtyBadge: $("dirty-badge"),
  saveState: $("save-state"),
  catalogSearch: $("catalog-search"),
  catalogList: $("catalog-list"),
  catalogCount: $("catalog-count"),
  canvas: $("canvas"),
  canvasWrap: $("canvas-wrap"),
  canvasHint: $("canvas-hint"),
  canvasEmpty: $("canvas-empty"),
  wireLayer: $("wire-layer"),
  partLayer: $("part-layer"),
  previewLayer: $("preview-layer"),
  inspectorBody: $("inspector-body"),
  projectStats: $("project-stats"),
  storageNote: $("storage-note"),
  validationSummary: $("validation-summary"),
  validationList: $("validation-list"),
  serial: $("serial"),
  diagnostics: $("diagnostics"),
  diagCount: $("diag-count"),
  serialPanel: $("serial-panel"),
  diagnosticsPanel: $("diagnostics-panel"),
  tabSerial: $("tab-serial"),
  tabDiagnostics: $("tab-diagnostics"),
  simRun: $("sim-run"),
  simPause: $("sim-pause"),
  simReset: $("sim-reset"),
  simSpeed: $("sim-speed"),
  simStatus: $("sim-status"),
  simLoad: $("sim-load"),
  firmwareKind: $("firmware-kind"),
  firmwareApp: $("firmware-app"),
  firmwareRom: $("firmware-rom"),
  firmwareBoot: $("firmware-boot"),
  firmwarePtable: $("firmware-ptable"),
  firmwareHint: $("firmware-hint"),
  serialForm: $("serial-form"),
  serialInput: $("serial-input"),
  scopeForm: $("scope-form"),
  scopePoint: $("scope-point"),
  scopeName: $("scope-name"),
  scopeChannels: $("scope-channels"),
  decodeForm: $("decode-form"),
  decodeSpec: $("decode-spec"),
  scopeDecodes: $("scope-decodes"),
  scopeReadout: $("scope-readout"),
  scopeWindow: $("scope-window"),
  scopeCanvas: $("scope-canvas"),
  displayPanel: $("display-panel"),
  displayCanvas: $("display-canvas"),
  displaySize: $("display-size"),
  customButton: $("custom-parts"),
  customDialog: $("custom-dialog"),
  customList: $("custom-list"),
  customEditor: $("custom-editor"),
  customSave: $("custom-save"),
  customImport: $("custom-import"),
  customExport: $("custom-export"),
  customStatus: $("custom-status"),
};

const state = {
  project: createProject("Untitled"),
  catalog: null,
  catalogBase: null,
  catalogError: "",
  customParts: [], // definitions as stored, not yet validated
  customEntries: [], // validated entries merged into the catalog
  selected: null, // {kind:"part", id} or {kind:"wire", index}
  pendingPin: null, // {partId, pinName}
  pointer: null,
  drag: null,
  suppressClick: false,
  projectHandle: null,
  dirty: false,
  statusText: "In memory",
  tab: "serial",
  log: [],
  validation: { issues: [], errors: [], warnings: [] },
  sim: null,
  simInitError: "",
  firmware: { app: null, rom: null, boot: null, ptable: null },
  firmwareKind: "application",
  heldControl: null,
  partNodes: new Map(),
  scopeChannels: [],
  scopeDecodes: [],
  scopeMeasureNodes: new Map(),
  scopeDoc: null,
  scopePollAt: 0,
  scopeRunning: false,
  scopeHover: null,
};

// Small DOM helpers ---------------------------------------------------------

function element(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function svgEl(name, attrs = {}, text) {
  const node = document.createElementNS(SVG_NS, name);
  for (const [key, value] of Object.entries(attrs)) {
    node.setAttribute(key, String(value));
  }
  if (text !== undefined) node.textContent = text;
  return node;
}

function truncate(text, limit) {
  const value = String(text);
  return value.length > limit ? `${value.slice(0, limit - 1)}…` : value;
}

function field(labelText, control) {
  const wrap = element("div", "field");
  wrap.append(element("span", "field-label", labelText), control);
  return wrap;
}

function readonly(text) {
  return element("div", "readonly-value", text);
}

function wireColor(name) {
  return WIRE_COLORS[String(name || "").toLowerCase()] ?? WIRE_COLORS[DEFAULT_WIRE_COLOR];
}

function findPart(id) {
  return state.project.circuit.parts.find((part) => part.id === id) ?? null;
}

function entryFor(part) {
  return part ? (state.catalog?.byId.get(part.type) ?? null) : null;
}

function endpointPosition(ref) {
  if (!ref || ref.pseudo) return null;
  const part = findPart(ref.part);
  const entry = entryFor(part);
  if (!part || !entry) return null;
  return pinWorld(part, entry, ref.name);
}

function wireIsDrawn(connection) {
  return Boolean(
    endpointPosition(parsePinRef(connection[0])) &&
      endpointPosition(parsePinRef(connection[1])),
  );
}

function toCanvasPoint(event) {
  const point = ui.canvas.createSVGPoint();
  point.x = event.clientX;
  point.y = event.clientY;
  const matrix = ui.canvas.getScreenCTM();
  if (!matrix) return { x: 0, y: 0 };
  const local = point.matrixTransform(matrix.inverse());
  return { x: local.x, y: local.y };
}

// Canvas rendering ----------------------------------------------------------

function renderCanvas() {
  ui.wireLayer.replaceChildren();
  ui.partLayer.replaceChildren();
  ui.previewLayer.replaceChildren();
  state.partNodes.clear();

  state.project.circuit.connections.forEach((connection, index) => {
    drawWire(connection, index);
  });
  for (const part of state.project.circuit.parts) {
    drawPart(part);
  }
  drawPreview();

  ui.canvasEmpty.hidden = state.project.circuit.parts.length > 0;
  renderCanvasHint();
  reflectSimState();
}

function drawWire(connection, index) {
  const from = parsePinRef(connection[0]);
  const to = parsePinRef(connection[1]);
  const a = endpointPosition(from);
  const b = endpointPosition(to);
  // A wire to a rail or host device is valid in the file but has no pin to
  // anchor on this canvas. It stays in the document and in the counts.
  if (!a || !b) return;

  const path = polylinePath(routeWire(a, b));
  const selected = state.selected?.kind === "wire" && state.selected.index === index;
  const group = svgEl("g", {
    class: `wire${selected ? " is-selected" : ""}`,
    "data-wire-index": index,
  });
  group.append(
    svgEl("path", { class: "wire-hit", d: path }),
    svgEl("path", {
      class: "wire-line",
      d: path,
      stroke: wireColor(connection[2]),
      "data-color": connection[2] || DEFAULT_WIRE_COLOR,
    }),
    svgEl("title", {}, `${formatPinRef(from)} to ${formatPinRef(to)}`),
  );
  ui.wireLayer.append(group);
}

function drawPart(part) {
  const entry = entryFor(part);
  const visual = entry ?? {
    kind: "part",
    name: part.type,
    pins: [],
    attrs: [],
  };
  const { width, height } = partMetrics(visual);
  const selected = state.selected?.kind === "part" && state.selected.id === part.id;
  const rotate = Number(part.rotate) || 0;

  const group = svgEl("g", {
    class: `part${selected ? " is-selected" : ""}${entry ? "" : " is-unknown"}`,
    "data-part-id": part.id,
    transform: `translate(${Number(part.left) || 0} ${Number(part.top) || 0}) rotate(${rotate} ${width / 2} ${height / 2})`,
  });
  group.append(
    svgEl("rect", { class: "part-body", x: 0, y: 0, width, height, rx: 7 }),
    svgEl("text", { class: "part-title", x: 10, y: 18 }, truncate(visual.name, 21)),
    svgEl("text", { class: "part-id", x: 10, y: 32 }, truncate(part.id, 21)),
  );

  drawControl(part, entry, group);

  for (const slot of pinSlots(visual)) {
    const pending =
      state.pendingPin?.partId === part.id && state.pendingPin?.pinName === slot.name;
    const pin = svgEl("g", {
      class: `pin${pending ? " is-pending" : ""}`,
      "data-pin": "1",
      "data-part-id": part.id,
      "data-pin-name": slot.name,
      "data-pin-kind": slot.kind,
    });
    pin.append(
      svgEl("circle", { class: "pin-dot", cx: slot.x, cy: slot.y, r: 4 }),
      svgEl("circle", { class: "pin-hit", cx: slot.x, cy: slot.y, r: 9 }),
      svgEl(
        "text",
        {
          class: "pin-label",
          x: slot.labelX,
          y: slot.y + 3.5,
          "text-anchor": slot.anchor,
        },
        truncate(slot.name, 16),
      ),
      svgEl("title", {}, `${slot.name} (${slot.kind})`),
    );
    group.append(pin);
  }

  state.partNodes.set(part.id, group);
  ui.partLayer.append(group);
}

/** Draw the interactive control a part carries, if the catalog says it has one. */
function drawControl(part, entry, group) {
  const control = partVisual(entry);
  if (!control.kind) return;

  if (control.kind === "led") {
    const color = LED_COLORS[String(part.attrs?.color || "red").toLowerCase()] ?? LED_COLORS.red;
    group.append(
      svgEl("circle", {
        class: "led-halo",
        cx: control.led.cx,
        cy: control.led.cy,
        r: control.led.halo,
        fill: color,
      }),
      svgEl("circle", {
        class: "led-core",
        cx: control.led.cx,
        cy: control.led.cy,
        r: control.led.r,
        fill: color,
      }),
    );
    return;
  }

  if (control.kind === "button") {
    const { x, y, width, height, rx } = control.cap;
    group.append(svgEl("rect", { class: "button-cap", x, y, width, height, rx }));
    return;
  }

  if (control.kind === "switch") {
    const { track, knob } = control;
    const handle = svgEl("rect", {
      class: "switch-knob",
      x: knob.offX,
      y: knob.y,
      width: knob.width,
      height: knob.height,
      rx: knob.rx,
    });
    group.append(
      svgEl("rect", {
        class: "switch-track",
        x: track.x,
        y: track.y,
        width: track.width,
        height: track.height,
        rx: track.rx,
      }),
      handle,
    );
    group.__switchKnob = handle;
    group.__switchOffX = knob.offX;
    group.__switchOnX = knob.onX;
    return;
  }

  // Potentiometer and rotary encoder: a knob with a needle.
  const { knob } = control;
  const needle = svgEl("line", {
    class: control.kind === "pot" ? "pot-needle" : "knob-needle",
    x1: knob.cx,
    y1: knob.cy,
    x2: knob.cx,
    y2: knob.cy + 6 - knob.r,
  });
  needle.setAttribute("transform", `rotate(${knobAngle(initialPermille(part, entry))} ${knob.cx} ${knob.cy})`);
  group.append(
    svgEl("circle", {
      class: control.kind === "pot" ? "pot-body" : "knob-body",
      cx: knob.cx,
      cy: knob.cy,
      r: knob.r,
    }),
    needle,
  );
  if (control.kind === "rotary") {
    group.append(svgEl("circle", { class: "knob-mark", cx: knob.cx, cy: knob.cy, r: 3 }));
  }
  group.__needle = needle;
  group.__knobCx = knob.cx;
  group.__knobCy = knob.cy;
}

function clamp(value, low, high) {
  return Math.min(high, Math.max(low, value));
}

/** A knob's drawn travel spans 270 degrees. */
function knobAngle(permille) {
  return -135 + (270 * clamp(Number(permille) || 0, 0, 1000)) / 1000;
}

/** The attribute's starting position before the simulator reports one. */
function initialPermille(part, entry) {
  const raw = Number(part.attrs?.value);
  if (!Number.isFinite(raw)) return 500;
  if (entry?.id === "litz-slide-potentiometer") {
    return Math.round((clamp(raw, 0, 1023) / 1023) * 1000);
  }
  return Math.round(clamp(raw, 0, 100) * 10);
}

/** Repaint part state without rebuilding the canvas. */
function reflectSimState() {
  const sim = state.sim;
  if (!sim?.hasSession()) {
    for (const node of state.partNodes.values()) {
      node.classList.remove("is-led-on", "is-pressed", "is-switch-on");
    }
    return;
  }

  for (const [id, node] of state.partNodes) {
    const part = findPart(id);
    const entry = entryFor(part);
    const kind = controlKind(entry);
    if (!kind) continue;
    const code = sim.partState(id);

    if (kind === "led") {
      node.classList.toggle("is-led-on", code === 2);
      continue;
    }
    if (kind === "button") {
      node.classList.toggle(
        "is-pressed",
        code === 4 || (state.heldControl?.id === id && state.heldControl.kind === "button"),
      );
      continue;
    }
    if (kind === "switch") {
      node.classList.toggle("is-switch-on", code === 6);
      if (node.__switchKnob) {
        node.__switchKnob.setAttribute("x", code === 6 ? node.__switchOnX : node.__switchOffX);
      }
      continue;
    }
    if (kind === "pot" || kind === "rotary") {
      if (code === 11 && node.__needle) {
        const value = sim.partValue(id);
        node.__needle.setAttribute(
          "transform",
          `rotate(${knobAngle(value)} ${node.__knobCx} ${node.__knobCy})`,
        );
      }
    }
  }
}

function drawPreview() {
  if (!state.pendingPin || !state.pointer) return;
  const part = findPart(state.pendingPin.partId);
  const entry = entryFor(part);
  if (!part || !entry) return;
  const from = pinWorld(part, entry, state.pendingPin.pinName);
  if (!from) return;
  const points = routeWire(from, state.pointer);
  if (points.length < 2) return;
  ui.previewLayer.append(
    svgEl("path", { class: "wire-preview", d: polylinePath(points) }),
    svgEl("circle", { class: "preview-dot", cx: state.pointer.x, cy: state.pointer.y, r: 3 }),
  );
}

function renderCanvasHint() {
  const parts = state.project.circuit.parts.length;
  const wires = state.project.circuit.connections.length;
  const undrawn = state.project.circuit.connections.filter(
    (connection) => !wireIsDrawn(connection),
  ).length;
  if (state.pendingPin) {
    ui.canvasHint.textContent =
      `Wiring from ${state.pendingPin.partId}:${state.pendingPin.pinName}. ` +
      "Click a second pin. Esc cancels.";
    return;
  }
  if (parts === 0) {
    ui.canvasHint.textContent = "Drag a part; click a pin, then another, to wire.";
    return;
  }
  ui.canvasHint.textContent =
    `${parts} part${parts === 1 ? "" : "s"}, ${wires} connection${wires === 1 ? "" : "s"}` +
    `${undrawn > 0 ? ` (${undrawn} to a rail or host, not drawn)` : ""}. ` +
    "Drag a part; click a pin, then another, to wire.";
}

// Selection and edits -------------------------------------------------------

function selectPart(id) {
  const current = state.selected;
  if (current?.kind === "part" && current.id === id) return;
  state.selected = { kind: "part", id };
  renderCanvas();
  renderInspector();
  renderStatus();
}

function selectWire(index) {
  const current = state.selected;
  if (current?.kind === "wire" && current.index === index) return;
  state.selected = { kind: "wire", index };
  renderCanvas();
  renderInspector();
  renderStatus();
}

function cancelPending() {
  if (!state.pendingPin) return;
  state.pendingPin = null;
  renderCanvas();
}

function addPartFromCatalog(entryId) {
  if (!state.catalog) return;
  const entry = state.catalog.byId.get(entryId);
  if (!entry) return;
  const circuit = state.project.circuit;
  const id = uniquePartId(circuit, slugForType(entry.id));
  const placement = nextPlacement(
    circuit.parts,
    entry,
    (part) => partBounds(part, entryFor(part) ?? { kind: "part", pins: [], attrs: [] }),
  );
  const part = {
    id,
    type: entry.id,
    attrs: defaultAttrs(entry),
    left: placement.left,
    top: placement.top,
    rotate: 0,
  };
  state.project.circuit = withPartAdded(circuit, part);
  state.selected = { kind: "part", id };
  state.pendingPin = null;
  markDirty();
  refreshValidation();
  renderCanvas();
  renderInspector();
  log("info", `Added ${entry.name} as ${id}.`);
}

function handlePinClick(partId, pinName, pinKind) {
  if (!state.pendingPin) {
    state.pendingPin = { partId, pinName };
    state.selected = { kind: "part", id: partId };
    renderCanvas();
    renderInspector();
    renderStatus();
    return;
  }

  const from = state.pendingPin;
  if (from.partId === partId && from.pinName === pinName) {
    cancelPending();
    return;
  }

  const fromEntry = entryFor(findPart(from.partId));
  const fromKind = findPin(fromEntry, from.pinName)?.kind ?? "";
  if (!pinKindAccepts(fromKind, pinKind)) {
    log("warning", `${from.partId}:${from.pinName} (${fromKind}) cannot be wired to ${partId}:${pinName} (${pinKind}).`);
    return;
  }

  const a = `${from.partId}:${from.pinName}`;
  const b = `${partId}:${pinName}`;
  if (connectionExists(state.project.circuit, a, b)) {
    log("warning", `${a} and ${b} are already connected.`);
    cancelPending();
    return;
  }

  state.project.circuit = withConnectionAdded(state.project.circuit, [a, b, DEFAULT_WIRE_COLOR, []]);
  const index = state.project.circuit.connections.length - 1;
  state.pendingPin = null;
  state.selected = { kind: "wire", index };
  markDirty();
  refreshValidation();
  renderCanvas();
  renderInspector();
}

function deleteSelection() {
  const selected = state.selected;
  if (!selected) return;
  if (selected.kind === "part") {
    if (!findPart(selected.id)) return;
    state.project.circuit = withPartRemoved(state.project.circuit, selected.id);
    if (state.pendingPin?.partId === selected.id) state.pendingPin = null;
    state.selected = null;
    markDirty();
    refreshValidation();
    renderCanvas();
    renderInspector();
    log("info", `Removed ${selected.id}.`);
    return;
  }
  if (selected.kind === "wire") {
    state.project.circuit = withConnectionRemoved(state.project.circuit, selected.index);
    state.selected = null;
    markDirty();
    refreshValidation();
    renderCanvas();
    renderInspector();
    log("info", "Removed a connection.");
  }
}

function setPartAttr(id, name, value) {
  const part = findPart(id);
  if (!part) return;
  state.project.circuit = withPartAttrs(state.project.circuit, id, {
    ...part.attrs,
    [name]: value,
  });
  markDirty();
  refreshValidation();
  renderCanvas();
}

function rotatePart(id) {
  const part = findPart(id);
  if (!part) return;
  const rotate = ((Number(part.rotate) || 0) + 90) % 360;
  state.project.circuit = {
    ...state.project.circuit,
    parts: state.project.circuit.parts.map((candidate) =>
      candidate.id === id ? { ...candidate, rotate } : candidate,
    ),
  };
  markDirty();
  refreshValidation();
  renderCanvas();
  renderInspector();
}

function commitPartId(part, input) {
  const next = input.value.trim();
  if (next === part.id) {
    input.value = part.id;
    return;
  }
  if (!/^[A-Za-z0-9_-]+$/.test(next) || next.startsWith("$")) {
    log("error", `Id ${JSON.stringify(next)} is not allowed. Use letters, digits, "_" and "-".`);
    input.value = part.id;
    return;
  }
  if (state.project.circuit.parts.some((candidate) => candidate.id === next)) {
    log("error", `Id ${JSON.stringify(next)} is already in use.`);
    input.value = part.id;
    return;
  }
  state.project.circuit = withPartRenamed(state.project.circuit, part.id, next);
  if (state.selected?.kind === "part" && state.selected.id === part.id) {
    state.selected = { kind: "part", id: next };
  }
  if (state.pendingPin?.partId === part.id) state.pendingPin.partId = next;
  markDirty();
  refreshValidation();
  renderCanvas();
  renderInspector();
}

function setWireColor(index, color) {
  const connection = state.project.circuit.connections[index];
  if (!connection) return;
  state.project.circuit = {
    ...state.project.circuit,
    connections: state.project.circuit.connections.map((candidate, at) =>
      at === index ? [candidate[0], candidate[1], color, candidate[3] ?? []] : candidate,
    ),
  };
  markDirty();
  renderCanvas();
}

// Inspector ----------------------------------------------------------------

function renderInspector() {
  const body = ui.inspectorBody;
  body.replaceChildren();
  const selection = state.selected;
  if (!selection) {
    body.append(
      element("p", "inspector-empty", "Select a part or a wire on the canvas."),
    );
    body.append(
      element(
        "p",
        "inspector-empty",
        "Delete removes the selection. Esc cancels a wire in progress.",
      ),
    );
    return;
  }
  if (selection.kind === "part") {
    renderPartInspector(body, selection.id);
  } else {
    renderWireInspector(body, selection.index);
  }
}

function renderPartInspector(body, id) {
  const part = findPart(id);
  if (!part) {
    state.selected = null;
    renderInspector();
    return;
  }
  const entry = entryFor(part);
  body.append(element("div", "inspector-kicker", "Part"));
  const title = element("h3", "inspector-title", entry?.name ?? part.type);
  if (entry?.custom) title.append(element("span", "custom-badge", "custom"));
  body.append(title);

  const idInput = document.createElement("input");
  idInput.type = "text";
  idInput.value = part.id;
  idInput.spellcheck = false;
  idInput.autocomplete = "off";
  idInput.addEventListener("change", () => commitPartId(part, idInput));
  body.append(field("Id", idInput));

  body.append(field("Type", readonly(part.type)));
  body.append(
    field("Position", readonly(`${Math.round(part.left)}, ${Math.round(part.top)} · ${Number(part.rotate) || 0}°`)),
  );

  const rotate = element("button", null, "Rotate 90°");
  rotate.type = "button";
  rotate.addEventListener("click", () => rotatePart(part.id));
  const remove = element("button", "danger", "Remove part");
  remove.type = "button";
  remove.addEventListener("click", () => {
    state.selected = { kind: "part", id: part.id };
    deleteSelection();
  });
  const actions = element("div", "button-row");
  actions.append(rotate, remove);
  body.append(actions);

  body.append(element("div", "section-label", "Attributes"));
  if (!entry) {
    body.append(element("p", "inspector-empty", "This type is not in the catalog."));
  } else if (entry.attrs.length === 0) {
    body.append(element("p", "inspector-empty", "This part has no attributes."));
  } else {
    for (const def of entry.attrs) {
      body.append(attributeControl(part, def));
    }
  }
}

function attributeControl(part, def) {
  const current = part.attrs[def.name] ?? def.default ?? null;

  if (def.type === "enum") {
    const select = document.createElement("select");
    for (const value of def.values) {
      const option = document.createElement("option");
      option.value = value;
      option.textContent = value;
      select.append(option);
    }
    select.value = String(current ?? def.values[0] ?? "");
    select.addEventListener("change", () => setPartAttr(part.id, def.name, select.value));
    return field(def.name, select);
  }

  if (def.type === "bool") {
    const label = element("label", "check-field");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.checked = Boolean(current);
    input.addEventListener("change", () => setPartAttr(part.id, def.name, input.checked));
    label.append(input, element("span", null, def.name));
    return label;
  }

  const input = document.createElement("input");
  if (def.type === "int" || def.type === "float") {
    input.type = "number";
    input.step = def.type === "int" ? "1" : "any";
    input.value = String(Number(current ?? 0));
  } else {
    input.type = "text";
    input.value = String(current ?? "");
  }
  input.addEventListener("change", () => {
    if (def.type === "int") {
      const next = Number(input.value);
      if (!Number.isInteger(next)) {
        log("warning", `${def.name} must be a whole number.`);
        input.value = String(current ?? 0);
        return;
      }
      setPartAttr(part.id, def.name, next);
      return;
    }
    if (def.type === "float") {
      const next = Number(input.value);
      if (!Number.isFinite(next)) {
        log("warning", `${def.name} must be a number.`);
        input.value = String(current ?? 0);
        return;
      }
      setPartAttr(part.id, def.name, next);
      return;
    }
    setPartAttr(part.id, def.name, input.value);
  });
  return field(def.name, input);
}

function renderWireInspector(body, index) {
  const connection = state.project.circuit.connections[index];
  if (!connection) {
    state.selected = null;
    renderInspector();
    return;
  }
  body.append(element("div", "inspector-kicker", "Connection"));
  body.append(
    element("h3", "inspector-title", `${connection[0]} to ${connection[1]}`),
  );
  body.append(field("From", readonly(connection[0])));
  body.append(field("To", readonly(connection[1])));

  const select = document.createElement("select");
  for (const name of WIRE_COLOR_NAMES) {
    const option = document.createElement("option");
    option.value = name;
    option.textContent = name;
    select.append(option);
  }
  const current = String(connection[2] || DEFAULT_WIRE_COLOR).toLowerCase();
  select.value = WIRE_COLOR_NAMES.includes(current) ? current : DEFAULT_WIRE_COLOR;
  select.addEventListener("change", () => setWireColor(index, select.value));
  body.append(field("Color", select));

  const remove = element("button", "danger", "Remove connection");
  remove.type = "button";
  remove.addEventListener("click", () => {
    state.selected = { kind: "wire", index };
    deleteSelection();
  });
  const actions = element("div", "button-row");
  actions.append(remove);
  body.append(actions);
}

// Project panel and diagnostics --------------------------------------------

function renderProjectPanel() {
  const stats = ui.projectStats;
  stats.replaceChildren();
  const row = (label, value) => {
    const wrap = document.createElement("div");
    wrap.append(element("dt", null, label), element("dd", null, String(value)));
    stats.append(wrap);
  };
  row("Format", `${state.project.format} v${state.project.version}`);
  row("Parts", state.project.circuit.parts.length);
  row("Connections", state.project.circuit.connections.length);
  row("Files", state.project.files.length);
  row("Catalog", state.catalog ? `${state.catalog.entries.length} entries` : "unavailable");

  ui.storageNote.textContent = state.projectHandle
    ? `Save target: ${state.projectHandle.name ?? "file"}`
    : supportsFileSystemAccess()
      ? "Save asks for a file location."
      : "Save downloads a file.";

  const errors = state.validation.errors.length;
  const warnings = state.validation.warnings.length;
  ui.validationSummary.className =
    `validation-summary ${errors > 0 ? "bad" : warnings > 0 ? "warn" : "ok"}`;
  ui.validationSummary.textContent =
    errors > 0
      ? `${errors} error${errors === 1 ? "" : "s"}, ${warnings} warning${warnings === 1 ? "" : "s"}`
      : warnings > 0
        ? `${warnings} warning${warnings === 1 ? "" : "s"}`
        : "Validation: OK";

  ui.validationList.replaceChildren();
  for (const issue of state.validation.issues.slice(0, 8)) {
    const item = element("li", issue.severity, `${issue.code}: ${issue.message}`);
    ui.validationList.append(item);
  }
  if (state.validation.issues.length > 8) {
    ui.validationList.append(
      element("li", null, `${state.validation.issues.length - 8} more in Diagnostics.`),
    );
  }
}

function renderDiagnostics() {
  const list = ui.diagnostics;
  list.replaceChildren();

  const entries = [];
  if (state.catalogError) {
    entries.push({ severity: "error", message: state.catalogError });
  }
  for (const issue of state.validation.issues) {
    entries.push({ severity: issue.severity, message: `${issue.code}: ${issue.message}` });
  }
  for (const entry of state.log) {
    entries.push({ severity: entry.severity, message: entry.message, at: entry.at });
  }

  if (entries.length === 0) {
    const item = element("li", "empty", "No diagnostics.");
    list.append(item);
  } else {
    for (const entry of entries.slice(0, 200)) {
      const item = element("li", `diag diag-${entry.severity}`);
      item.append(
        element("span", "diag-kind", entry.severity),
        element("span", "diag-message", entry.message),
      );
      list.append(item);
    }
  }

  const count = state.validation.issues.length + (state.catalogError ? 1 : 0);
  ui.diagCount.hidden = count === 0;
  ui.diagCount.textContent = String(count);
}

function log(severity, message) {
  state.log.unshift({ severity, message, at: Date.now() });
  if (state.log.length > 200) state.log.length = 200;
  renderDiagnostics();
  if (severity === "error") switchTab("diagnostics");
}

function switchTab(tab) {
  state.tab = tab;
  const serial = tab === "serial";
  ui.serialPanel.hidden = !serial;
  ui.diagnosticsPanel.hidden = serial;
  ui.tabSerial.setAttribute("aria-selected", String(serial));
  ui.tabDiagnostics.setAttribute("aria-selected", String(!serial));
}

function refreshValidation() {
  state.validation = validateProject(state.project, state.catalog);
  renderProjectPanel();
  renderDiagnostics();
}

// Status and catalog -------------------------------------------------------

function renderStatus() {
  ui.dirtyBadge.hidden = !state.dirty;
  ui.saveState.textContent = state.statusText;
  document.title = `Litz Workbench · ${state.project.name}${state.dirty ? " *" : ""}`;
}

function syncProjectNameInput() {
  ui.projectName.value = state.project.name;
}

function markDirty() {
  state.dirty = true;
  state.statusText = "Unsaved changes";
  renderStatus();
}

// Simulator ----------------------------------------------------------------

async function ensureSimulator() {
  if (!state.sim) {
    state.sim = createRunController({
      onFrame: () => {
        consumeSerial();
        reflectSimState();
        renderSimStatus();
        pollScope();
        drawDisplay();
      },
      onError: (message) => log("error", `Simulator: ${message}`),
    });
  }
  try {
    await state.sim.init();
    state.simInitError = "";
    return state.sim;
  } catch (error) {
    state.simInitError = error.message;
    renderSimStatus();
    updateFirmwareHint();
    return null;
  }
}

/** Paint the board's display when its pixels changed. */
function drawDisplay() {
  const sim = state.sim;
  if (!sim) return;
  const info = sim.displayInfo();
  if (!info) {
    ui.displayPanel.hidden = true;
    return;
  }
  ui.displayPanel.hidden = false;
  ui.displaySize.textContent = `${info.width} × ${info.height}`;
  const frame = sim.display();
  if (!frame) return;
  if (ui.displayCanvas.width !== frame.width || ui.displayCanvas.height !== frame.height) {
    ui.displayCanvas.width = frame.width;
    ui.displayCanvas.height = frame.height;
  }
  const context = ui.displayCanvas.getContext("2d");
  const image = new ImageData(
    new Uint8ClampedArray(frame.data.buffer, frame.data.byteOffset, frame.data.byteLength),
    frame.width,
    frame.height,
  );
  context.putImageData(image, 0, 0);
}

/** Panel pixels for a pointer position over the display canvas. */
function displayPoint(event) {
  const rect = ui.displayCanvas.getBoundingClientRect();
  const width = ui.displayCanvas.width || 1;
  const height = ui.displayCanvas.height || 1;
  return {
    x: Math.max(0, Math.min(width - 1, ((event.clientX - rect.left) * width) / rect.width)),
    y: Math.max(0, Math.min(height - 1, ((event.clientY - rect.top) * height) / rect.height)),
  };
}

ui.displayCanvas.addEventListener("pointerdown", (event) => {
  if (!state.sim) return;
  const { x, y } = displayPoint(event);
  state.sim.touch(x, y, true);
  ui.displayCanvas.setPointerCapture?.(event.pointerId);
});

ui.displayCanvas.addEventListener("pointermove", (event) => {
  if (!state.sim || !ui.displayCanvas.hasPointerCapture?.(event.pointerId)) return;
  const { x, y } = displayPoint(event);
  state.sim.touch(x, y, true);
});

for (const type of ["pointerup", "pointercancel"]) {
  ui.displayCanvas.addEventListener(type, (event) => {
    if (!state.sim) return;
    const { x, y } = displayPoint(event);
    state.sim.touch(x, y, false);
  });
}

function consumeSerial() {
  const sim = state.sim;
  if (!sim) return;
  const chunk = sim.takeSerial();
  if (!chunk) return;
  ui.serial.textContent = sim.serial;
  ui.serial.scrollTop = ui.serial.scrollHeight;
}

function boardPart() {
  return (
    state.project.circuit.parts.find((part) => {
      const entry = entryFor(part);
      return entry?.kind === "board" || String(part.type).startsWith("board-");
    }) ?? null
  );
}

function requiredFirmwareMissing() {
  const missing = [];
  if (!boardPart()) missing.push("a development board in the circuit");
  if (!state.firmware.app) missing.push("an application image (.bin or .hex)");
  if (state.firmwareKind !== "application") {
    if (!state.firmware.boot) missing.push("the bootloader image");
    if (!state.firmware.ptable) missing.push("the partition table image");
  }
  if (state.firmwareKind === "full" && !state.firmware.rom) {
    missing.push("the mask ROM image");
  }
  return missing;
}

function renderSimStatus() {
  const sim = state.sim;
  let text = "Simulator idle";
  let tone = "hint";

  if (state.simInitError) {
    text = `Simulator unavailable: ${state.simInitError}`;
    tone = "hint bad";
  } else if (sim?.state() === "loading") {
    text = "Loading simulator…";
  } else if (sim?.fatal) {
    text = `Simulation stopped: ${sim.error}`;
    tone = "hint bad";
  } else if (!sim?.hasSession()) {
    const missing = requiredFirmwareMissing();
    text =
      missing.length > 0
        ? `Simulator ready · missing ${missing.join(", ")}`
        : "Simulator ready · press Load & Run";
    if (missing.length > 0) tone = "hint warn";
  } else {
    const faults = sim.conflicts;
    text =
      `${formatMs(sim.simMs)} · ${faults} electrical fault${faults === 1 ? "" : "s"} · ` +
      (sim.running ? "running" : "paused");
    if (sim.firstConflict) text += ` · ${sim.firstConflict}`;
    if (faults > 0) tone = "hint bad";
  }

  if (ui.simStatus.textContent !== text) ui.simStatus.textContent = text;
  ui.simStatus.className = tone;
  ui.simStatus.title = sim?.error ?? "";

  ui.simRun.disabled = !sim;
  ui.simPause.disabled = !sim?.running;
  ui.simReset.disabled = !sim?.hasSession();
  ui.simLoad.disabled = !sim;
}

function updateFirmwareHint() {
  const notes = [];
  const board = boardPart();
  if (!board) {
    notes.push("Add a board from Components; the simulator needs one development board.");
  } else if (board.type !== "board-esp32-c3-devkitm-1") {
    notes.push(
      `This browser build runs board-esp32-c3-devkitm-1; the circuit uses ${board.type}.`,
    );
  }

  if (!state.firmware.app) {
    notes.push("Choose the application image (.bin or .hex).");
  } else {
    const app = state.firmware.app;
    notes.push(`${app.name}: ${app.kind.toUpperCase()}, ${app.bytes.length} bytes.`);
  }

  if (state.firmwareKind !== "application") {
    if (!state.firmware.boot) notes.push("This image kind also needs a bootloader.");
    if (!state.firmware.ptable) notes.push("This image kind also needs a partition table.");
  }
  if (state.firmwareKind === "full") {
    if (!state.firmware.rom) notes.push("This image kind also needs a mask ROM.");
    else notes.push(`ROM: ${state.firmware.rom.name}.`);
  } else if (state.firmware.rom) {
    notes.push("The ROM file is ignored unless the image kind includes it.");
  }
  if (
    state.firmwareKind === "application" &&
    (state.firmware.boot || state.firmware.ptable)
  ) {
    notes.push("The bootloader and partition table are ignored with the application-only image kind.");
  }

  ui.firmwareHint.textContent = notes.join(" ");
  ui.firmwareHint.className = state.firmware.app ? "hint" : "hint warn";
}

async function readFirmwareField(input) {
  const file = input.files?.[0];
  if (!file) return null;
  try {
    const image = firmwareImage(file.name, await file.arrayBuffer());
    return { name: file.name, ...image };
  } catch (error) {
    log("error", `Could not read ${file.name}: ${error.message}`);
    input.value = "";
    return null;
  }
}

async function onFirmwareChange(slot, input) {
  const image = await readFirmwareField(input);
  state.firmware[slot] = image;
  updateFirmwareHint();
  renderSimStatus();
  if (image) {
    log("info", `Firmware ${image.name}: ${image.kind.toUpperCase()}, ${image.bytes.length} bytes.`);
  }
}

async function loadAndRun() {
  const sim = await ensureSimulator();
  if (!sim) return;

  const missing = requiredFirmwareMissing();
  if (missing.length > 0) {
    log("warning", `Cannot run yet. Choose ${missing.join(", ")}.`);
    updateFirmwareHint();
    renderSimStatus();
    return;
  }

  const kind = state.firmwareKind;
  const input = {
    diagramText: JSON.stringify(diagramFromProject(state.project)),
    app: state.firmware.app.bytes,
    rom: kind === "full" ? state.firmware.rom?.bytes : null,
    boot: kind === "application" ? null : state.firmware.boot?.bytes,
    ptable: kind === "application" ? null : state.firmware.ptable?.bytes,
  };

  try {
    ui.simLoad.disabled = true;
    await sim.load(input);
    resetScopeView();
    registerScopeChannels();
    ui.serial.textContent = "";
    renderCanvas();
    sim.run();
    reflectSimState();
    log("info", `Simulator started with ${state.firmware.app.name}.`);
  } catch (error) {
    log("error", `Could not start the simulator: ${error.message}`);
  } finally {
    renderSimStatus();
    updateFirmwareHint();
  }
}

async function resetSimulator() {
  const sim = state.sim;
  if (!sim?.hasSession()) {
    log("warning", "There is no running session to reset. Press Load & Run first.");
    return;
  }
  const wasRunning = sim.running;
  try {
    await sim.reset();
    resetScopeView();
    registerScopeChannels();
    ui.serial.textContent = sim.serial;
    if (wasRunning) sim.run();
    consumeSerial();
    renderCanvas();
    reflectSimState();
    renderSimStatus();
  } catch (error) {
    log("error", `Could not reset the simulator: ${error.message}`);
  }
}

function applyControlAt(id, point) {
  const part = findPart(id);
  const entry = entryFor(part);
  if (!part || !entry || !state.sim?.hasSession()) return;
  const permille = controlPermille(part, entry, point);
  if (permille === null) return;
  state.sim.setControl(id, permille);
  reflectSimState();
}

function releaseHeldControl() {
  const held = state.heldControl;
  if (!held) return;
  state.heldControl = null;
  if (held.kind === "button" || held.kind === "rotary") {
    state.sim?.press(held.id, false);
  }
  reflectSimState();
  renderSimStatus();
}

// Scope --------------------------------------------------------------------

function addScopeChannel() {
  const point = ui.scopePoint.value.trim();
  if (!point) {
    log("warning", "Enter a probe point such as esp:GPIO8.");
    return;
  }
  const sim = state.sim;
  if (!sim?.hasSession()) {
    log("warning", "Load firmware before adding a probe.");
    return;
  }
  const name = ui.scopeName.value.trim() || point;
  if (state.scopeChannels.some((channel) => channel.name === name)) {
    log("warning", `Probe name "${name}" is already in use.`);
    return;
  }

  try {
    sim.addProbe(name, point);
  } catch (error) {
    log("error", `Probe refused: ${error.message}`);
    return;
  }

  state.scopeChannels.push({
    name,
    point,
    color: SCOPE_COLORS[state.scopeChannels.length % SCOPE_COLORS.length],
    hidden: false,
  });
  ui.scopePoint.value = "";
  ui.scopeName.value = "";
  ui.scopePoint.focus();
  renderScopeChannels();
  fetchScope();
  log("info", `Probe ${name} on ${point}.`);
}

function addScopeDecode() {
  const spec = ui.decodeSpec.value.trim();
  if (!spec) {
    log("warning", "Enter a decoder such as uart:rx=tx.");
    return;
  }
  const sim = state.sim;
  if (!sim?.hasSession()) {
    log("warning", "Load firmware before adding a decoder.");
    return;
  }

  try {
    sim.addDecode(spec);
  } catch (error) {
    log("error", `Decoder refused: ${error.message}`);
    return;
  }

  state.scopeDecodes.push({ spec, label: provisionalDecodeLabel(spec) });
  ui.decodeSpec.value = "";
  renderScopeDecodes();
  fetchScope();
}

/** The label shown until the scope document names the decoder itself. */
function provisionalDecodeLabel(spec) {
  const named = /\bname=([^,\s]+)/.exec(spec);
  if (named) return named[1];
  const [kind, channels = ""] = spec.split(":", 2);
  const first = channels.split(",")[0]?.split("=")[0]?.trim() ?? "";
  return first ? `${kind.trim()} ${first}` : kind.trim() || spec;
}

/** Register every listed channel with a fresh session. */
function registerScopeChannels() {
  const sim = state.sim;
  if (!sim?.hasSession()) return;
  for (const channel of state.scopeChannels) {
    try {
      sim.addProbe(channel.name, channel.point);
    } catch (error) {
      log("warning", `Probe ${channel.name}: ${error.message}`);
    }
  }
  for (const decoder of state.scopeDecodes) {
    try {
      sim.addDecode(decoder.spec);
    } catch (error) {
      log("warning", `Decoder ${decoder.label}: ${error.message}`);
    }
  }
  fetchScope();
}

function resetScopeView() {
  state.scopeDoc = null;
  state.scopePollAt = 0;
  state.scopeRunning = false;
  state.scopeHover = null;
  updateScopeMeasurements();
  renderScopeDecodes();
  updateScopeReadout();
  drawScopeTrace();
}

function pollScope() {
  const sim = state.sim;
  if (!sim?.hasSession()) {
    state.scopeRunning = false;
    return;
  }
  if (sim.running) {
    if (state.scopePollAt && Date.now() - state.scopePollAt < 100) return;
    fetchScope();
  } else if (state.scopeRunning) {
    // One more read after a pause or a stop so the last record is complete.
    fetchScope();
  }
  state.scopeRunning = sim.running;
}

function fetchScope() {
  const sim = state.sim;
  if (!sim?.hasSession()) return;
  state.scopePollAt = Date.now();
  const doc = sim.scope();
  if (!doc) return;
  state.scopeDoc = doc;
  updateScopeMeasurements();
  renderScopeDecodes();
  drawScopeTrace();
  updateScopeReadout();
}

/** `1.000 kHz · duty 50.0% · 4 edges`, omitting values the record lacks. */
function scopeMeasurements(probe) {
  if (!probe) return "";
  const parts = [];

  if (probe.frequency_hz != null && Number.isFinite(Number(probe.frequency_hz))) {
    const hz = Number(probe.frequency_hz);
    parts.push(hz >= 1000 ? `${(hz / 1000).toFixed(3)} kHz` : `${Math.round(hz)} Hz`);
  }
  if (probe.duty != null && Number.isFinite(Number(probe.duty))) {
    parts.push(`duty ${(Number(probe.duty) * 100).toFixed(1)}%`);
  }
  if (probe.edges != null && Number.isFinite(Number(probe.edges))) {
    const edges = Math.round(Number(probe.edges));
    parts.push(`${edges} edge${edges === 1 ? "" : "s"}`);
  }

  return parts.join(" · ");
}

function updateScopeMeasurements() {
  for (const channel of state.scopeChannels) {
    const node = state.scopeMeasureNodes.get(channel.name);
    if (!node) continue;
    node.textContent = scopeMeasurements(probeFor(channel));
  }
}

function renderScopeChannels() {
  const list = ui.scopeChannels;
  list.replaceChildren();
  state.scopeMeasureNodes.clear();
  if (state.scopeChannels.length === 0) {
    list.append(element("li", "scope-empty", "No probes"));
    return;
  }
  for (const channel of state.scopeChannels) {
    const item = element("li", `scope-channel${channel.hidden ? " is-hidden" : ""}`);
    const row = element("div", "scope-channel-row");
    const swatch = element("span", "swatch");
    swatch.style.background = channel.color;
    const toggle = element("button", "ch-toggle", channel.hidden ? "Show" : "Hide");
    toggle.type = "button";
    toggle.setAttribute("aria-pressed", String(channel.hidden));
    toggle.addEventListener("click", () => {
      channel.hidden = !channel.hidden;
      renderScopeChannels();
      drawScopeTrace();
    });
    row.append(
      swatch,
      element("span", "ch-name", channel.name),
      element("span", "ch-point", channel.point),
      toggle,
    );
    const measure = element("div", "ch-measure");
    state.scopeMeasureNodes.set(channel.name, measure);
    item.append(row, measure);
    list.append(item);
  }
  updateScopeMeasurements();
}

function renderScopeDecodes() {
  const list = ui.scopeDecodes;
  list.replaceChildren();
  if (state.scopeDecodes.length === 0 || !state.scopeDoc) return;
  const decodes = Array.isArray(state.scopeDoc.decodes) ? state.scopeDoc.decodes : [];

  for (const decoder of state.scopeDecodes) {
    const entry =
      decodes.find((item) => item?.spec === decoder.spec) ??
      decodes.find((item) => item?.name === decoder.label) ??
      null;
    const frames = Array.isArray(entry?.frames) ? entry.frames : [];
    const item = element("li", "scope-decode");
    item.append(
      element("span", "decode-name", entry?.name ?? decoder.label),
      element("span", "decode-count", `${frames.length} frame${frames.length === 1 ? "" : "s"}`),
    );

    const body = element("div", "decode-frames");
    const shown = frames.slice(-SCOPE_DECODE_FRAMES);
    if (frames.length > shown.length) {
      body.append(
        element("div", "decode-more", `+${frames.length - shown.length} earlier frames`),
      );
    }
    for (const frame of shown) {
      const line = element("div", "decode-frame");
      line.append(element("span", "frame-text", String(frame?.text ?? "")));
      if (typeof frame?.error === "string" && frame.error) {
        line.append(element("span", "frame-error", frame.error));
      }
      body.append(line);
    }
    item.append(body);
    list.append(item);
  }
}

function probeFor(channel) {
  const probes = state.scopeDoc?.probes;
  if (!Array.isArray(probes)) return null;
  return probes.find((probe) => probe.name === channel.name) ?? null;
}

/** The visible interval in nanoseconds. `all` starts at the first sample. */
function scopeWindowNs(doc) {
  const probes = Array.isArray(doc?.probes) ? doc.probes : [];
  let lastNs = Math.max(0, (Number(doc?.ms) || 0) * 1e6);
  let firstNs = lastNs;
  for (const probe of probes) {
    for (const samples of [probe.digital, probe.volts]) {
      for (const sample of samples ?? []) {
        const time = Number(sample[0]) || 0;
        if (time < firstNs) firstNs = time;
        if (time > lastNs) lastNs = time;
      }
    }
  }

  const setting = ui.scopeWindow.value;
  let startNs;
  let spanNs;
  if (setting === "all") {
    startNs = firstNs;
    spanNs = lastNs - firstNs;
  } else {
    const widthNs = (Number(setting) || 100) * 1e6;
    startNs = Math.max(0, lastNs - widthNs);
    spanNs = lastNs - startNs;
  }
  if (!(spanNs > 0)) spanNs = (Number(setting) || 100) * 1e6;
  return { startNs, endNs: startNs + spanNs, spanNs };
}

function heldSample(samples, timeNs) {
  if (!Array.isArray(samples)) return null;
  let held = null;
  for (const sample of samples) {
    if (Number(sample[0]) > timeNs) break;
    held = sample;
  }
  return held;
}

function levelText(level) {
  const code = Number(level);
  return code === 1 ? "1" : code === 2 ? "z" : "0";
}

function updateScopeReadout() {
  const readout = ui.scopeReadout;
  if (!readout) return;
  const hover = state.scopeHover;
  if (!hover || !state.scopeDoc) {
    if (readout.textContent) readout.textContent = "";
    return;
  }

  const parts = [`t = ${(hover.timeNs / 1e6).toFixed(1)} ms`];
  for (const channel of state.scopeChannels) {
    if (channel.hidden) continue;
    const probe = probeFor(channel);
    if (!probe) continue;
    const digital = heldSample(probe.digital, hover.timeNs);
    const volts = heldSample(probe.volts, hover.timeNs);
    if (digital && volts) {
      parts.push(`${channel.name} = ${levelText(digital[1])}`);
      parts.push(`${Number(volts[1]).toFixed(2)} V`);
    } else if (digital) {
      parts.push(`${channel.name} = ${levelText(digital[1])}`);
    } else if (volts) {
      parts.push(`${channel.name} = ${Number(volts[1]).toFixed(2)} V`);
    } else {
      parts.push(`${channel.name} = -`);
    }
  }
  readout.textContent = parts.join("  ");
}

function onScopePointerMove(event) {
  if (!state.scopeDoc) return;
  const canvas = ui.scopeCanvas;
  const rect = canvas.getBoundingClientRect();
  const plotWidth = Math.max(1, rect.width - SCOPE_PAD.left - SCOPE_PAD.right);
  const view = scopeWindowNs(state.scopeDoc);
  const fraction = clamp(
    (event.clientX - rect.left - SCOPE_PAD.left) / plotWidth,
    0,
    1,
  );
  state.scopeHover = { timeNs: view.startNs + fraction * view.spanNs };
  updateScopeReadout();
  drawScopeTrace();
}

function onScopePointerLeave() {
  if (!state.scopeHover) return;
  state.scopeHover = null;
  updateScopeReadout();
  drawScopeTrace();
}

function strokeSegments(ctx, segments, color, dash) {
  if (segments.length === 0) return;
  ctx.save();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  if (dash) ctx.setLineDash(dash);
  ctx.beginPath();
  for (const [x0, y0, x1, y1] of segments) {
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
  }
  ctx.stroke();
  ctx.restore();
}

function drawDigitalTrace(ctx, probe, color, rowTop, rowHeight, toX, endNs) {
  const samples = Array.isArray(probe.digital) ? probe.digital : [];
  if (samples.length === 0) return;
  const inset = Math.min(2, rowHeight / 4);
  const yFor = (level) => {
    if (Number(level) === 2) return rowTop + rowHeight / 2;
    return Number(level) === 1 ? rowTop + inset : rowTop + rowHeight - inset;
  };

  const solid = [];
  const dashed = [];
  for (let index = 0; index < samples.length; index += 1) {
    const level = Number(samples[index][1]);
    const x0 = toX(Number(samples[index][0]));
    const x1 =
      index + 1 < samples.length ? toX(Number(samples[index + 1][0])) : toX(endNs);
    const y = yFor(level);
    (level === 2 ? dashed : solid).push([x0, y, x1, y]);
  }
  for (let index = 1; index < samples.length; index += 1) {
    const previous = Number(samples[index - 1][1]);
    const level = Number(samples[index][1]);
    if (previous === level) continue;
    const x = toX(Number(samples[index][0]));
    (previous === 2 || level === 2 ? dashed : solid).push([
      x,
      yFor(previous),
      x,
      yFor(level),
    ]);
  }

  strokeSegments(ctx, solid, color, null);
  strokeSegments(ctx, dashed, color, [3, 3]);
}

function drawAnalogTrace(ctx, probe, color, rowTop, rowHeight, toX) {
  const samples = Array.isArray(probe.volts) ? probe.volts : [];
  if (samples.length === 0) return;
  const inset = Math.min(2, rowHeight / 4);
  const yFor = (volts) =>
    rowTop +
    rowHeight -
    inset -
    clamp(Number(volts) / 3.3, 0, 1) * (rowHeight - inset * 2);

  ctx.save();
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  ctx.lineJoin = "round";
  ctx.beginPath();
  samples.forEach((sample, index) => {
    const x = toX(Number(sample[0]));
    const y = yFor(sample[1]);
    if (index === 0) ctx.moveTo(x, y);
    else ctx.lineTo(x, y);
  });
  ctx.stroke();
  ctx.restore();
}

function drawScopeAxis(ctx, view, plotWidth, height) {
  const axisY = height - SCOPE_PAD.bottom;
  const ticks = plotWidth >= 380 ? 5 : 4;
  const spanMs = view.spanNs / 1e6;
  ctx.save();
  ctx.fillStyle = "#8494a8";
  ctx.font = '10px ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
  ctx.textBaseline = "top";
  for (let index = 0; index < ticks; index += 1) {
    const fraction = index / (ticks - 1);
    const x = SCOPE_PAD.left + plotWidth * fraction;
    const ms = (view.startNs + view.spanNs * fraction) / 1e6;
    ctx.textAlign = index === 0 ? "left" : index === ticks - 1 ? "right" : "center";
    const label = spanMs >= 100 ? `${Math.round(ms)} ms` : `${ms.toFixed(1)} ms`;
    ctx.fillText(label, x, axisY + 2);
  }
  ctx.restore();
}

function drawScopeTrace() {
  const canvas = ui.scopeCanvas;
  if (!canvas) return;
  const width = canvas.clientWidth;
  const height = canvas.clientHeight;
  if (width <= 0 || height <= 0) return;

  const dpr = window.devicePixelRatio || 1;
  const pixelWidth = Math.max(1, Math.round(width * dpr));
  const pixelHeight = Math.max(1, Math.round(height * dpr));
  if (canvas.width !== pixelWidth) canvas.width = pixelWidth;
  if (canvas.height !== pixelHeight) canvas.height = pixelHeight;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const visible = state.scopeChannels.filter((channel) => !channel.hidden);
  if (!state.scopeDoc || visible.length === 0) return;

  const plotWidth = Math.max(1, width - SCOPE_PAD.left - SCOPE_PAD.right);
  const plotHeight = Math.max(1, height - SCOPE_PAD.top - SCOPE_PAD.bottom);
  const view = scopeWindowNs(state.scopeDoc);
  const toX = (ns) =>
    SCOPE_PAD.left + ((ns - view.startNs) / view.spanNs) * plotWidth;
  const rowHeight = plotHeight / visible.length;

  ctx.save();
  ctx.beginPath();
  ctx.rect(SCOPE_PAD.left, SCOPE_PAD.top, plotWidth, plotHeight);
  ctx.clip();

  visible.forEach((channel, index) => {
    const probe = probeFor(channel);
    if (!probe) return;
    const rowTop = SCOPE_PAD.top + index * rowHeight;

    ctx.strokeStyle = "rgba(132, 148, 168, 0.16)";
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(SCOPE_PAD.left, rowTop + rowHeight);
    ctx.lineTo(SCOPE_PAD.left + plotWidth, rowTop + rowHeight);
    ctx.stroke();

    const volts = Array.isArray(probe.volts) && probe.volts.length > 0;
    ctx.save();
    if (volts) ctx.globalAlpha = 0.45;
    drawDigitalTrace(ctx, probe, channel.color, rowTop, rowHeight, toX, view.endNs);
    ctx.restore();
    if (volts) {
      drawAnalogTrace(ctx, probe, channel.color, rowTop, rowHeight, toX);
    }
  });

  ctx.restore();
  drawScopeAxis(ctx, view, plotWidth, height);

  if (state.scopeHover) {
    const x = toX(state.scopeHover.timeNs);
    if (x >= SCOPE_PAD.left && x <= SCOPE_PAD.left + plotWidth) {
      ctx.strokeStyle = "rgba(217, 226, 236, 0.4)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, SCOPE_PAD.top);
      ctx.lineTo(x, SCOPE_PAD.top + plotHeight);
      ctx.stroke();
    }
  }
}

function renderCatalog() {
  const list = ui.catalogList;
  list.replaceChildren();
  ui.catalogCount.textContent = state.catalog
    ? `${state.catalog.entries.length} entries`
    : "";

  if (!state.catalog) {
    list.append(
      element(
        "div",
        "catalog-empty",
        state.catalogError || "Loading catalog.json…",
      ),
    );
    return;
  }

  const result = searchCatalog(state.catalog, ui.catalogSearch.value);
  const groups = groupedCatalog(result);
  if (groups.length === 0) {
    list.append(element("div", "catalog-empty", "No components match this search."));
    return;
  }

  for (const group of groups) {
    const section = element("section", "catalog-group");
    section.append(element("h3", null, group.label));
    for (const entry of group.items) {
      const button = element("button", "catalog-item");
      button.type = "button";
      button.dataset.addId = entry.id;
      const name = element("span", "cat-name");
      name.append(
        document.createTextNode(entry.name),
        element("span", "cat-id", entry.id),
      );
      if (entry.custom) name.append(element("span", "custom-badge", "custom"));
      button.append(name);
      if (entry.description) {
        button.append(element("span", "cat-desc", entry.description));
      }
      button.append(
        element(
          "span",
          "cat-pins",
          `${entry.pins.length} pin${entry.pins.length === 1 ? "" : "s"}${
            entry.attrs.length ? ` · ${entry.attrs.length} attr${entry.attrs.length === 1 ? "" : "s"}` : ""
          }`,
        ),
      );
      button.addEventListener("click", () => addPartFromCatalog(entry.id));
      section.append(button);
    }
    list.append(section);
  }
}

async function loadCatalog() {
  try {
    const response = await fetch("./catalog.json", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const entries = await response.json();
    state.catalogBase = indexCatalog(entries);
    state.catalogError = "";
    const errors = applyCustomParts();
    for (const error of errors) log("warning", `Custom part ignored: ${error.message}`);
    if (errors.length > 0) {
      // Keep only definitions that still validate.
      state.customParts = state.customEntries.map(definitionOf);
      persistCustomParts();
    }
    renderCustomParts();
    renderCatalog();
    refreshValidation();
    renderCanvas();
    renderInspector();
    log(
      "info",
      `Catalog loaded: ${state.catalog.entries.length} entries` +
        (state.customEntries.length ? `, ${state.customEntries.length} custom.` : "."),
    );
  } catch (error) {
    state.catalogBase = null;
    state.catalog = null;
    state.catalogError =
      `catalog.json could not be loaded (${error.message}). ` +
      "Serve this directory over HTTP with litz serve.";
    renderCatalog();
    refreshValidation();
    log("error", state.catalogError);
  }
}

// Custom parts --------------------------------------------------------------

function browserStorage() {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function persistCustomParts() {
  return saveCustomParts(browserStorage(), state.customParts);
}

/** Validate the stored definitions and rebuild the merged catalog index. */
function applyCustomParts() {
  const { entries, errors } = normalizeCustomParts(state.customParts, {
    catalog: state.catalogBase,
  });
  state.customEntries = entries;
  state.catalog = state.catalogBase ? mergeCatalog(state.catalogBase, entries) : null;
  return errors;
}

function customStatus(text, isError = false) {
  ui.customStatus.textContent = text;
  ui.customStatus.className = isError ? "hint bad" : "hint";
}

function renderCustomParts() {
  ui.customList.replaceChildren();
  if (state.customEntries.length === 0) {
    ui.customList.append(element("li", "custom-empty", "No saved parts."));
    return;
  }
  for (const entry of state.customEntries) {
    const item = element("li");
    const meta = element("span", "custom-meta");
    meta.append(
      element("strong", null, entry.name),
      element("span", "cat-id", entry.id),
      element("span", "hint", `${entry.pins.length} pin${entry.pins.length === 1 ? "" : "s"}`),
    );
    const remove = element("button", "danger small", "Delete");
    remove.type = "button";
    remove.addEventListener("click", () => deleteCustomPart(entry.id));
    item.append(meta, remove);
    ui.customList.append(item);
  }
}

function openCustomParts() {
  renderCustomParts();
  customStatus(
    state.customEntries.length
      ? `${state.customEntries.length} saved part${state.customEntries.length === 1 ? "" : "s"}.`
      : "No saved parts.",
  );
  if (!ui.customDialog.open) ui.customDialog.showModal();
}

/** Add or replace definitions (from the editor or an imported file). */
function applyCustomDefinitions(list, source) {
  const { entries, errors } = normalizeCustomParts(list, {
    catalog: state.catalogBase,
    existing: state.customParts,
  });
  if (entries.length === 0) {
    customStatus(errors.length > 0 ? errors.map((error) => error.message).join(" ") : "No definitions found.", true);
    return false;
  }
  for (const entry of entries) state.customParts = upsertCustomPart(state.customParts, entry);
  const ignored = applyCustomParts();
  const stored = persistCustomParts();
  renderCustomParts();
  renderCatalog();
  renderCanvas();
  renderInspector();
  refreshValidation();

  const names = entries.map((entry) => entry.name).join(", ");
  for (const entry of entries) log("info", `Custom part ${entry.name} saved.`);
  if (!stored) {
    customStatus(
      `Saved ${names} for this tab only; browser storage is unavailable, so it will not survive a reload.`,
      true,
    );
  } else if (errors.length > 0 || ignored.length > 0) {
    const problems = [...errors, ...ignored].map((error) => error.message).join(" ");
    customStatus(`Saved ${names}. Ignored: ${problems}`, true);
  } else {
    customStatus(`Saved ${names} from ${source}.`);
  }
  return true;
}

function saveCustomPartsFromEditor() {
  let list;
  try {
    list = parseCustomParts(ui.customEditor.value);
  } catch (error) {
    customStatus(error.message, true);
    return;
  }
  if (applyCustomDefinitions(list, "the editor")) ui.customEditor.value = "";
}

async function importCustomParts() {
  let file;
  try {
    file = await openText({ types: CUSTOM_TYPES });
  } catch (error) {
    customStatus(`Could not open a file: ${error.message}`, true);
    return;
  }
  if (!file) return;
  let list;
  try {
    list = parseCustomParts(file.text);
  } catch (error) {
    customStatus(`${file.name}: ${error.message}`, true);
    return;
  }
  applyCustomDefinitions(list, file.name);
}

async function exportCustomParts() {
  if (state.customEntries.length === 0) {
    customStatus("There is nothing to export.", true);
    return;
  }
  try {
    const result = await saveText({
      suggestedName: "custom-parts.json",
      text: serializeCustomParts(state.customEntries),
      types: CUSTOM_TYPES,
    });
    if (!result.cancelled) {
      customStatus(`Exported ${state.customEntries.length} part${state.customEntries.length === 1 ? "" : "s"} to ${result.name}.`);
    }
  } catch (error) {
    customStatus(`Could not export: ${error.message}`, true);
  }
}

function deleteCustomPart(id) {
  const entry = state.customEntries.find((candidate) => candidate.id === id);
  const name = entry?.name ?? id;
  const placed = state.project.circuit.parts.some((part) => part.type === id);
  state.customParts = removeCustomPart(state.customParts, id);
  applyCustomParts();
  persistCustomParts();
  renderCustomParts();
  renderCatalog();
  renderCanvas();
  renderInspector();
  refreshValidation();
  customStatus(
    placed
      ? `Deleted ${name}. Placed parts of this type now show as unknown.`
      : `Deleted ${name}.`,
    placed,
  );
  log("info", `Custom part ${name} deleted.`);
}

// Toolbar actions ----------------------------------------------------------

function projectFileName() {
  const base =
    String(state.project.name || "project")
      .trim()
      .replace(/[^A-Za-z0-9_.-]+/g, "-")
      .replace(/^-+|-+$/g, "") || "project";
  return base.toLowerCase().endsWith(PROJECT_EXTENSION) ? base : `${base}${PROJECT_EXTENSION}`;
}

function confirmDiscard() {
  if (!state.dirty) return true;
  return window.confirm("Discard unsaved changes?");
}

function newProject() {
  if (!confirmDiscard()) return;
  state.sim?.unload();
  resetScopeView();
  state.project = createProject("Untitled");
  state.projectHandle = null;
  state.selected = null;
  state.pendingPin = null;
  state.dirty = false;
  state.statusText = "In memory";
  syncProjectNameInput();
  renderAll();
  log("info", "New project in memory.");
}

async function openProject() {
  let file;
  try {
    file = await openText({ types: PROJECT_TYPES });
  } catch (error) {
    log("error", `Could not open a file: ${error.message}`);
    return;
  }
  if (!file) return;
  // Ask after the picker so the picker still runs under the click's user
  // activation.
  if (!confirmDiscard()) return;

  try {
    const { kind, project, warnings } = parseDocument(file.text, file.name);
    const imported = kind === "diagram";
    state.sim?.unload();
    resetScopeView();
    state.project = project;
    // A diagram.json is not a .litz file, so Save must not write back to the
    // handle the user opened it through.
    state.projectHandle = imported ? null : (file.handle ?? null);
    state.selected = null;
    state.pendingPin = null;
    state.dirty = imported;
    state.statusText = imported
      ? `Opened ${file.name} as an unsaved project`
      : `Opened ${file.name}`;
    syncProjectNameInput();
    renderAll();
    for (const warning of warnings) log("warning", warning);
    log(
      "info",
      imported
        ? `Opened ${file.name} as an unsaved project; Save writes a .litz file.`
        : `Opened project ${file.name}.`,
    );
  } catch (error) {
    log("error", `Could not open ${file.name}: ${error.message}`);
  }
}

async function saveProject() {
  const validation = validateProject(state.project, state.catalog);
  state.validation = validation;
  renderProjectPanel();
  renderDiagnostics();
  if (validation.errors.length > 0) {
    log("error", `Fix ${validation.errors.length} validation error${validation.errors.length === 1 ? "" : "s"} before saving.`);
    return;
  }

  try {
    const result = await saveText({
      suggestedName: projectFileName(),
      text: serializeProject(state.project),
      handle: state.projectHandle,
      types: PROJECT_TYPES,
    });
    if (result.cancelled) return;
    if (result.handle) state.projectHandle = result.handle;
    state.dirty = false;
    state.statusText =
      result.method === "download" ? "Downloaded .litz" : `Saved ${result.name}`;
    renderStatus();
    renderProjectPanel();
    log("info", `Saved ${result.name}.`);
  } catch (error) {
    log("error", `Could not save: ${error.message}`);
  }
}

async function importDiagram() {
  let file;
  try {
    file = await openText({ types: DIAGRAM_TYPES });
  } catch (error) {
    log("error", `Could not open a diagram: ${error.message}`);
    return;
  }
  if (!file) return;
  if (!confirmDiscard()) return;

  try {
    const raw = JSON.parse(file.text);
    if (!raw || !Array.isArray(raw.parts) || !Array.isArray(raw.connections)) {
      throw new Error("diagram.json needs a parts array and a connections array");
    }
    const { project, warnings } = projectFromDiagram(raw, nameFromFile(file.name));
    state.sim?.unload();
    resetScopeView();
    state.project = project;
    state.projectHandle = null;
    state.selected = null;
    state.pendingPin = null;
    state.dirty = true;
    state.statusText = `Imported ${file.name}`;
    syncProjectNameInput();
    renderAll();
    for (const warning of warnings) log("warning", warning);
    log("info", `Imported ${file.name}.`);
  } catch (error) {
    log("error", `Could not import ${file.name}: ${error.message}`);
  }
}

async function exportDiagram() {
  try {
    const result = await saveText({
      suggestedName: "diagram.json",
      text: `${JSON.stringify(diagramFromProject(state.project), null, 2)}\n`,
      types: DIAGRAM_TYPES,
    });
    if (result.cancelled) return;
    log(
      "info",
      result.method === "download"
        ? "Exported diagram.json as a download."
        : `Exported diagram.json to ${result.name}.`,
    );
  } catch (error) {
    log("error", `Could not export diagram.json: ${error.message}`);
  }
}

// Input handling -----------------------------------------------------------

function onPointerDown(event) {
  if (event.button !== 0) return;
  const target = event.target;
  const point = toCanvasPoint(event);
  state.pointer = point;

  if (target.closest?.("[data-pin]")) return; // handled by click

  const partEl = target.closest?.("[data-part-id]");
  if (partEl) {
    const id = partEl.dataset.partId;
    const part = findPart(id);
    const entry = entryFor(part);

    // While a session is loaded, a control takes the pointer: pressing a
    // button, throwing a switch, or dragging a knob. Everything else keeps the
    // editor's drag behaviour.
    if (part && entry && state.sim?.hasSession()) {
      const control = controlAt(part, entry, point);
      if (control?.inside && control.kind !== "led") {
        selectPart(id);
        state.drag = null;
        state.heldControl = { id, kind: control.kind };
        if (control.kind === "button" || control.kind === "switch" || control.kind === "rotary") {
          state.sim.press(id, true);
        }
        if (control.kind === "pot" || control.kind === "rotary") {
          applyControlAt(id, point);
        }
        try {
          ui.canvas.setPointerCapture?.(event.pointerId);
        } catch {
          // Pointer capture is an optimisation; the control still works without it.
        }
        reflectSimState();
        renderSimStatus();
        event.preventDefault();
        return;
      }
    }

    selectPart(id);
    if (!part) return;
    state.drag = { id, start: point, left: part.left, top: part.top, moved: false };
    try {
      ui.canvas.setPointerCapture?.(event.pointerId);
    } catch {
      // Pointer capture is an optimisation; the drag still works without it.
    }
    event.preventDefault();
    return;
  }
}

function onPointerMove(event) {
  const point = toCanvasPoint(event);
  state.pointer = point;

  if (state.heldControl) {
    const { id, kind } = state.heldControl;
    if (kind === "pot" || kind === "rotary") {
      applyControlAt(id, point);
      event.preventDefault();
    }
    return;
  }

  if (state.drag) {
    const part = findPart(state.drag.id);
    if (!part) return;
    const dx = point.x - state.drag.start.x;
    const dy = point.y - state.drag.start.y;
    if (!state.drag.moved && Math.hypot(dx, dy) < 3) return;
    state.drag.moved = true;
    const left = snap(state.drag.left + dx);
    const top = snap(state.drag.top + dy);
    if (part.left !== left || part.top !== top) {
      state.project.circuit = withPartMoved(state.project.circuit, part.id, left, top);
      markDirty();
      renderCanvas();
    }
    return;
  }

  if (state.pendingPin) {
    ui.previewLayer.replaceChildren();
    drawPreview();
  }
}

function onPointerUp(event) {
  if (state.heldControl) {
    releaseHeldControl();
    try {
      ui.canvas.releasePointerCapture?.(event.pointerId);
    } catch {
      // The browser may already have released a cancelled pointer.
    }
    return;
  }
  if (!state.drag) return;
  if (state.drag.moved) {
    state.suppressClick = true;
    setTimeout(() => {
      state.suppressClick = false;
    }, 0);
    state.statusText = "Unsaved changes";
    renderStatus();
    refreshValidation();
    renderProjectPanel();
    renderCanvas();
  }
  state.drag = null;
  try {
    ui.canvas.releasePointerCapture?.(event.pointerId);
  } catch {
    // The browser may already have released a cancelled pointer.
  }
}

function onClick(event) {
  if (state.suppressClick) {
    state.suppressClick = false;
    return;
  }
  const target = event.target;

  const pinEl = target.closest?.("[data-pin]");
  if (pinEl) {
    handlePinClick(pinEl.dataset.partId, pinEl.dataset.pinName, pinEl.dataset.pinKind);
    return;
  }

  const partEl = target.closest?.("[data-part-id]");
  if (partEl) {
    selectPart(partEl.dataset.partId);
    return;
  }

  const wireEl = target.closest?.("[data-wire-index]");
  if (wireEl) {
    selectWire(Number(wireEl.dataset.wireIndex));
    return;
  }

  state.selected = null;
  state.pendingPin = null;
  renderCanvas();
  renderInspector();
  renderStatus();
}

function onKeyDown(event) {
  const tag = event.target?.tagName?.toLowerCase();
  const editing =
    tag === "input" ||
    tag === "textarea" ||
    tag === "select" ||
    event.target?.isContentEditable;
  if (editing) return;

  if (event.key === "Escape") {
    if (state.pendingPin) {
      cancelPending();
    } else if (state.selected) {
      state.selected = null;
      renderCanvas();
      renderInspector();
      renderStatus();
    }
    return;
  }

  if (event.key === "Delete" || event.key === "Backspace") {
    if (!state.selected) return;
    event.preventDefault();
    deleteSelection();
  }
}

function bindEvents() {
  ui.catalogSearch.addEventListener("input", renderCatalog);

  ui.canvas.addEventListener("pointerdown", onPointerDown);
  ui.canvas.addEventListener("pointermove", onPointerMove);
  ui.canvas.addEventListener("pointerup", onPointerUp);
  ui.canvas.addEventListener("pointercancel", onPointerUp);
  // Pointer capture normally keeps the events on the canvas; these also end
  // a drag if capture was unavailable.
  window.addEventListener("pointerup", onPointerUp);
  window.addEventListener("pointercancel", onPointerUp);
  ui.canvas.addEventListener("click", onClick);
  ui.canvas.addEventListener("contextmenu", (event) => {
    event.preventDefault();
    cancelPending();
  });

  ui.tabSerial.addEventListener("click", () => switchTab("serial"));
  ui.tabDiagnostics.addEventListener("click", () => switchTab("diagnostics"));
  $("clear-output").addEventListener("click", () => {
    if (state.tab === "serial") {
      ui.serial.textContent = "";
      state.sim?.clearSerial();
    } else {
      state.log = [];
      renderDiagnostics();
    }
  });

  $("new-project").addEventListener("click", newProject);
  $("open-project").addEventListener("click", openProject);
  $("save-project").addEventListener("click", saveProject);
  $("import-diagram").addEventListener("click", importDiagram);
  $("export-diagram").addEventListener("click", exportDiagram);

  ui.customButton.addEventListener("click", openCustomParts);
  ui.customSave.addEventListener("click", saveCustomPartsFromEditor);
  ui.customImport.addEventListener("click", importCustomParts);
  ui.customExport.addEventListener("click", exportCustomParts);

  ui.simRun.addEventListener("click", () => {
    if (state.sim?.hasSession() && !state.sim.fatal) {
      state.sim.run();
      reflectSimState();
      renderSimStatus();
      return;
    }
    loadAndRun();
  });
  ui.simPause.addEventListener("click", () => {
    state.sim?.pause();
    reflectSimState();
    renderSimStatus();
  });
  ui.simReset.addEventListener("click", resetSimulator);
  ui.simLoad.addEventListener("click", loadAndRun);
  ui.simSpeed.addEventListener("change", () => {
    state.sim?.setSpeed(Number(ui.simSpeed.value));
    renderSimStatus();
  });
  ui.firmwareKind.addEventListener("change", () => {
    state.firmwareKind = ui.firmwareKind.value;
    updateFirmwareHint();
    renderSimStatus();
  });
  ui.firmwareApp.addEventListener("change", () => onFirmwareChange("app", ui.firmwareApp));
  ui.firmwareRom.addEventListener("change", () => onFirmwareChange("rom", ui.firmwareRom));
  ui.firmwareBoot.addEventListener("change", () => onFirmwareChange("boot", ui.firmwareBoot));
  ui.firmwarePtable.addEventListener("change", () =>
    onFirmwareChange("ptable", ui.firmwarePtable),
  );

  ui.serialForm.addEventListener("submit", (event) => {
    event.preventDefault();
    const text = ui.serialInput.value;
    if (!state.sim?.hasSession()) {
      log("warning", "Load firmware before typing to the serial monitor.");
      return;
    }
    if (!text) return;
    if (!state.sim.writeSerial(`${text}\n`)) {
      log("error", state.sim.error || "The serial write was refused.");
      return;
    }
    ui.serialInput.value = "";
  });

  ui.scopeForm.addEventListener("submit", (event) => {
    event.preventDefault();
    addScopeChannel();
  });
  ui.decodeForm.addEventListener("submit", (event) => {
    event.preventDefault();
    addScopeDecode();
  });
  ui.scopeWindow.addEventListener("change", () => {
    drawScopeTrace();
    updateScopeReadout();
  });
  ui.scopeCanvas.addEventListener("pointermove", onScopePointerMove);
  ui.scopeCanvas.addEventListener("pointerleave", onScopePointerLeave);
  window.addEventListener("resize", drawScopeTrace);

  ui.projectName.addEventListener("input", () => {
    if (ui.projectName.value !== state.project.name) {
      state.project.name = ui.projectName.value;
      markDirty();
    }
  });
  ui.projectName.addEventListener("change", () => {
    if (!ui.projectName.value.trim()) {
      ui.projectName.value = "Untitled";
      state.project.name = "Untitled";
      markDirty();
    }
  });

  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("beforeunload", (event) => {
    if (!state.dirty) return;
    event.preventDefault();
    event.returnValue = "";
  });
}

function renderAll() {
  refreshValidation();
  renderCanvas();
  renderInspector();
  renderStatus();
  updateFirmwareHint();
  renderSimStatus();
  renderScopeChannels();
  drawScopeTrace();
}

function init() {
  bindEvents();
  syncProjectNameInput();
  state.customParts = loadCustomParts(browserStorage());
  state.sim = createRunController({
    onFrame: () => {
      consumeSerial();
      reflectSimState();
      renderSimStatus();
      pollScope();
    },
    onError: (message) => log("error", `Simulator: ${message}`),
  });
  renderAll();
  renderCatalog();
  loadCatalog();
  ensureSimulator().then((sim) => {
    if (!sim) {
      log("warning", state.simInitError);
    }
    renderSimStatus();
  });
}

init();
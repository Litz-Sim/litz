import assert from "node:assert/strict";
import test from "node:test";

import { indexCatalog } from "../src/catalog.js";
import {
  connectionExists,
  createProject,
  diagramFromProject,
  formatPinRef,
  nameFromFile,
  parseDocument,
  parsePinRef,
  parseProject,
  projectDocument,
  projectFromDiagram,
  serializeProject,
  slugForType,
  uniquePartId,
  validateProject,
  withConnectionAdded,
  withPartAdded,
  withPartRenamed,
} from "../src/project.js";

const CATALOG = indexCatalog([
  {
    id: "board-test",
    kind: "board",
    name: "Test Board",
    pins: [
      { name: "GPIO1", kind: "digital-io" },
      { name: "GND", kind: "ground" },
      { name: "5V", kind: "power" },
    ],
    attrs: [],
  },
  {
    id: "litz-led",
    kind: "part",
    name: "LED",
    pins: [
      { name: "A", kind: "digital-in" },
      { name: "C", kind: "ground" },
    ],
    attrs: [{ name: "color", type: "enum", values: ["red", "green"], default: "red" }],
  },
]);

function blinkCircuit() {
  let circuit = createProject().circuit;
  circuit = withPartAdded(circuit, {
    id: "uno",
    type: "board-test",
    attrs: {},
    left: 80,
    top: 80,
    rotate: 0,
  });
  circuit = withPartAdded(circuit, {
    id: "led1",
    type: "litz-led",
    attrs: { color: "green" },
    left: 360,
    top: 120,
    rotate: 90,
  });
  circuit = withConnectionAdded(circuit, ["uno:GPIO1", "led1:A", "green", []]);
  circuit = withConnectionAdded(circuit, ["led1:C", "uno:GND", "black", ["v1", "v2"]]);
  return circuit;
}

test("project: pin references parse and format like the Rust parser", () => {
  assert.deepEqual(parsePinRef("esp:GPIO8"), {
    part: "esp",
    name: "GPIO8",
    index: 0,
    pseudo: false,
  });
  assert.equal(parsePinRef("esp:GND.2").index, 2);
  assert.equal(parsePinRef("btn1:1.l").name, "1.l");
  assert.equal(parsePinRef("btn1:1.l").index, 0);
  assert.equal(parsePinRef("$gnd").pseudo, true);
  assert.equal(parsePinRef("$serialMonitor:RX").name, "RX");
  assert.equal(parsePinRef("esp"), null);
  assert.equal(parsePinRef(":GPIO8"), null);
  assert.equal(parsePinRef("esp:"), null);

  assert.equal(formatPinRef(parsePinRef("esp:GND.2")), "esp:GND.2");
  assert.equal(formatPinRef(parsePinRef("btn1:1.l")), "btn1:1.l");
  assert.equal(formatPinRef(parsePinRef("$gnd")), "$gnd");
});

test("project: a .litz document round-trips through save and load", () => {
  const project = { ...createProject("Blink"), circuit: blinkCircuit() };
  const text = serializeProject(project);
  assert.match(text, /"format": "litz-project"/);
  assert.match(text, /"version": 1/);

  const again = parseProject(text);
  assert.equal(again.name, "Blink");
  assert.deepEqual(projectDocument(again), projectDocument(project));
  assert.deepEqual(again.circuit.parts[1].attrs, { color: "green" });
  assert.equal(again.circuit.parts[1].rotate, 90);
  assert.deepEqual(again.circuit.connections[1], ["led1:C", "uno:GND", "black", ["v1", "v2"]]);
});

test("project: diagram.json export uses four-element connections", () => {
  const project = { ...createProject("Blink"), circuit: blinkCircuit() };
  const diagram = diagramFromProject(project);
  assert.equal(diagram.version, 1);
  assert.equal(diagram.parts.length, 2);
  assert.deepEqual(diagram.connections[0], ["uno:GPIO1", "led1:A", "green", []]);
  assert.equal(diagram.parts[0].type, "board-test");
});

test("project: a diagram imports into a project and is detected on open", () => {
  const diagram = {
    version: 1,
    parts: [
      { id: "uno", type: "board-test", attrs: {}, left: 20, top: 40 },
      { id: "led1", type: "litz-led", attrs: { color: "red" } },
    ],
    connections: [["uno:GPIO1", "led1:A", "green", ["v1"]]],
    files: ["firmware.bin"],
  };
  const { project, warnings } = projectFromDiagram(diagram, "blinky");
  assert.equal(project.name, "blinky");
  assert.equal(project.files.length, 0, "diagram file entries are not trusted or loaded");
  assert.ok(warnings.some((warning) => warning.includes("not loaded")));

  const opened = parseDocument(JSON.stringify(diagram), "examples/blinky.json");
  assert.equal(opened.kind, "diagram");
  assert.equal(opened.project.name, "blinky");

  assert.equal(nameFromFile("examples/diagram.json"), "Imported circuit");
  const asProject = parseDocument(
    serializeProject({ ...createProject("Named"), circuit: blinkCircuit() }),
    "named.litz",
  );
  assert.equal(asProject.kind, "project");
  assert.equal(asProject.project.name, "Named");
});

test("project: invalid documents are refused with a message", () => {
  assert.throws(() => parseProject("{"), /not valid JSON/);
  assert.throws(
    () => parseProject(JSON.stringify({ format: "litz-project", version: 2, circuit: {} })),
    /version/,
  );
  assert.throws(
    () => parseDocument(JSON.stringify({ hello: "world" }), "x.json"),
    /neither a Litz project nor a diagram/,
  );
});

test("project: validation accepts a catalog-backed circuit", () => {
  const project = { ...createProject("Blink"), circuit: blinkCircuit() };
  const result = validateProject(project, CATALOG);
  assert.deepEqual(result.errors, []);
  assert.deepEqual(result.warnings, []);
});

test("project: validation reports duplicate ids, unknown pins and type mismatches", () => {
  let circuit = blinkCircuit();
  circuit = withPartAdded(circuit, {
    id: "led1",
    type: "litz-led",
    attrs: {},
    left: 0,
    top: 0,
    rotate: 0,
  });
  circuit = withConnectionAdded(circuit, ["led1:B", "uno:GND", "green", []]);
  circuit = withPartAdded(circuit, {
    id: "mystery",
    type: "not-in-catalog",
    attrs: {},
    left: 0,
    top: 0,
    rotate: 0,
  });
  const project = { ...createProject("Bad"), circuit };
  project.circuit.parts[1].attrs = { color: "blue" };

  const codes = validateProject(project, CATALOG).errors.map((issue) => issue.code);
  assert.ok(codes.includes("duplicate-id"), codes.join(", "));
  assert.ok(codes.includes("unknown-pin"), codes.join(", "));
  assert.ok(codes.includes("unknown-part"), codes.join(", "));
  assert.ok(codes.includes("attr-type"), codes.join(", "));
});

test("project: validation refuses a rail-to-rail short", () => {
  let circuit = blinkCircuit();
  circuit = withConnectionAdded(circuit, ["uno:GND", "uno:5V", "black", []]);
  const codes = validateProject({ ...createProject("Short"), circuit }, CATALOG).errors.map(
    (issue) => issue.code,
  );
  assert.ok(codes.includes("pin-kind"), codes.join(", "));

  let rails = blinkCircuit();
  rails = withConnectionAdded(rails, ["$gnd", "$v33", "black", []]);
  const railCodes = validateProject({ ...createProject("Rails"), circuit: rails }, CATALOG).errors.map(
    (issue) => issue.code,
  );
  assert.ok(railCodes.includes("rail-short"), railCodes.join(", "));
});

test("project: renaming a part rewrites every connection that names it", () => {
  let circuit = blinkCircuit();
  circuit.connections[0][0] = "uno:GPIO1.1";
  const renamed = withPartRenamed(circuit, "led1", "d1");
  assert.equal(renamed.parts[1].id, "d1");
  assert.equal(renamed.connections[0][1], "d1:A");
  assert.equal(renamed.connections[1][0], "d1:C");
  assert.equal(renamed.connections[0][0], "uno:GPIO1.1");
});

test("project: ids and duplicate wires are generated predictably", () => {
  assert.equal(slugForType("litz-led"), "led");
  assert.equal(slugForType("board-esp32-c3-devkitm-1"), "esp32-c3-devkitm-1");
  assert.equal(slugForType("litz-24c02"), "24c02");

  const circuit = blinkCircuit();
  assert.equal(uniquePartId(circuit, "led"), "led", "led1 does not occupy the bare id");
  assert.equal(uniquePartId(circuit, "led1"), "led12", "a suffix is added to the id as written");
  assert.equal(uniquePartId(circuit, "fresh"), "fresh");

  const withLed = withPartAdded(circuit, {
    id: "led",
    type: "litz-led",
    attrs: {},
    left: 0,
    top: 0,
    rotate: 0,
  });
  assert.equal(uniquePartId(withLed, "led"), "led2");
  assert.equal(connectionExists(circuit, "led1:A", "uno:GPIO1"), true);
  assert.equal(connectionExists(circuit, "led1:C", "uno:GPIO1"), false);
});
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  defaultAttrs,
  findPin,
  groupedCatalog,
  indexCatalog,
  pinIndex,
  pinKindAccepts,
  searchCatalog,
} from "../src/catalog.js";

const ENTRIES = [
  {
    id: "board-test",
    kind: "board",
    name: "Test Board",
    description: "A board with a GPIO",
    pins: [
      { name: "GPIO1", kind: "digital-io" },
      { name: "GND", kind: "ground" },
    ],
    attrs: [],
  },
  {
    id: "litz-led",
    kind: "part",
    name: "LED",
    description: "5 mm through-hole LED",
    pins: [
      { name: "A", kind: "digital-in" },
      { name: "C", kind: "ground" },
    ],
    attrs: [
      { name: "color", type: "enum", values: ["red", "green"], default: "red" },
      { name: "label", type: "string", values: [], default: "" },
    ],
  },
  {
    id: "litz-resistor",
    kind: "part",
    name: "Resistor",
    description: "Through-hole resistor",
    pins: [
      { name: "1", kind: "passive" },
      { name: "2", kind: "passive" },
    ],
    attrs: [{ name: "value", type: "string", values: [], default: "220" }],
  },
];

test("catalog: entries index by id and split boards from parts", () => {
  const index = indexCatalog(ENTRIES);
  assert.equal(index.entries.length, 3);
  assert.deepEqual(
    index.boards.map((entry) => entry.id),
    ["board-test"],
  );
  assert.deepEqual(
    index.parts.map((entry) => entry.id),
    ["litz-led", "litz-resistor"],
  );
  assert.equal(index.byId.get("litz-led").name, "LED");
  assert.equal(index.byId.get("missing"), undefined);
});

test("catalog: search matches names, ids and descriptions", () => {
  const index = indexCatalog(ENTRIES);
  assert.deepEqual(
    searchCatalog(index, "led").parts.map((entry) => entry.id),
    ["litz-led"],
  );
  assert.deepEqual(
    searchCatalog(index, "resistor").parts.map((entry) => entry.id),
    ["litz-resistor"],
  );
  assert.deepEqual(
    searchCatalog(index, "GPIO").boards.map((entry) => entry.id),
    ["board-test"],
  );
  assert.equal(searchCatalog(index, "nothing").boards.length, 0);
  assert.equal(searchCatalog(index, "  ").parts.length, 2, "an empty query keeps everything");
});

test("catalog: grouping drops empty categories", () => {
  const index = indexCatalog(ENTRIES);
  const all = groupedCatalog(searchCatalog(index, ""));
  assert.deepEqual(
    all.map((group) => group.label),
    ["Boards", "Parts"],
  );
  const onlyParts = groupedCatalog(searchCatalog(index, "resistor"));
  assert.deepEqual(
    onlyParts.map((group) => group.label),
    ["Parts"],
  );
});

test("catalog: pin lookup and kinds follow the Rust catalog rule", () => {
  const led = indexCatalog(ENTRIES).byId.get("litz-led");
  assert.equal(findPin(led, "A").kind, "digital-in");
  assert.equal(findPin(led, "missing"), null);
  assert.equal(pinIndex(led).get("C").kind, "ground");

  assert.equal(pinKindAccepts("digital-in", "ground"), true);
  assert.equal(pinKindAccepts("passive", "power"), true);
  assert.equal(pinKindAccepts("power", "ground"), false);
  assert.equal(pinKindAccepts("ground", "power"), false);
  assert.equal(pinKindAccepts("nc", "digital-io"), false);
  assert.equal(pinKindAccepts("digital-io", "nc"), false);
});

test("catalog: default attributes come from the manifest defaults", () => {
  const index = indexCatalog(ENTRIES);
  assert.deepEqual(defaultAttrs(index.byId.get("litz-led")), { color: "red", label: "" });
  assert.deepEqual(defaultAttrs(index.byId.get("board-test")), {});
});

test("catalog: the generated catalog.json indexes", () => {
  const text = readFileSync(new URL("../catalog.json", import.meta.url), "utf8");
  const entries = JSON.parse(text);
  const index = indexCatalog(entries);
  assert.ok(index.entries.length > 50, `catalog has ${index.entries.length} entries`);
  assert.ok(index.boards.length >= 5);
  assert.ok(index.parts.length >= 50);
  assert.equal(index.byId.get("litz-led").name, "LED");
  assert.equal(index.byId.get("board-esp32-c3-devkitm-1").kind, "board");
  assert.ok(index.byId.has("board-arduino-uno"));
});
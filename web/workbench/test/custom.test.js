import assert from "node:assert/strict";
import test from "node:test";

import { indexCatalog, searchCatalog } from "../src/catalog.js";
import {
  CUSTOM_STORAGE_KEY,
  loadCustomParts,
  mergeCatalog,
  normalizeCustomParts,
  parseCustomParts,
  saveCustomParts,
  serializeCustomParts,
  validateCustomPart,
} from "../src/custom.js";

const BASE = indexCatalog([
  {
    id: "board-test",
    kind: "board",
    name: "Test Board",
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
    pins: [
      { name: "A", kind: "digital-in" },
      { name: "C", kind: "ground" },
    ],
    attrs: [],
  },
]);

const GOOD = {
  id: "acme-sensor",
  name: "ACME sensor",
  description: "An I2C temperature sensor",
  pins: [
    { name: "VCC", kind: "power" },
    { name: "GND", kind: "ground" },
    { name: "SDA", kind: "i2c-sda" },
    { name: "SCL", kind: "i2c-scl" },
  ],
  attrs: [
    { name: "address", type: "int", default: 72 },
    { name: "mode", type: "enum", values: ["fast", "slow"], default: "fast" },
  ],
  width: 140,
  height: 80,
};

function fakeStorage(initial = {}) {
  const map = new Map(Object.entries(initial));
  return {
    getItem(key) {
      return map.has(key) ? map.get(key) : null;
    },
    setItem(key, value) {
      map.set(String(key), String(value));
    },
    removeItem(key) {
      map.delete(key);
    },
  };
}

test("custom: a good definition validates and normalises to a catalog entry", () => {
  const result = validateCustomPart(GOOD, { catalog: BASE });
  assert.equal(result.ok, true);
  assert.deepEqual(result.errors, []);
  assert.equal(result.entry.id, "acme-sensor");
  assert.equal(result.entry.kind, "part");
  assert.equal(result.entry.custom, true);
  assert.equal(result.entry.name, "ACME sensor");
  assert.deepEqual(result.entry.pins, GOOD.pins);
  assert.deepEqual(result.entry.attrs[0], {
    name: "address",
    type: "int",
    values: [],
    default: 72,
  });
  assert.deepEqual(result.entry.attrs[1], {
    name: "mode",
    type: "enum",
    values: ["fast", "slow"],
    default: "fast",
  });
  assert.equal(result.entry.width, 140);
  assert.equal(result.entry.height, 80);
});

test("custom: duplicate ids and built-in collisions are rejected", () => {
  const collision = validateCustomPart({ ...GOOD, id: "litz-led" }, { catalog: BASE });
  assert.equal(collision.ok, false);
  assert.deepEqual(
    collision.errors.map((error) => error.code),
    ["id-collision"],
  );

  // Replacing an existing custom id is allowed.
  const replacement = validateCustomPart(
    { ...GOOD, name: "Renamed sensor" },
    { catalog: BASE, customs: ["acme-sensor"] },
  );
  assert.equal(replacement.ok, true);
  assert.equal(replacement.entry.name, "Renamed sensor");

  // Two definitions with the same id in one import are refused.
  const batch = normalizeCustomParts([GOOD, { ...GOOD, name: "Again" }], { catalog: BASE });
  assert.equal(batch.entries.length, 1);
  assert.deepEqual(
    batch.errors.map((error) => error.code),
    ["duplicate-id"],
  );

  const builtinBatch = normalizeCustomParts([{ ...GOOD, id: "litz-led" }], {
    catalog: BASE,
    existing: ["acme-sensor"],
  });
  assert.equal(builtinBatch.entries.length, 0);
  assert.equal(builtinBatch.errors[0].code, "id-collision");
});

test("custom: empty pins and unknown pin kinds are rejected", () => {
  const empty = validateCustomPart({ ...GOOD, pins: [] }, { catalog: BASE });
  assert.equal(empty.ok, false);
  assert.ok(empty.errors.some((error) => error.code === "pins-empty"));
  assert.equal(empty.entry, null);

  const badKind = validateCustomPart(
    { ...GOOD, pins: [{ name: "A", kind: "magic" }] },
    { catalog: BASE },
  );
  assert.equal(badKind.ok, false);
  assert.ok(badKind.errors.some((error) => error.code === "pin-kind"));

  const duplicatePin = validateCustomPart(
    {
      ...GOOD,
      pins: [
        { name: "A", kind: "passive" },
        { name: "A", kind: "ground" },
      ],
    },
    { catalog: BASE },
  );
  assert.equal(duplicatePin.ok, false);
  assert.ok(duplicatePin.errors.some((error) => error.code === "pin-duplicate"));

  const colonPin = validateCustomPart(
    { ...GOOD, pins: [{ name: "A:B", kind: "passive" }] },
    { catalog: BASE },
  );
  assert.equal(colonPin.ok, false);
  assert.ok(colonPin.errors.some((error) => error.code === "pin-name"));

  const reservedAttr = validateCustomPart(
    { ...GOOD, attrs: [{ name: "__proto__", type: "string" }] },
    { catalog: BASE },
  );
  assert.equal(reservedAttr.ok, false);
  assert.ok(reservedAttr.errors.some((error) => error.code === "attr-name"));
});

test("custom: merging adds the custom part and keeps every built-in entry", () => {
  const { entry } = validateCustomPart(GOOD, { catalog: BASE });
  const merged = mergeCatalog(BASE, [entry]);

  assert.equal(merged.byId.get("acme-sensor").name, "ACME sensor");
  assert.equal(merged.byId.get("acme-sensor").custom, true);
  assert.ok(merged.byId.has("litz-led"));
  assert.ok(merged.byId.has("board-test"));
  assert.equal(merged.byId.get("litz-led").custom, undefined);
  assert.equal(merged.entries.length, BASE.entries.length + 1);
  assert.deepEqual(
    merged.boards.map((entry) => entry.id),
    ["board-test"],
  );
  assert.deepEqual(
    searchCatalog(merged, "acme").parts.map((entry) => entry.id),
    ["acme-sensor"],
  );
  assert.equal(mergeCatalog(BASE, []), BASE, "an empty merge returns the same index");
});

test("custom: definitions round-trip through localStorage", () => {
  const storage = fakeStorage();
  const { entries } = normalizeCustomParts([GOOD], { catalog: BASE });
  assert.equal(saveCustomParts(storage, entries), true);
  assert.match(storage.getItem(CUSTOM_STORAGE_KEY), /"litz-custom-parts"/);

  const loaded = loadCustomParts(storage);
  assert.equal(loaded.length, 1);
  const again = normalizeCustomParts(loaded, { catalog: BASE });
  assert.deepEqual(again.errors, []);
  assert.deepEqual(again.entries, entries);
  assert.equal(again.entries[0].width, 140);

  assert.deepEqual(loadCustomParts(fakeStorage()), [], "missing storage reads empty");
  const broken = fakeStorage({ [CUSTOM_STORAGE_KEY]: "{not json" });
  assert.deepEqual(loadCustomParts(broken), [], "corrupt storage reads empty");
});

test("custom: import accepts one definition, an array or an exported file", () => {
  assert.deepEqual(parseCustomParts(JSON.stringify(GOOD)), [GOOD]);
  assert.deepEqual(parseCustomParts(JSON.stringify([GOOD])), [GOOD]);

  const exported = serializeCustomParts(normalizeCustomParts([GOOD], { catalog: BASE }).entries);
  const parsed = parseCustomParts(exported);
  assert.equal(parsed.length, 1);
  assert.equal(parsed[0].id, "acme-sensor");
  assert.deepEqual(parsed[0].pins, GOOD.pins);

  assert.throws(() => parseCustomParts("{"), /not valid JSON/);
  assert.throws(() => parseCustomParts("42"), /expected a custom part/);
});

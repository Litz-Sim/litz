// User-defined parts that live in this browser, not in `catalog.json`.
//
// A definition is JSON in this shape:
//
//   {
//     "id": "acme-temp",              // unique; becomes the part's `type`
//     "name": "ACME temperature",     // shown in the picker and inspector
//     "description": "I2C sensor",    // optional, shown under the name
//     "pins": [                       // at least one; kind is from PIN_KINDS
//       { "name": "VCC", "kind": "power" },
//       { "name": "GND", "kind": "ground" },
//       { "name": "SDA", "kind": "i2c-sda" }
//     ],
//     "attrs": [                      // optional
//       { "name": "address", "type": "int", "default": 72 },
//       { "name": "mode", "type": "enum", "values": ["fast", "slow"], "default": "fast" }
//     ],
//     "width": 132,                   // optional box size in canvas pixels
//     "height": 80
//   }
//
// Every definition is validated before use and normalised into the same entry
// shape `indexCatalog` produces, with `custom: true`. Definitions are stored
// under CUSTOM_STORAGE_KEY as
//   {"format":"litz-custom-parts","version":1,"parts":[...]}
// and merged into the catalog index the picker and canvas share.

import { indexCatalog } from "./catalog.js";

/** Pin kinds the built-in catalog uses; a custom part may not invent one. */
export const PIN_KINDS = Object.freeze([
  "analog-in",
  "analog-io",
  "analog-out",
  "digital-in",
  "digital-io",
  "digital-out",
  "ground",
  "i2c-scl",
  "i2c-sda",
  "nc",
  "passive",
  "power",
  "spi-cs",
  "spi-miso",
  "spi-mosi",
  "spi-sck",
  "uart-rx",
  "uart-tx",
]);

/** Attribute types the inspector knows how to edit. */
export const ATTR_TYPES = Object.freeze(["bool", "enum", "float", "int", "string"]);

export const CUSTOM_PARTS_FORMAT = "litz-custom-parts";
export const CUSTOM_PARTS_VERSION = 1;
export const CUSTOM_STORAGE_KEY = "litz.workbench.customParts";

const CUSTOM_ID = /^[A-Za-z0-9][A-Za-z0-9_-]*$/;
const PIN_INDEX_SUFFIX = /\.\d+$/;
const FORBIDDEN_ATTR_NAMES = new Set(["__proto__", "constructor", "prototype"]);
const PIN_KIND_SET = new Set(PIN_KINDS);
const ATTR_TYPE_SET = new Set(ATTR_TYPES);

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function customIds(customs) {
  const ids = new Set();
  for (const custom of customs ?? []) {
    if (typeof custom === "string") ids.add(custom);
    else if (custom && typeof custom.id === "string") ids.add(custom.id);
  }
  return ids;
}

function builtinHas(catalog, id) {
  if (!catalog) return false;
  if (catalog.byId instanceof Map) return catalog.byId.has(id);
  if (Array.isArray(catalog)) return catalog.some((entry) => entry?.id === id);
  return false;
}

function defaultStorage() {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

function attrDefaultMatches(def, value) {
  switch (def.type) {
    case "bool":
      return typeof value === "boolean";
    case "int":
      return Number.isInteger(value);
    case "float":
      return typeof value === "number" && Number.isFinite(value);
    case "string":
      return typeof value === "string";
    case "enum":
      return typeof value === "string" && def.values.includes(value);
    default:
      return false;
  }
}

/**
 * Validate one definition. `catalog` is the built-in index (or its entry
 * array); `customs` names the custom ids already stored, so the same id may
 * be replaced but a built-in id may not be shadowed.
 *
 * Returns `{ok, entry, errors}`. `entry` is null when anything failed.
 */
export function validateCustomPart(raw, { catalog = null, customs = [] } = {}) {
  const errors = [];
  const fail = (code, message) => errors.push({ code, message });

  if (!isObject(raw)) {
    fail("definition-shape", "A custom part definition must be an object.");
    return { ok: false, entry: null, errors };
  }

  const id = raw.id;
  if (typeof id !== "string" || id.length === 0) {
    fail("id", "The definition needs a non-empty id.");
  } else if (!CUSTOM_ID.test(id)) {
    fail("id-format", `Id ${JSON.stringify(id)} may only use letters, digits, "_" and "-".`);
  } else if (builtinHas(catalog, id) && !customIds(customs).has(id)) {
    fail("id-collision", `Id ${JSON.stringify(id)} is already a built-in part.`);
  }

  const name = raw.name;
  if (typeof name !== "string" || name.trim() === "") {
    fail("name", "The definition needs a non-empty name.");
  }

  const description = raw.description ?? "";
  if (typeof description !== "string") {
    fail("description", "description must be a string.");
  }

  const pins = [];
  if (!Array.isArray(raw.pins) || raw.pins.length === 0) {
    fail("pins-empty", "A custom part needs at least one pin.");
  } else {
    const seen = new Set();
    raw.pins.forEach((pin, index) => {
      if (!isObject(pin)) {
        fail("pin-shape", `Pin ${index + 1} is not an object.`);
        return;
      }
      const pinName = pin.name;
      if (typeof pinName !== "string" || pinName.trim() === "") {
        fail("pin-name", `Pin ${index + 1} has no name.`);
        return;
      }
      if (pinName.includes(":")) {
        fail("pin-name", `Pin ${JSON.stringify(pinName)} may not contain ":".`);
        return;
      }
      if (PIN_INDEX_SUFFIX.test(pinName)) {
        fail(
          "pin-name",
          `Pin ${JSON.stringify(pinName)} would be read as an index; rename it.`,
        );
        return;
      }
      if (seen.has(pinName)) {
        fail("pin-duplicate", `Pin ${JSON.stringify(pinName)} is listed more than once.`);
        return;
      }
      seen.add(pinName);
      if (typeof pin.kind !== "string" || !PIN_KIND_SET.has(pin.kind)) {
        fail(
          "pin-kind",
          `Pin ${JSON.stringify(pinName)} has kind ${JSON.stringify(pin.kind)}; expected one of ${PIN_KINDS.join(", ")}.`,
        );
        return;
      }
      pins.push({ name: pinName, kind: pin.kind });
    });
  }

  const attrs = [];
  if (raw.attrs !== undefined) {
    if (!Array.isArray(raw.attrs)) {
      fail("attrs-shape", "attrs must be an array.");
    } else {
      const seen = new Set();
      raw.attrs.forEach((attr, index) => {
        if (!isObject(attr)) {
          fail("attr-shape", `Attribute ${index + 1} is not an object.`);
          return;
        }
        const attrName = attr.name;
        if (typeof attrName !== "string" || attrName.trim() === "") {
          fail("attr-name", `Attribute ${index + 1} has no name.`);
          return;
        }
        if (FORBIDDEN_ATTR_NAMES.has(attrName)) {
          fail("attr-name", `Attribute name ${JSON.stringify(attrName)} is reserved.`);
          return;
        }
        if (seen.has(attrName)) {
          fail("attr-duplicate", `Attribute ${JSON.stringify(attrName)} is listed more than once.`);
          return;
        }
        seen.add(attrName);
        if (typeof attr.type !== "string" || !ATTR_TYPE_SET.has(attr.type)) {
          fail(
            "attr-type",
            `Attribute ${JSON.stringify(attrName)} has type ${JSON.stringify(attr.type)}; expected one of ${ATTR_TYPES.join(", ")}.`,
          );
          return;
        }
        let values = [];
        if (attr.values !== undefined) {
          if (!Array.isArray(attr.values) || attr.values.some((value) => typeof value !== "string")) {
            fail("attr-values", `Attribute ${JSON.stringify(attrName)} values must be an array of strings.`);
            return;
          }
          values = [...attr.values];
        }
        if (attr.type === "enum" && values.length === 0) {
          fail("attr-values", `Enum attribute ${JSON.stringify(attrName)} needs at least one value.`);
          return;
        }
        const def = {
          name: attrName,
          type: attr.type,
          values,
          default: attr.default ?? null,
        };
        if (def.default !== null && !attrDefaultMatches(def, def.default)) {
          fail("attr-default", `Attribute ${JSON.stringify(attrName)} default does not match ${def.type}.`);
          return;
        }
        attrs.push(def);
      });
    }
  }

  const size = {};
  for (const key of ["width", "height"]) {
    if (raw[key] === undefined) continue;
    if (typeof raw[key] !== "number" || !Number.isFinite(raw[key]) || raw[key] <= 0) {
      fail("size", `${key} must be a positive number.`);
    } else {
      size[key] = raw[key];
    }
  }

  if (errors.length > 0) return { ok: false, entry: null, errors };

  return {
    ok: true,
    errors,
    entry: {
      id,
      kind: "part",
      name,
      description,
      pins,
      attrs,
      custom: true,
      ...size,
    },
  };
}

/**
 * Validate a list of definitions (a pasted array or an imported file).
 * Duplicate ids inside the list are rejected; ids matching `existing` custom
 * parts are replacements and allowed.
 *
 * Returns `{entries, errors}`, where each error also carries `index` and `id`.
 */
export function normalizeCustomParts(list, { catalog = null, existing = [] } = {}) {
  if (!Array.isArray(list)) {
    return {
      entries: [],
      errors: [{ code: "list-shape", message: "Custom parts must be an array.", index: -1, id: null }],
    };
  }
  const entries = [];
  const errors = [];
  // Only ids already stored may shadow a built-in id; ids inside this very
  // list do not, which is how a built-in collision is still caught.
  const allowed = customIds(existing);
  const seen = new Set();
  list.forEach((raw, index) => {
    const result = validateCustomPart(raw, { catalog, customs: [...allowed] });
    if (!result.ok) {
      for (const error of result.errors) {
        errors.push({ ...error, index, id: isObject(raw) && typeof raw.id === "string" ? raw.id : null });
      }
      return;
    }
    if (seen.has(result.entry.id)) {
      errors.push({
        code: "duplicate-id",
        message: `Custom part id ${JSON.stringify(result.entry.id)} is defined more than once.`,
        index,
        id: result.entry.id,
      });
      return;
    }
    seen.add(result.entry.id);
    entries.push(result.entry);
  });
  return { entries, errors };
}

/**
 * Parse pasted/imported JSON. Accepts one definition, an array of
 * definitions, or the exported `{format,version,parts}` wrapper. Throws on
 * malformed JSON or a shape it does not recognise.
 */
export function parseCustomParts(text) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    throw new Error(`not valid JSON: ${error.message}`);
  }
  if (Array.isArray(raw)) return raw;
  if (isObject(raw)) {
    if (raw.format === CUSTOM_PARTS_FORMAT) {
      if (raw.version !== CUSTOM_PARTS_VERSION) {
        throw new Error(
          `custom-parts version is ${JSON.stringify(raw.version)}, expected ${CUSTOM_PARTS_VERSION}`,
        );
      }
      if (!Array.isArray(raw.parts)) throw new Error("the custom-parts file has no parts array");
      return raw.parts;
    }
    if (Array.isArray(raw.parts)) return raw.parts;
    return [raw];
  }
  throw new Error("expected a custom part, an array, or a litz-custom-parts file");
}

/** One normalised entry back to the documented definition shape. */
export function definitionOf(entry) {
  const definition = {
    id: entry.id,
    name: entry.name,
  };
  if (entry.description) definition.description = entry.description;
  definition.pins = entry.pins.map((pin) => ({ name: pin.name, kind: pin.kind }));
  definition.attrs = entry.attrs.map((attr) => ({
    name: attr.name,
    type: attr.type,
    ...(attr.values.length > 0 ? { values: [...attr.values] } : {}),
    ...(attr.default !== null && attr.default !== undefined ? { default: attr.default } : {}),
  }));
  if (Number.isFinite(entry.width)) definition.width = entry.width;
  if (Number.isFinite(entry.height)) definition.height = entry.height;
  return definition;
}

/** Add or replace a definition (by id) in the stored list. */
export function upsertCustomPart(definitions, entry) {
  const definition = definitionOf(entry);
  const index = (definitions ?? []).findIndex((item) => item?.id === definition.id);
  if (index < 0) return [...(definitions ?? []), definition];
  const next = [...definitions];
  next[index] = definition;
  return next;
}

export function removeCustomPart(definitions, id) {
  return (definitions ?? []).filter((item) => item?.id !== id);
}

/**
 * Merge validated custom entries into the built-in index. Custom ids replace
 * nothing built-in (validation forbids that), and every built-in entry stays.
 */
export function mergeCatalog(baseIndex, customEntries = []) {
  if (!baseIndex) return baseIndex;
  if (customEntries.length === 0) return baseIndex;
  const replaced = new Set(customEntries.map((entry) => entry.id));
  return indexCatalog([
    ...(baseIndex.entries ?? []).filter((entry) => !replaced.has(entry.id)),
    ...customEntries,
  ]);
}

export function serializeCustomParts(entries) {
  const document = {
    format: CUSTOM_PARTS_FORMAT,
    version: CUSTOM_PARTS_VERSION,
    parts: (entries ?? []).map(definitionOf),
  };
  return `${JSON.stringify(document, null, 2)}\n`;
}

/**
 * Read the stored definitions. Anything unreadable yields an empty list; use
 * `normalizeCustomParts` afterwards to validate what came back.
 */
export function loadCustomParts(storage = defaultStorage()) {
  if (!storage) return [];
  let text;
  try {
    text = storage.getItem(CUSTOM_STORAGE_KEY);
  } catch {
    return [];
  }
  if (typeof text !== "string" || text.trim() === "") return [];
  try {
    return parseCustomParts(text);
  } catch {
    return [];
  }
}

/** Write the definitions. Returns false when storage is unavailable or full. */
export function saveCustomParts(storage, entries) {
  if (!storage) return false;
  try {
    storage.setItem(CUSTOM_STORAGE_KEY, serializeCustomParts(entries));
    return true;
  } catch {
    return false;
  }
}

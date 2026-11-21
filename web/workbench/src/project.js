// The in-memory document and the two JSON shapes it speaks:
//
//   .litz project   {format:"litz-project",version:1,name,circuit,files,options}
//   diagram.json    {version:1,parts:[...],connections:[...]}
//
// Everything here is pure. The document is the only copy the app edits; a
// loaded file is normalised into it and validated against the catalog before
// it can be saved again. Nothing in a file is treated as a path or fetched.

import { findPin, pinKindAccepts } from "./catalog.js";

export const PROJECT_FORMAT = "litz-project";
export const PROJECT_VERSION = 1;
export const PROJECT_EXTENSION = ".litz";
export const RAILS = ["$gnd", "$v33", "$v5", "$vin"];
export const HOST_DEVICES = ["$serialMonitor"];

const PART_ID = /^[A-Za-z0-9_-]+$/;
const FORBIDDEN_ATTR_KEYS = new Set(["__proto__", "constructor", "prototype"]);

const RAIL_KINDS = {
  $gnd: "ground",
  $v33: "power",
  $v5: "power",
  $vin: "power",
};

function isObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

// ---------------------------------------------------------------- pin refs

/**
 * Parse `part:pin`, `part:pin.index`, `$rail` or `$host:pin`. The trailing
 * dot-segment is an index only when every character is a digit, which is what
 * makes `btn1:1.l` a pin literally named `1.l`.
 */
export function parsePinRef(text) {
  if (typeof text !== "string" || text.length === 0) return null;
  const colon = text.indexOf(":");
  if (colon < 0) {
    if (!text.startsWith("$")) return null;
    return { part: text, name: text, index: 0, pseudo: true };
  }
  const part = text.slice(0, colon);
  const rest = text.slice(colon + 1);
  if (!part || !rest) return null;
  let name = rest;
  let index = 0;
  const dot = rest.lastIndexOf(".");
  if (dot > 0) {
    const digits = rest.slice(dot + 1);
    if (/^[0-9]+$/.test(digits)) {
      index = Number(digits);
      if (!Number.isSafeInteger(index) || index > 0xffffffff) return null;
      name = rest.slice(0, dot);
    }
  }
  if (!name) return null;
  return { part, name, index, pseudo: part.startsWith("$") };
}

export function formatPinRef(ref) {
  if (!ref) return "";
  if (ref.pseudo && ref.part === ref.name && ref.index === 0) return ref.part;
  return ref.index === 0 ? `${ref.part}:${ref.name}` : `${ref.part}:${ref.name}.${ref.index}`;
}

export function endpointPart(text) {
  return parsePinRef(text)?.part ?? null;
}

// --------------------------------------------------------------- document

export function emptyCircuit() {
  return { version: 1, parts: [], connections: [] };
}

export function createProject(name = "Untitled") {
  return {
    format: PROJECT_FORMAT,
    version: PROJECT_VERSION,
    name,
    circuit: emptyCircuit(),
    files: [],
    options: {},
  };
}

function scalarAttrs(raw) {
  const attrs = {};
  if (!isObject(raw)) return attrs;
  for (const [key, value] of Object.entries(raw)) {
    if (FORBIDDEN_ATTR_KEYS.has(key)) continue;
    if (value === null) continue;
    if (["string", "number", "boolean"].includes(typeof value)) attrs[key] = value;
  }
  return attrs;
}

function finiteNumber(value, fallback = 0) {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

export function normalizePart(raw, index = 0) {
  if (!isObject(raw)) throw new Error(`part ${index + 1} is not an object`);
  if (typeof raw.id !== "string" || raw.id.length === 0) {
    throw new Error(`part ${index + 1} has no id`);
  }
  if (typeof raw.type !== "string" || raw.type.length === 0) {
    throw new Error(`part ${raw.id} has no type`);
  }
  return {
    id: raw.id,
    type: raw.type,
    attrs: scalarAttrs(raw.attrs),
    left: finiteNumber(raw.left),
    top: finiteNumber(raw.top),
    rotate: finiteNumber(raw.rotate),
  };
}

export function normalizeConnection(raw, index = 0) {
  if (!Array.isArray(raw) || raw.length < 2) {
    throw new Error(`connection ${index + 1} needs a source and a target`);
  }
  if (typeof raw[0] !== "string" || typeof raw[1] !== "string") {
    throw new Error(`connection ${index + 1} endpoints are not strings`);
  }
  return [
    raw[0],
    raw[1],
    typeof raw[2] === "string" ? raw[2] : "",
    Array.isArray(raw[3]) ? raw[3].filter((entry) => typeof entry === "string") : [],
  ];
}

/** Accept only what the workbench models; presentation keys are kept. */
export function normalizeCircuit(raw) {
  if (!isObject(raw)) throw new Error("circuit is not an object");
  if (!Array.isArray(raw.parts)) throw new Error("circuit.parts is not an array");
  if (!Array.isArray(raw.connections)) throw new Error("circuit.connections is not an array");
  return {
    version: 1,
    parts: raw.parts.map(normalizePart),
    connections: raw.connections.map(normalizeConnection),
  };
}

export function normalizeProject(raw) {
  if (!isObject(raw)) throw new Error("project is not an object");
  if (raw.format !== PROJECT_FORMAT) {
    throw new Error(`project format is ${JSON.stringify(raw.format)}, expected ${PROJECT_FORMAT}`);
  }
  if (raw.version !== PROJECT_VERSION) {
    throw new Error(`project version is ${JSON.stringify(raw.version)}, expected ${PROJECT_VERSION}`);
  }
  return {
    format: PROJECT_FORMAT,
    version: PROJECT_VERSION,
    name: typeof raw.name === "string" && raw.name.trim() ? raw.name : "Untitled",
    circuit: normalizeCircuit(raw.circuit),
    files: Array.isArray(raw.files) ? raw.files.filter((file) => typeof file === "string") : [],
    options: isObject(raw.options) ? { ...raw.options } : {},
  };
}

export function projectDocument(project) {
  return {
    format: PROJECT_FORMAT,
    version: PROJECT_VERSION,
    name: project.name,
    circuit: {
      version: 1,
      parts: project.circuit.parts.map((part) => ({
        id: part.id,
        type: part.type,
        attrs: { ...part.attrs },
        left: part.left,
        top: part.top,
        rotate: part.rotate,
      })),
      connections: project.circuit.connections.map(([from, to, color, route]) => [
        from,
        to,
        typeof color === "string" ? color : "",
        Array.isArray(route) ? [...route] : [],
      ]),
    },
    files: Array.isArray(project.files) ? [...project.files] : [],
    options: { ...(project.options ?? {}) },
  };
}

export function serializeProject(project) {
  return `${JSON.stringify(projectDocument(project), null, 2)}\n`;
}

export function parseProject(text) {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    throw new Error(`not valid JSON: ${error.message}`);
  }
  return normalizeProject(raw);
}

/** The diagram.json a project exports. Route metadata is presentation only. */
export function diagramFromProject(project) {
  return {
    version: 1,
    parts: project.circuit.parts.map((part) => ({
      id: part.id,
      type: part.type,
      attrs: { ...part.attrs },
      left: part.left,
      top: part.top,
      rotate: part.rotate,
    })),
    connections: project.circuit.connections.map(([from, to, color]) => [
      from,
      to,
      typeof color === "string" ? color : "",
      [],
    ]),
  };
}

export function projectFromDiagram(raw, name = "Imported circuit") {
  const warnings = [];
  if (isObject(raw) && raw.files) {
    warnings.push("Files listed in the diagram are not loaded by the workbench.");
  }
  return {
    project: { ...createProject(name), circuit: normalizeCircuit(raw) },
    warnings,
  };
}

export function nameFromFile(filename) {
  const base = String(filename || "Imported circuit").split(/[\\/]/).pop();
  const stem = base.replace(/\.(litz|json)$/i, "");
  return !stem || stem.toLowerCase() === "diagram" ? "Imported circuit" : stem;
}

/**
 * Open either document shape. A project is used as-is; a diagram is wrapped
 * in a new project named after the file. Anything else is refused.
 */
export function parseDocument(text, filename = "project.litz") {
  let raw;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    throw new Error(`not valid JSON: ${error.message}`);
  }
  if (isObject(raw) && raw.format === PROJECT_FORMAT) {
    return { kind: "project", project: normalizeProject(raw), warnings: [] };
  }
  if (isObject(raw) && Array.isArray(raw.parts)) {
    const { project, warnings } = projectFromDiagram(raw, nameFromFile(filename));
    return { kind: "diagram", project, warnings };
  }
  throw new Error("the file is neither a Litz project nor a diagram.json");
}

// ------------------------------------------------------------- pure edits

export function withPartAdded(circuit, part) {
  return { ...circuit, parts: [...circuit.parts, part] };
}

export function withPartRemoved(circuit, id) {
  return {
    ...circuit,
    parts: circuit.parts.filter((part) => part.id !== id),
    connections: circuit.connections.filter(
      ([from, to]) => endpointPart(from) !== id && endpointPart(to) !== id,
    ),
  };
}

export function withPartRenamed(circuit, oldId, newId) {
  const rename = (text) => {
    const ref = parsePinRef(text);
    return ref && ref.part === oldId ? formatPinRef({ ...ref, part: newId }) : text;
  };
  return {
    ...circuit,
    parts: circuit.parts.map((part) => (part.id === oldId ? { ...part, id: newId } : part)),
    connections: circuit.connections.map(([from, to, color, route]) => [
      rename(from),
      rename(to),
      color,
      route,
    ]),
  };
}

export function withPartMoved(circuit, id, left, top) {
  return {
    ...circuit,
    parts: circuit.parts.map((part) => (part.id === id ? { ...part, left, top } : part)),
  };
}

export function withPartAttrs(circuit, id, attrs) {
  return {
    ...circuit,
    parts: circuit.parts.map((part) => (part.id === id ? { ...part, attrs: { ...attrs } } : part)),
  };
}

export function withConnectionAdded(circuit, connection) {
  return { ...circuit, connections: [...circuit.connections, normalizeConnection(connection)] };
}

export function withConnectionRemoved(circuit, index) {
  return { ...circuit, connections: circuit.connections.filter((_, i) => i !== index) };
}

export function connectionExists(circuit, a, b) {
  const key = [a, b].sort().join("\u0000");
  return circuit.connections.some(([from, to]) => [from, to].sort().join("\u0000") === key);
}

export function slugForType(type) {
  return (
    String(type)
      .replace(/^board-/, "")
      .replace(/^litz-/, "")
      .replace(/^litz-/, "")
      .replace(/[^A-Za-z0-9_-]+/g, "_")
      .replace(/^_+|_+$/g, "") || "part"
  );
}

export function uniquePartId(circuit, base) {
  const used = new Set(circuit.parts.map((part) => part.id));
  if (!used.has(base)) return base;
  let suffix = 2;
  while (used.has(`${base}${suffix}`)) suffix += 1;
  return `${base}${suffix}`;
}

// ------------------------------------------------------------ validation

function attrMatches(def, value) {
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
      return true;
  }
}

function endpointKind(ref, partsById, index) {
  if (ref.pseudo) {
    if (ref.part === "$serialMonitor") return "digital-io";
    return RAIL_KINDS[ref.part] ?? null;
  }
  const part = partsById.get(ref.part);
  if (!part) return null;
  return findPin(index.byId.get(part.type), ref.name)?.kind ?? null;
}

/**
 * Validate the document against the catalog. Errors block a save; warnings
 * are shown but do not. Pin indices beyond a pin's `repeat` are not checked:
 * `catalog.json` carries the pin name and kind, not the repeat count.
 */
export function validateProject(project, index) {
  const issues = [];
  const error = (code, message) => issues.push({ severity: "error", code, message });
  const warning = (code, message) => issues.push({ severity: "warning", code, message });

  if (!isObject(project)) {
    error("project-shape", "The document is not an object.");
    return { issues, errors: issues, warnings: [] };
  }
  if (project.format !== PROJECT_FORMAT || project.version !== PROJECT_VERSION) {
    error("project-version", `Expected ${PROJECT_FORMAT} version ${PROJECT_VERSION}.`);
  }
  if (typeof project.name !== "string" || project.name.trim() === "") {
    warning("project-name", "The project has no name.");
  }
  if (!Array.isArray(project.files)) {
    warning("project-files", "The project file list is not an array; it will be saved empty.");
  }

  const circuit = project.circuit;
  if (!isObject(circuit) || !Array.isArray(circuit.parts) || !Array.isArray(circuit.connections)) {
    error("circuit-shape", "The circuit needs a parts array and a connections array.");
    return { issues, errors: issues, warnings: [] };
  }

  if (!index) {
    warning("catalog-missing", "The catalog is unavailable, so part types and pins were not checked.");
  }

  const partsById = new Map();
  for (const part of circuit.parts) {
    if (!isObject(part)) {
      error("part-shape", "A part entry is not an object.");
      continue;
    }
    if (typeof part.id !== "string" || !PART_ID.test(part.id)) {
      error("part-id", `Part id ${JSON.stringify(part.id)} must match ${PART_ID}.`);
      continue;
    }
    if (part.id.startsWith("$")) {
      error("reserved-id", `Part id ${JSON.stringify(part.id)} starts with "$", which is reserved.`);
    }
    if (partsById.has(part.id)) {
      error("duplicate-id", `Part id ${JSON.stringify(part.id)} is used more than once.`);
    }
    partsById.set(part.id, part);
    if (typeof part.type !== "string" || part.type === "") {
      error("part-type", `Part ${JSON.stringify(part.id)} has no type.`);
      continue;
    }
    const entry = index?.byId.get(part.type);
    if (index && !entry) {
      error("unknown-part", `Part ${part.id} has type ${part.type}, which is not in the catalog.`);
      continue;
    }
    if (!entry) continue;
    if (!isObject(part.attrs)) {
      warning("part-attrs", `Part ${part.id} has no attribute map; it is treated as empty.`);
    }
    for (const [name, value] of Object.entries(isObject(part.attrs) ? part.attrs : {})) {
      const def = entry.attrs.find((attr) => attr.name === name);
      if (!def) {
        warning("unknown-attr", `Part ${part.id} sets unknown attribute ${JSON.stringify(name)}.`);
      } else if (!attrMatches(def, value)) {
        error(
          "attr-type",
          `Part ${part.id} attribute ${JSON.stringify(name)} does not match ${def.type}.`,
        );
      }
    }
    for (const key of ["left", "top", "rotate"]) {
      if (part[key] !== undefined && !Number.isFinite(part[key])) {
        error("part-position", `Part ${part.id} has a non-numeric ${key}.`);
      }
    }
  }

  const wired = new Set();
  circuit.connections.forEach((connection, connectionIndex) => {
    if (!Array.isArray(connection) || connection.length < 2) {
      error("connection-shape", `Connection ${connectionIndex + 1} needs two endpoints.`);
      return;
    }
    const refs = [];
    for (const text of [connection[0], connection[1]]) {
      const ref = parsePinRef(text);
      if (!ref) {
        error("invalid-pin-ref", `Connection ${connectionIndex + 1} endpoint ${JSON.stringify(text)} is not a pin reference.`);
        refs.push(null);
        continue;
      }
      refs.push(ref);
      if (ref.pseudo) {
        if (!RAILS.includes(ref.part) && !HOST_DEVICES.includes(ref.part)) {
          error(
            "unknown-host-device",
            `${formatPinRef(ref)} names a host device the format does not define.`,
          );
        }
        continue;
      }
      const part = partsById.get(ref.part);
      if (!part) {
        error(
          "unknown-part-ref",
          `${formatPinRef(ref)} refers to a part that is not placed in the circuit.`,
        );
        continue;
      }
      const entry = index?.byId.get(part.type);
      if (entry && !findPin(entry, ref.name)) {
        error("unknown-pin", `${formatPinRef(ref)}: ${part.type} has no pin ${JSON.stringify(ref.name)}.`);
      }
    }

    const [a, b] = refs;
    if (a && b) {
      const railA = a.pseudo && RAILS.includes(a.part) ? a.part : null;
      const railB = b.pseudo && RAILS.includes(b.part) ? b.part : null;
      if (railA && railB && railA !== railB) {
        error("rail-short", `${railA} is wired to ${railB}.`);
      } else {
        const kindA = endpointKind(a, partsById, index);
        const kindB = endpointKind(b, partsById, index);
        if (kindA && kindB && !pinKindAccepts(kindA, kindB)) {
          error("pin-kind", `${formatPinRef(a)} (${kindA}) cannot be wired to ${formatPinRef(b)} (${kindB}).`);
        }
      }
      const key = [formatPinRef(a), formatPinRef(b)].sort().join("\u0000");
      if (wired.has(key)) warning("duplicate-wire", `${formatPinRef(a)} and ${formatPinRef(b)} are wired more than once.`);
      wired.add(key);
    }
  });

  return {
    issues,
    errors: issues.filter((issue) => issue.severity === "error"),
    warnings: issues.filter((issue) => issue.severity === "warning"),
  };
}
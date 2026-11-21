// Catalog indexing and queries. `catalog.json` is produced by
// `litz catalog --json`; this module only rearranges what it holds.

export const CATEGORIES = [
  { id: "boards", label: "Boards" },
  { id: "parts", label: "Parts" },
];

/**
 * Build the lookup structures the picker and the inspector use. Unknown
 * `kind` values are treated as leaf parts, and missing arrays become empty
 * so one malformed entry cannot break the picker.
 */
export function indexCatalog(entries) {
  if (!Array.isArray(entries)) throw new TypeError("catalog.json must hold an array");
  const byId = new Map();
  const boards = [];
  const parts = [];
  for (const raw of entries) {
    if (!raw || typeof raw.id !== "string" || raw.id.length === 0) continue;
    const entry = {
      id: raw.id,
      kind: raw.kind === "board" ? "board" : "part",
      name: typeof raw.name === "string" && raw.name ? raw.name : raw.id,
      description: typeof raw.description === "string" ? raw.description : "",
      pins: Array.isArray(raw.pins)
        ? raw.pins
            .filter((pin) => pin && typeof pin.name === "string")
            .map((pin) => ({ name: pin.name, kind: String(pin.kind ?? "") }))
        : [],
      attrs: Array.isArray(raw.attrs)
        ? raw.attrs
            .filter((attr) => attr && typeof attr.name === "string")
            .map((attr) => ({
              name: attr.name,
              type: String(attr.type ?? "string"),
              values: Array.isArray(attr.values) ? attr.values.map(String) : [],
              default: attr.default ?? null,
            }))
        : [],
    };
    // Custom entries may carry a fixed box size and an origin marker. Built-in
    // catalog entries have neither, so nothing changes for them.
    if (Number.isFinite(raw.width) && raw.width > 0) entry.width = raw.width;
    if (Number.isFinite(raw.height) && raw.height > 0) entry.height = raw.height;
    if (raw.custom === true) entry.custom = true;
    byId.set(entry.id, entry);
    (entry.kind === "board" ? boards : parts).push(entry);
  }
  const byName = (a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id);
  boards.sort(byName);
  parts.sort(byName);
  return { byId, boards, parts, entries: [...boards, ...parts] };
}

function score(entry, query) {
  const name = entry.name.toLowerCase();
  const id = entry.id.toLowerCase();
  const description = entry.description.toLowerCase();
  if (name === query || id === query) return 0;
  if (name.startsWith(query)) return 1;
  if (id.startsWith(query)) return 2;
  if (name.includes(query)) return 3;
  if (id.includes(query)) return 4;
  if (description.includes(query)) return 5;
  return Infinity;
}

/** Case-insensitive ranked search, preserving the boards/parts split. */
export function searchCatalog(index, query) {
  const term = String(query ?? "").trim().toLowerCase();
  if (term === "") return { boards: index.boards, parts: index.parts };
  const rank = (list) =>
    list
      .map((entry) => ({ entry, rank: score(entry, term) }))
      .filter((item) => Number.isFinite(item.rank))
      .sort((a, b) => a.rank - b.rank || a.entry.name.localeCompare(b.entry.name))
      .map((item) => item.entry);
  return { boards: rank(index.boards), parts: rank(index.parts) };
}

/** Picker groups, with empty groups removed. */
export function groupedCatalog(result) {
  return CATEGORIES.map((category) => ({
    ...category,
    items: category.id === "boards" ? result.boards : result.parts,
  })).filter((category) => category.items.length > 0);
}

export function findPin(entry, name) {
  if (!entry) return null;
  return entry.pins.find((pin) => pin.name === name) ?? null;
}

export function pinIndex(entry) {
  return new Map((entry?.pins ?? []).map((pin) => [pin.name, pin]));
}

/**
 * The same structural rule the Rust catalog applies: an NC pin connects to
 * nothing, and a supply rail never meets ground. Everything else is legal
 * wiring even when it will misbehave electrically.
 */
export function pinKindAccepts(kindA, kindB) {
  const a = String(kindA);
  const b = String(kindB);
  if (a === "nc" || b === "nc") return false;
  if ((a === "power" && b === "ground") || (a === "ground" && b === "power")) return false;
  return true;
}

/** Attributes a newly placed part starts with. */
export function defaultAttrs(entry) {
  const attrs = {};
  for (const attr of entry?.attrs ?? []) {
    if (attr.default !== null && attr.default !== undefined) attrs[attr.name] = attr.default;
  }
  return attrs;
}
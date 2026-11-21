// Where a part's pins sit, derived from its catalog entry. Pure: the same
// calculation positions the drawn pin and the endpoint of a wire.

import { rotatePoint } from "./geometry.js";

export const TITLE_HEIGHT = 42;
export const ROW_HEIGHT = 16;
export const PAD_BOTTOM = 10;
export const MIN_HEIGHT = 64;

/** Overall box for a catalog entry. Boards are wider than leaf parts. */
export function partMetrics(entry) {
  const pins = entry?.pins ?? [];
  const rows = Math.max(1, Math.ceil(pins.length / 2));
  // A custom part may ask for a size; never go below what the pins need.
  const defaultWidth = entry?.kind === "board" ? 180 : 132;
  const minHeight = Math.max(MIN_HEIGHT, TITLE_HEIGHT + rows * ROW_HEIGHT + PAD_BOTTOM);
  const width =
    Number.isFinite(entry?.width) && entry.width > 0
      ? Math.max(80, entry.width)
      : defaultWidth;
  const height =
    Number.isFinite(entry?.height) && entry.height > 0
      ? Math.max(minHeight, entry.height)
      : minHeight;
  return { width, height };
}

/**
 * One entry per declared pin. The first half of the pin list is on the left
 * edge, the rest on the right, each edge top to bottom in declaration order.
 */
export function pinSlots(entry) {
  const pins = entry?.pins ?? [];
  const { width } = partMetrics(entry);
  const half = Math.ceil(pins.length / 2);
  return pins.map((pin, index) => {
    const side = index < half ? "left" : "right";
    const row = side === "left" ? index : index - half;
    return {
      name: pin.name,
      kind: pin.kind,
      side,
      x: side === "left" ? 0 : width,
      y: TITLE_HEIGHT + row * ROW_HEIGHT,
      labelX: side === "left" ? 10 : width - 10,
      anchor: side === "left" ? "start" : "end",
      row,
    };
  });
}

/** World position of one pin, applying the part's rotation about its centre. */
export function pinWorld(part, entry, pinName) {
  const slot = pinSlots(entry).find((candidate) => candidate.name === pinName);
  if (!slot) return null;
  const { width, height } = partMetrics(entry);
  const local = { x: slot.x, y: slot.y };
  const centre = { x: width / 2, y: height / 2 };
  const rotated = rotatePoint(local, centre, Number(part.rotate) || 0);
  return {
    x: (Number(part.left) || 0) + rotated.x,
    y: (Number(part.top) || 0) + rotated.y,
  };
}

// Interactive controls -------------------------------------------------------
//
// Everything here is in the part's local coordinate system (before rotation),
// so the same numbers position the drawn control and answer a pointer.

const CONTROL_KINDS = new Map([
  ["litz-led", "led"],
  ["litz-pushbutton", "button"],
  ["litz-pushbutton-6mm", "button"],
  ["litz-slide-switch", "switch"],
  ["litz-potentiometer", "pot"],
  ["litz-slide-potentiometer", "pot"],
  ["litz-rotary-encoder", "rotary"],
]);

/** The control a catalog entry draws, or null for an inert part. */
export function controlKind(entry) {
  if (!entry) return null;
  return CONTROL_KINDS.get(entry.id) ?? null;
}

/**
 * Control geometry in part-local coordinates. The canvas draws from this and
 * the pointer path hit-tests against it, so the two cannot drift apart.
 */
export function partVisual(entry) {
  const { width, height } = partMetrics(entry);
  const kind = controlKind(entry);
  if (!kind) return { kind: null, width, height };
  if (kind === "led") {
    return {
      kind,
      width,
      height,
      led: { cx: width - 28, cy: height / 2, r: 7, halo: 16 },
    };
  }
  if (kind === "button") {
    return {
      kind,
      width,
      height,
      cap: { x: width / 2 - 18, y: height / 2 - 9, width: 36, height: 18, rx: 3 },
      hit: { x: width / 2 - 24, y: height / 2 - 15, width: 48, height: 30 },
    };
  }
  if (kind === "switch") {
    const track = { x: 14, y: height / 2 - 9, width: width - 28, height: 18, rx: 9 };
    return {
      kind,
      width,
      height,
      track,
      knob: { x: 18, y: height / 2 - 7, width: 28, height: 14, rx: 7, offX: 18, onX: 38 },
      hit: track,
    };
  }
  if (kind === "pot") {
    const knob = { cx: width / 2, cy: height / 2 + 4, r: 26 };
    return { kind, width, height, knob, hit: knob };
  }
  // A rotary encoder: a knob with a push switch under it.
  const knob = { cx: width / 2, cy: height / 2, r: 24 };
  return { kind, width, height, knob, hit: knob };
}

function contains(box, point) {
  return (
    point.x >= box.x &&
    point.x <= box.x + box.width &&
    point.y >= box.y &&
    point.y <= box.y + box.height
  );
}

/** World point to the part's local frame, undoing `translate` then `rotate`. */
export function partLocalPoint(part, entry, point) {
  if (!point || !Number.isFinite(point.x) || !Number.isFinite(point.y)) return null;
  const { width, height } = partMetrics(entry);
  const centre = { x: width / 2, y: height / 2 };
  const translated = {
    x: point.x - (Number(part.left) || 0),
    y: point.y - (Number(part.top) || 0),
  };
  return rotatePoint(translated, centre, -(Number(part.rotate) || 0));
}

/**
 * Which control a world point lands on. Returns `{kind, local, inside}` or
 * null. `inside` is false when the part is hit but not the drawn control, so
 * a caller can fall back to moving the part.
 */
export function controlAt(part, entry, point) {
  const visual = partVisual(entry);
  if (!visual.kind) return null;
  const local = partLocalPoint(part, entry, point);
  if (!local) return null;
  let inside;
  if (visual.led) inside = Math.hypot(local.x - visual.led.cx, local.y - visual.led.cy) <= visual.led.halo;
  else if (visual.knob) inside = Math.hypot(local.x - visual.knob.cx, local.y - visual.knob.cy) <= visual.knob.r + 4;
  else inside = contains(visual.hit, local);
  return { kind: visual.kind, local, inside };
}

/** Permille of travel for a knob at `point`; null for parts without one. */
export function controlPermille(part, entry, point) {
  const visual = partVisual(entry);
  if (visual.kind !== "pot" && visual.kind !== "rotary") return null;
  const local = partLocalPoint(part, entry, point);
  if (!local) return null;
  const margin = 12;
  const span = Math.max(1, visual.width - margin * 2);
  const t = (local.x - margin) / span;
  return Math.round(Math.min(1, Math.max(0, t)) * 1000);
}

/** Axis-aligned bounds after rotation, used for placement and hit tests. */
export function partBounds(part, entry) {
  const { width, height } = partMetrics(entry);
  const centre = { x: width / 2, y: height / 2 };
  const corners = [
    { x: 0, y: 0 },
    { x: width, y: 0 },
    { x: width, y: height },
    { x: 0, y: height },
  ].map((corner) => rotatePoint(corner, centre, Number(part.rotate) || 0));
  const xs = corners.map((corner) => corner.x);
  const ys = corners.map((corner) => corner.y);
  const left = (Number(part.left) || 0) + Math.min(...xs);
  const top = (Number(part.top) || 0) + Math.min(...ys);
  return {
    x: left,
    y: top,
    width: Math.max(...xs) - Math.min(...xs),
    height: Math.max(...ys) - Math.min(...ys),
  };
}

/**
 * A free grid position for a newly added part. `boundsFor` supplies the
 * occupied box of each existing part, since their catalog entries differ.
 */
export function nextPlacement(
  parts,
  entry,
  boundsFor = (part) => partBounds(part, entry),
  { columns = 6 } = {},
) {
  const { width, height } = partMetrics(entry);
  const ceilGrid = (value) => Math.ceil(value / 20) * 20;
  const stepX = ceilGrid(Math.max(180, width + 60));
  const stepY = ceilGrid(Math.max(120, height + 40));
  for (let index = 0; index < 96; index += 1) {
    const x = 80 + (index % columns) * stepX;
    const y = 80 + Math.floor(index / columns) * stepY;
    const overlaps = parts.some((part) => {
      const bounds = boundsFor(part);
      return !(
        x + width + 20 < bounds.x ||
        x > bounds.x + bounds.width + 20 ||
        y + height + 20 < bounds.y ||
        y > bounds.y + bounds.height + 20
      );
    });
    if (!overlaps) return { left: x, top: y };
  }
  return { left: 80, top: 80 };
}
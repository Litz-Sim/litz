// Pure wire geometry. No DOM, so `node --test` can exercise it directly.

export const GRID = 20;

/** Round a coordinate to the schematic grid. */
export function snap(value, grid = GRID) {
  if (!Number.isFinite(value)) return 0;
  return Math.round(value / grid) * grid;
}

export function snapPoint(point, grid = GRID) {
  return { x: snap(point?.x, grid), y: snap(point?.y, grid) };
}

/** Rotate `point` around `center` by `degrees` (SVG's y-down convention). */
export function rotatePoint(point, center, degrees) {
  const radians = ((Number(degrees) || 0) * Math.PI) / 180;
  if (radians === 0) return { x: point.x, y: point.y };
  const cos = Math.cos(radians);
  const sin = Math.sin(radians);
  const dx = point.x - center.x;
  const dy = point.y - center.y;
  return {
    x: center.x + dx * cos - dy * sin,
    y: center.y + dx * sin + dy * cos,
  };
}

function finite(point) {
  return Boolean(point) && Number.isFinite(point.x) && Number.isFinite(point.y);
}

/**
 * Drop repeated points and collinear middle points, leaving the corners a
 * renderer needs. A path of fewer than two points is not a wire.
 */
export function simplifyPoints(points) {
  const out = [];
  for (const point of points ?? []) {
    if (!finite(point)) continue;
    const last = out[out.length - 1];
    if (last && last.x === point.x && last.y === point.y) continue;
    out.push({ x: point.x, y: point.y });
  }
  for (let i = 1; i < out.length - 1; ) {
    const a = out[i - 1];
    const b = out[i];
    const c = out[i + 1];
    const cross = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if (Math.abs(cross) < 1e-9) out.splice(i, 1);
    else i += 1;
  }
  return out;
}

/**
 * An orthogonal route between two pins. A span whose horizontal distance is
 * at least its vertical distance bends at the horizontal midpoint first;
 * otherwise it bends at the vertical midpoint. The result always starts at
 * `from`, ends at `to`, and every segment is axis-aligned.
 */
export function routeWire(from, to) {
  if (!finite(from) || !finite(to)) return [];
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  if (Math.abs(dx) < 1e-9 || Math.abs(dy) < 1e-9) {
    return simplifyPoints([from, to]);
  }
  if (Math.abs(dx) >= Math.abs(dy)) {
    const midX = from.x + dx / 2;
    return simplifyPoints([
      from,
      { x: midX, y: from.y },
      { x: midX, y: to.y },
      to,
    ]);
  }
  const midY = from.y + dy / 2;
  return simplifyPoints([
    from,
    { x: from.x, y: midY },
    { x: to.x, y: midY },
    to,
  ]);
}

function number(value) {
  return String(Math.round(value * 1000) / 1000);
}

/** SVG path data for a polyline. */
export function polylinePath(points) {
  if (!points || points.length === 0) return "";
  let d = `M${number(points[0].x)} ${number(points[0].y)}`;
  for (let i = 1; i < points.length; i += 1) {
    d += ` L${number(points[i].x)} ${number(points[i].y)}`;
  }
  return d;
}

/** Total length along the axes. */
export function manhattanLength(points) {
  let total = 0;
  for (let i = 1; i < (points?.length ?? 0); i += 1) {
    total += Math.abs(points[i].x - points[i - 1].x) + Math.abs(points[i].y - points[i - 1].y);
  }
  return total;
}

/** Shortest distance from a point to a line segment. */
export function pointSegmentDistance(point, a, b) {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lengthSq = dx * dx + dy * dy;
  if (lengthSq === 0) return Math.hypot(point.x - a.x, point.y - a.y);
  let t = ((point.x - a.x) * dx + (point.y - a.y) * dy) / lengthSq;
  t = Math.max(0, Math.min(1, t));
  const x = a.x + t * dx;
  const y = a.y + t * dy;
  return Math.hypot(point.x - x, point.y - y);
}

/** Distance from a point to the nearest segment of a polyline. */
export function distanceToPolyline(point, points) {
  if (!points || points.length === 0) return Infinity;
  if (points.length === 1) return Math.hypot(point.x - points[0].x, point.y - points[0].y);
  let best = Infinity;
  for (let i = 1; i < points.length; i += 1) {
    best = Math.min(best, pointSegmentDistance(point, points[i - 1], points[i]));
  }
  return best;
}

/** Axis-aligned bounds of a point list. */
export function pointsBounds(points) {
  if (!points || points.length === 0) return null;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const point of points) {
    minX = Math.min(minX, point.x);
    minY = Math.min(minY, point.y);
    maxX = Math.max(maxX, point.x);
    maxY = Math.max(maxY, point.y);
  }
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}
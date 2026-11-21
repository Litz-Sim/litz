import assert from "node:assert/strict";
import test from "node:test";

import {
  distanceToPolyline,
  manhattanLength,
  polylinePath,
  routeWire,
  simplifyPoints,
  snap,
  snapPoint,
} from "../src/geometry.js";
import { partMetrics, pinWorld } from "../src/layout.js";

test("geometry: snapping lands on the 20 px grid", () => {
  assert.equal(snap(0), 0);
  assert.equal(snap(29), 20);
  assert.equal(snap(31), 40);
  assert.equal(snap(-11), -20);
  assert.deepEqual(snapPoint({ x: 29, y: 51 }), { x: 20, y: 60 });
});

test("geometry: a wide wire bends at its horizontal midpoint first", () => {
  const points = routeWire({ x: 0, y: 0 }, { x: 100, y: 40 });
  assert.deepEqual(points, [
    { x: 0, y: 0 },
    { x: 50, y: 0 },
    { x: 50, y: 40 },
    { x: 100, y: 40 },
  ]);
});

test("geometry: a tall wire bends at its vertical midpoint first", () => {
  const points = routeWire({ x: 0, y: 0 }, { x: 40, y: 100 });
  assert.deepEqual(points, [
    { x: 0, y: 0 },
    { x: 0, y: 50 },
    { x: 40, y: 50 },
    { x: 40, y: 100 },
  ]);
});

test("geometry: an aligned wire is one straight segment", () => {
  assert.deepEqual(routeWire({ x: 10, y: 10 }, { x: 90, y: 10 }), [
    { x: 10, y: 10 },
    { x: 90, y: 10 },
  ]);
  assert.deepEqual(routeWire({ x: 10, y: 10 }, { x: 10, y: 90 }), [
    { x: 10, y: 10 },
    { x: 10, y: 90 },
  ]);
});

test("geometry: every routed segment is axis-aligned and ends on the pins", () => {
  const cases = [
    [{ x: 0, y: 0 }, { x: 100, y: 40 }],
    [{ x: 0, y: 0 }, { x: 40, y: 100 }],
    [{ x: 200, y: 60 }, { x: -30, y: -90 }],
    [{ x: 7, y: 13 }, { x: 7, y: 99 }],
  ];
  for (const [from, to] of cases) {
    const points = routeWire(from, to);
    assert.ok(points.length >= 2, `route ${JSON.stringify(from)} -> ${JSON.stringify(to)}`);
    assert.deepEqual(points[0], from);
    assert.deepEqual(points.at(-1), to);
    for (let i = 1; i < points.length; i += 1) {
      const a = points[i - 1];
      const b = points[i];
      assert.ok(
        a.x === b.x || a.y === b.y,
        `segment ${JSON.stringify(a)} -> ${JSON.stringify(b)} is not orthogonal`,
      );
    }
  }
});

test("geometry: collinear and repeated points collapse", () => {
  assert.deepEqual(
    simplifyPoints([
      { x: 0, y: 0 },
      { x: 0, y: 0 },
      { x: 10, y: 0 },
      { x: 20, y: 0 },
      { x: 20, y: 10 },
    ]),
    [
      { x: 0, y: 0 },
      { x: 20, y: 0 },
      { x: 20, y: 10 },
    ],
  );
  assert.deepEqual(simplifyPoints([]), []);
});

test("geometry: path, length and distance agree with the points", () => {
  const points = [
    { x: 0, y: 0 },
    { x: 30, y: 0 },
    { x: 30, y: 40 },
  ];
  assert.equal(polylinePath(points), "M0 0 L30 0 L30 40");
  assert.equal(manhattanLength(points), 70);
  assert.equal(distanceToPolyline({ x: 10, y: 5 }, points), 5);
  assert.equal(distanceToPolyline({ x: 35, y: 20 }, points), 5);
  assert.equal(distanceToPolyline({ x: 0, y: 0 }, []), Infinity);
});

test("geometry: rotated pins follow the part transform", () => {
  const entry = {
    kind: "part",
    pins: [
      { name: "A", kind: "digital-in" },
      { name: "C", kind: "ground" },
    ],
    attrs: [],
  };
  const { width, height } = partMetrics(entry);
  assert.equal(width, 132);
  assert.equal(height, 68);

  const upright = { id: "led", type: "led", attrs: {}, left: 100, top: 50, rotate: 0 };
  assert.deepEqual(pinWorld(upright, entry, "A"), { x: 100, y: 92 });
  assert.deepEqual(pinWorld(upright, entry, "C"), { x: 232, y: 92 });

  const turned = { ...upright, rotate: 90 };
  assert.deepEqual(pinWorld(turned, entry, "A"), { x: 158, y: 18 });
  assert.deepEqual(pinWorld(turned, entry, "C"), { x: 158, y: 150 });
  assert.equal(pinWorld(turned, entry, "missing"), null);
});
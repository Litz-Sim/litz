import assert from "node:assert/strict";
import test from "node:test";

import {
  controlAt,
  controlKind,
  controlPermille,
  partLocalPoint,
  partMetrics,
  partVisual,
} from "../src/layout.js";

function entry(id, pins) {
  return { id, kind: "part", name: id, pins, attrs: [] };
}

const LED = entry("litz-led", [
  { name: "A", kind: "digital-in" },
  { name: "C", kind: "ground" },
]);
const BUTTON = entry("litz-pushbutton", [
  { name: "1.l", kind: "passive" },
  { name: "1.r", kind: "passive" },
  { name: "2.l", kind: "passive" },
  { name: "2.r", kind: "passive" },
]);
const POT = entry("litz-potentiometer", [
  { name: "1", kind: "passive" },
  { name: "2", kind: "analog-io" },
  { name: "3", kind: "passive" },
]);
const RESISTOR = entry("litz-resistor", [
  { name: "1", kind: "passive" },
  { name: "2", kind: "passive" },
]);

function part(type, left = 100, top = 50, rotate = 0) {
  return { id: type, type, attrs: {}, left, top, rotate };
}

test("layout: controls are classified by catalog id", () => {
  assert.equal(controlKind(LED), "led");
  assert.equal(controlKind(BUTTON), "button");
  assert.equal(controlKind(POT), "pot");
  assert.equal(controlKind(entry("litz-slide-switch", [])), "switch");
  assert.equal(controlKind(entry("litz-rotary-encoder", [])), "rotary");
  assert.equal(controlKind(entry("litz-pushbutton-6mm", [])), "button");
  assert.equal(controlKind(RESISTOR), null);
  assert.equal(controlKind(null), null);
});

test("layout: visual geometry sits inside the part box", () => {
  const led = partVisual(LED);
  assert.equal(led.kind, "led");
  assert.equal(led.led.cx, led.width - 28);
  assert.equal(led.led.cy, led.height / 2);

  const button = partVisual(BUTTON);
  assert.equal(button.kind, "button");
  assert.equal(button.cap.x + button.cap.width / 2, button.width / 2);
  assert.equal(button.cap.y + button.cap.height / 2, button.height / 2);

  const pot = partVisual(POT);
  assert.equal(pot.kind, "pot");
  assert.equal(pot.knob.cx, pot.width / 2);

  assert.equal(partVisual(RESISTOR).kind, null);
});

test("layout: local points undo translation and rotation", () => {
  const upright = part(BUTTON.id);
  assert.deepEqual(partLocalPoint(upright, BUTTON, { x: 110, y: 60 }), { x: 10, y: 10 });

  const turned = part(BUTTON.id, 100, 50, 90);
  const { width, height } = partMetrics(BUTTON);
  assert.deepEqual(partLocalPoint(turned, BUTTON, { x: 100 + width / 2, y: 50 + height / 2 }), {
    x: width / 2,
    y: height / 2,
  });
  assert.equal(partLocalPoint(upright, BUTTON, { x: NaN, y: 0 }), null);
});

test("layout: hit tests land on the drawn control and miss outside it", () => {
  const led = part(LED.id);
  const ledVisual = partVisual(LED);
  const onLed = controlAt(led, LED, {
    x: 100 + ledVisual.led.cx,
    y: 50 + ledVisual.led.cy,
  });
  assert.equal(onLed.kind, "led");
  assert.equal(onLed.inside, true);
  assert.equal(
    controlAt(led, LED, { x: 100 + ledVisual.led.halo + 10, y: 50 + ledVisual.led.cy }).inside,
    false,
  );

  const button = part(BUTTON.id);
  const buttonVisual = partVisual(BUTTON);
  const onCap = controlAt(button, BUTTON, {
    x: 100 + buttonVisual.cap.x + buttonVisual.cap.width / 2,
    y: 50 + buttonVisual.cap.y + buttonVisual.cap.height / 2,
  });
  assert.equal(onCap.kind, "button");
  assert.equal(onCap.inside, true);

  assert.equal(controlAt(part(RESISTOR.id), RESISTOR, { x: 120, y: 60 }), null);
});

test("layout: a knob maps horizontal travel to permille", () => {
  const pot = part(POT.id);
  const { width } = partMetrics(POT);
  assert.equal(controlPermille(pot, POT, { x: 100 + 12, y: 80 }), 0);
  assert.equal(controlPermille(pot, POT, { x: 100 + width - 12, y: 80 }), 1000);
  assert.equal(controlPermille(pot, POT, { x: 100 + width / 2, y: 80 }), 500);
  assert.equal(controlPermille(pot, POT, { x: 100 - 50, y: 80 }), 0, "travel is clamped");
  assert.equal(controlPermille(part(LED.id), LED, { x: 120, y: 80 }), null);
});
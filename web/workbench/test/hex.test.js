import assert from "node:assert/strict";
import test from "node:test";

import {
  firmwareImage,
  looksLikeIntelHex,
  parseIntelHex,
} from "../src/hex.js";

function record(type, address, data = []) {
  const bytes = [data.length, (address >> 8) & 0xff, address & 0xff, type, ...data];
  let sum = 0;
  for (const byte of bytes) sum += byte;
  bytes.push((0x100 - (sum & 0xff)) & 0xff);
  return `:${bytes.map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

function image(records) {
  return [...records, record(1, 0)].join("\n");
}

test("hex: a data record becomes its bytes at the record address", () => {
  const parsed = parseIntelHex(
    image([record(0, 0x100, [0x21, 0x46, 0x01, 0x36])]),
  );
  assert.deepEqual([...parsed.bytes], [0x21, 0x46, 0x01, 0x36]);
  assert.equal(parsed.minAddress, 0x100);
  assert.equal(parsed.maxAddress, 0x104);
  assert.equal(parsed.startAddress, null);
});

test("hex: the classic 16-byte sample record parses", () => {
  const parsed = parseIntelHex(
    ":10010000214601360121470136007EFE09D2190140\n:00000001FF\n",
  );
  assert.equal(parsed.bytes.length, 16);
  assert.equal(parsed.minAddress, 0x100);
  assert.equal(parsed.bytes[0], 0x21);
  assert.equal(parsed.bytes[15], 0x01);
});

test("hex: extended linear addresses place data above 64 KiB", () => {
  const parsed = parseIntelHex(
    image([
      record(4, 0, [0x08, 0x00]),
      record(0, 0x0010, [0xaa, 0xbb]),
    ]),
  );
  assert.equal(parsed.minAddress, 0x08000010);
  assert.equal(parsed.maxAddress, 0x08000012);
  assert.deepEqual([...parsed.bytes], [0xaa, 0xbb]);
});

test("hex: extended segment addresses use the 16-byte segment base", () => {
  const parsed = parseIntelHex(
    image([
      record(2, 0, [0x10, 0x00]),
      record(0, 0x0002, [0x7f]),
    ]),
  );
  assert.equal(parsed.minAddress, 0x10002);
  assert.deepEqual([...parsed.bytes], [0x7f]);
});

test("hex: gaps between records are filled with 0xff", () => {
  const parsed = parseIntelHex(
    image([
      record(0, 0, [0x01]),
      record(0, 4, [0x02]),
    ]),
  );
  assert.deepEqual([...parsed.bytes], [0x01, 0xff, 0xff, 0xff, 0x02]);
});

test("hex: start-address records are exposed but do not add data", () => {
  const parsed = parseIntelHex(
    image([
      record(0, 0, [0x90]),
      record(5, 0, [0x00, 0x01, 0x00, 0x02]),
    ]),
  );
  assert.equal(parsed.startAddress, 0x00010002);
  assert.equal(parsed.bytes.length, 1);
});

test("hex: a bad checksum is refused", () => {
  assert.throws(
    () => parseIntelHex(":0400000001020304F0\n:00000001FF\n"),
    /checksum/,
  );
});

test("hex: short, odd and non-hex records are refused", () => {
  assert.throws(() => parseIntelHex(":04000000\n:00000001FF\n"), /too short/);
  assert.throws(() => parseIntelHex(":040000000102030\n:00000001FF\n"), /odd number/);
  assert.throws(() => parseIntelHex(":04000000ZZ020304F0\n:00000001FF\n"), /non-hex/);
  assert.throws(() => parseIntelHex("no colon\n"), /starts with/);
});

test("hex: a byte count that disagrees with the record is refused", () => {
  assert.throws(
    () => parseIntelHex(":0500000001020304F8\n:00000001FF\n"),
    /does not match/,
  );
});

test("hex: a missing end-of-file, duplicate data and unsupported types are refused", () => {
  assert.throws(() => parseIntelHex(record(0, 0, [1, 2])), /end-of-file/);
  assert.throws(
    () => parseIntelHex(image([record(0, 0, [1]), record(0, 0, [2])])),
    /written twice/,
  );
  assert.throws(
    () => parseIntelHex(image([record(6, 0, [1])])),
    /unsupported record type/,
  );
  assert.throws(() => parseIntelHex(image([])), /no data records/);
});

test("hex: content detection recognises text and ignores blank lines", () => {
  assert.equal(looksLikeIntelHex(":00000001FF\n"), true);
  assert.equal(looksLikeIntelHex("﻿  \r\n:0400000001020304F2\r\n"), true);
  assert.equal(looksLikeIntelHex(new TextEncoder().encode(":00000001FF\n")), true);
  assert.equal(looksLikeIntelHex("\x00\x01\x02"), false);
  assert.equal(looksLikeIntelHex(""), false);
});

test("hex: firmwareImage converts hex names and content, and keeps raw bins", () => {
  const hex = firmwareImage("firmware.hex", image([record(0, 0, [0xde, 0xad])]));
  assert.equal(hex.kind, "hex");
  assert.deepEqual([...hex.bytes], [0xde, 0xad]);

  const detected = firmwareImage("renamed.bin", image([record(0, 0, [0x01])]));
  assert.equal(detected.kind, "hex");

  const raw = firmwareImage("firmware.bin", new Uint8Array([0xe9, 0x00]));
  assert.equal(raw.kind, "bin");
  assert.deepEqual([...raw.bytes], [0xe9, 0x00]);

  assert.throws(() => firmwareImage("broken.hex", "not a hex file"), /starts with/);
});
#!/bin/sh
# The official MicroPython firmware images the examples and the ignored
# MicroPython test boot, plus the ESP32-C3 mask ROM they need. The RP2040
# build is published as a UF2 rather than a flat image, so a verified UF2 is
# converted to the `.bin` the runner takes (needs python3; nothing else here
# does). micropython.org publishes each ESP release as a merged image
# (bootloader at 0x0, partition table at 0x8000, application at 0x10000),
# which is one of the two layouts the runner detects; the image is MIT-licensed
# and is not vendored.
#
# This is a build step, not something the runner does: `litz` never downloads.
# A file already present with the right hash is kept, so a cached directory
# costs no download. Every byte is pinned by SHA-256 here rather than fetched
# from a "latest" URL, so a green run is reproducible.
#
#   tools/fetch-micropython.sh [DIR]     default build/micropython
#
# It also fetches esp32c3_rev3_rom.elf (Apache-2.0, espressif/esp-rom-elfs),
# because the ESP32-C3 image runs from the mask ROM. Use the same directory:
#
#   LITZ_ROM_DIR=build/micropython cargo run -p litz-cli -- test examples/micropython
set -e
cd "$(dirname "$0")/.."

DIR=${1:-build/micropython}
# The release date and tag are part of the upstream file name; print them so a
# log says which interpreter was exercised.
RELEASE="v1.29.0 (2026-08-24)"

# name and SHA-256 of the official merged images.
IMAGES='ESP32_GENERIC-20260824-v1.29.0.bin e67ad6015a0a504c1fec9aa9bbf589d0432ed28e62546f4f8dd8a147f8bd95f6
ESP32_GENERIC_C3-20260824-v1.29.0.bin bf72ed9eb88ad3a8f49d02c3d371f9ea34c90a8a303aeb9e324d8efd4a2a655a
ESP32_GENERIC_S3-20260824-v1.29.0.bin 1dd9dcd27ba2d6a1cb7f7b49a063f46d3d807d68b0151161233920228285e300
ESP32_GENERIC_C6-20260824-v1.29.0.bin 3688ba4e2c3949e639f5772e99fdb0efa1258b162a496133f78c108434195c89
RPI_PICO-20260824-v1.29.0.uf2 e1160e602e277d85920adb5fe741435edccba9ba6ecadc85fdab75252eeff4e9'

# The mask ROMs the merged MicroPython images need, from the same pinned
# release the emulator's own tooling uses. Every ELF is pinned by hash; the
# tarball is only fetched when one of them is missing.
ROMS='esp32_rev300_rom.elf 920b70635440517866aab2230964a570d2cf2b676658d93c52fbac108c1cca31
esp32c3_rev3_rom.elf 19ac22e08707df926fb0cf4c54795d4067b983fae8635f396ded173a6d78fc3c
esp32c6_rev0_rom.elf 788e1d38724aeb8fd974fa10c4a7b089c02627d35342ce84b9e0b12b239f3551
esp32s3_rev0_rom.elf c0ce0f338d1de1bdc6efbef1591779a2a42c1ab7d759d3c6ae8ae63a7dd34cfd'
ROM_RELEASE=20260528
ROM_TARBALL_SHA=caa463d3cbef2430a5a35847c1d9f2f152403b17a802050927ff60c8da54fe46
ROM_URL=https://github.com/espressif/esp-rom-elfs/releases/download/$ROM_RELEASE/esp-rom-elfs-$ROM_RELEASE.tar.gz

# -c without the GNU-only --status, so macOS shasum and Linux sha256sum both work.
sha256_ok() {  # FILE HASH
  if command -v shasum >/dev/null 2>&1; then
    echo "$2  $1" | shasum -a 256 -c >/dev/null 2>&1
  else
    echo "$2  $1" | sha256sum -c >/dev/null 2>&1
  fi
}

mkdir -p "$DIR"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "MicroPython $RELEASE into $DIR"

while read -r name hash; do
  [ -n "$name" ] || continue
  if [ -f "$DIR/$name" ] && sha256_ok "$DIR/$name" "$hash"; then
    echo "  $name: already present"
  else
    curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 \
      "https://micropython.org/resources/firmware/$name" -o "$tmp/$name"
    sha256_ok "$tmp/$name" "$hash" || {
      echo "$name: SHA-256 mismatch" >&2
      exit 1
    }
    mv "$tmp/$name" "$DIR/$name"
    echo "  $name: fetched"
  fi
  size=$(wc -c <"$DIR/$name" | tr -d ' ')
  echo "    $size bytes  sha256 $hash"
done <<EOF
$IMAGES
EOF

missing=''
while read -r rom_name rom_sha; do
  [ -n "$rom_name" ] || continue
  if [ -f "$DIR/$rom_name" ] && sha256_ok "$DIR/$rom_name" "$rom_sha"; then
    echo "  $rom_name: already present"
  else
    missing="$missing $rom_name"
  fi
done <<EOF
$ROMS
EOF

if [ -n "$missing" ]; then
  curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 "$ROM_URL" -o "$tmp/rom.tar.gz"
  sha256_ok "$tmp/rom.tar.gz" "$ROM_TARBALL_SHA" || {
    echo "$ROM_URL: SHA-256 mismatch" >&2
    exit 1
  }
  mkdir "$tmp/rom"
  tar -xzf "$tmp/rom.tar.gz" -C "$tmp/rom"
  for rom_name in $missing; do
    rom_sha=$(echo "$ROMS" | awk -v n="$rom_name" '$1 == n { print $2 }')
    rom=$(find "$tmp/rom" -name "$rom_name" | head -n 1)
    [ -n "$rom" ] || {
      echo "$rom_name is not in esp-rom-elfs $ROM_RELEASE" >&2
      exit 1
    }
    sha256_ok "$rom" "$rom_sha" || {
      echo "$rom_name: SHA-256 mismatch" >&2
      exit 1
    }
    cp "$rom" "$DIR/$rom_name"
    echo "  $rom_name: fetched"
  done
fi

while read -r rom_name rom_sha; do
  [ -n "$rom_name" ] || continue
  echo "    $rom_name $(wc -c <"$DIR/$rom_name" | tr -d ' ') bytes  sha256 $rom_sha"
done <<EOF
$ROMS
EOF

# The RP2040 build is published as a UF2, which is a container rather than an
# image: 512-byte blocks, each carrying the flash address of its payload. The
# runner takes a flat dump of QSPI flash from 0x1000_0000, so a verified UF2 is
# converted once to the .bin beside it. Needs python3; nothing else here does.
for uf2 in "$DIR"/*.uf2; do
  [ -f "$uf2" ] || continue
  bin=${uf2%.uf2}.bin
  if [ -f "$bin" ] && [ "$bin" -nt "$uf2" ]; then
    continue
  fi
  python3 - "$uf2" "$bin" <<'PY'
import struct
import sys

source, target = sys.argv[1], sys.argv[2]
data = open(source, "rb").read()
chunks = {}
for offset in range(0, len(data), 512):
    block = data[offset:offset + 512]
    if len(block) < 512:
        break
    magic0, magic1, _flags, address, size = struct.unpack("<IIIII", block[:20])
    if (magic0, magic1) != (0x0A324655, 0x9E5D5157):
        continue
    chunks[address] = block[32:32 + size]
if not chunks:
    sys.exit(f"{source}: no UF2 blocks")
low = min(chunks)
high = max(address + len(payload) for address, payload in chunks.items())
image = bytearray(high - low)
for address, payload in chunks.items():
    image[address - low:address - low + len(payload)] = payload
open(target, "wb").write(image)
print(f"  {target.rsplit('/', 1)[-1]}: {len(image)} bytes from UF2 at {low:#010x}")
PY
done

echo "done: MicroPython $RELEASE in $DIR"
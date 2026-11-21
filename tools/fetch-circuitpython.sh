#!/bin/sh
# Official CircuitPython firmware images for the ESP32 family, for the
# `runtime = "circuitpython"` example and anyone booting one under Litz.
#
#   tools/fetch-circuitpython.sh VERSION CHIP [DIR]
#   tools/fetch-circuitpython.sh 10.3.2 esp32s3
#
# VERSION is a released CircuitPython version (`10.3.2`), not "latest": the
# download URL is built from it, so a log says exactly which interpreter was
# exercised. CHIP is one of esp32s3, esp32c3, esp32c6 or esp32, each mapped to
# the official Espressif development board whose board id CircuitPython
# publishes.
#
# The images are .bin (a raw flash image), not .uf2: the simulator boots flash
# directly and has no TinyUF2 USB mass-storage stage to hand a UF2 to. The URL
# layout is the one circuitpython.org links to:
#
#   https://downloads.circuitpython.org/bin/<board>/en_US/adafruit-circuitpython-<board>-en_US-<version>.bin
#
# CircuitPython publishes these per release, so no SHA-256 is pinned here: the
# version is the caller's choice. The hash is printed after the download for a
# CI config to record. A version or board that does not exist is reported as
# the 404 URL rather than guessed at or redirected to a mirror.
#
# This is a build step, not something the runner does: `litz` never downloads.
# Downloads land under build/, never a system temporary directory.
set -e
cd "$(dirname "$0")/.."

usage() {
  cat >&2 <<'EOF'
usage: tools/fetch-circuitpython.sh VERSION CHIP [DIR]

  VERSION  a released CircuitPython version, e.g. 10.3.2 (not "latest")
  CHIP     esp32s3, esp32c3, esp32c6 or esp32
  DIR      download directory, default build/circuitpython

Example:
  tools/fetch-circuitpython.sh 10.3.2 esp32s3
EOF
}

[ $# -ge 2 ] || { usage; exit 2; }
VERSION=$1
CHIP=$2
DIR=${3:-build/circuitpython}

# CircuitPython's release tags carry no leading "v"; accept one anyway so a
# tag copied from GitHub still works. A slash or space would change the URL
# path, so reject it rather than fetching an unintended file.
VERSION=${VERSION#v}
case "$VERSION" in
  ''|*/*|*' '*)
    echo "version \"$VERSION\" is not a released version like 10.3.2" >&2
    exit 2
    ;;
esac

# The board id picks the image; the ROM name is what the runner looks for when
# it boots the merged image through the mask ROM.
case "$CHIP" in
  esp32s3) BOARD=espressif_esp32s3_devkitc_1_n8; ROM=esp32s3_rev0_rom.elf ;;
  esp32c3) BOARD=espressif_esp32c3_devkitm_1_n4; ROM=esp32c3_rev3_rom.elf ;;
  esp32c6) BOARD=espressif_esp32c6_devkitc_1_n8; ROM=esp32c6_rev0_rom.elf ;;
  esp32)   BOARD=espressif_esp32_devkitc_v4_wroom_32e; ROM=esp32_rev300_rom.elf ;;
  *)
    echo "chip \"$CHIP\" has no image here; use esp32s3, esp32c3, esp32c6 or esp32" >&2
    exit 2
    ;;
esac

# A system temporary directory would be cleaned out from under a later run and
# is never where a locked build expects its image.
case "$DIR" in
  /tmp/*|/private/tmp/*)
    echo "refusing to download into $DIR; use a path under build/" >&2
    exit 2
    ;;
esac

command -v curl >/dev/null 2>&1 || {
  echo "curl is required to fetch CircuitPython images" >&2
  exit 1
}

NAME=adafruit-circuitpython-$BOARD-en_US-$VERSION.bin
URL=https://downloads.circuitpython.org/bin/$BOARD/en_US/$NAME
STABLE=circuitpython-$CHIP.bin

mkdir -p "$DIR"

echo "CircuitPython $VERSION for $CHIP ($BOARD) into $DIR"
if [ -f "$DIR/$NAME" ]; then
  echo "  $NAME: already present"
else
  # The partial file stays inside DIR: nothing is downloaded to /tmp.
  if ! curl -fsSL --retry 5 --retry-all-errors --retry-delay 5 \
    "$URL" -o "$DIR/$NAME.part"; then
    rm -f "$DIR/$NAME.part"
    echo "no CircuitPython $VERSION .bin for $CHIP at" >&2
    echo "  $URL" >&2
    echo "check the version at https://circuitpython.org/board/$BOARD/ and try again" >&2
    exit 1
  fi
  mv "$DIR/$NAME.part" "$DIR/$NAME"
  echo "  $NAME: fetched"
fi

# The example names one stable file, so its litz.toml does not change with
# every CircuitPython release.
if ln -sf "$NAME" "$DIR/$STABLE" 2>/dev/null; then :; else
  cp "$DIR/$NAME" "$DIR/$STABLE"
fi

size=$(wc -c <"$DIR/$NAME" | tr -d ' ')
echo "    $size bytes  $DIR/$NAME"
# The hash is informational: the caller chose the version, so the value to
# pin is whatever this prints for that choice.
if command -v shasum >/dev/null 2>&1; then
  echo "    sha256 $(shasum -a 256 "$DIR/$NAME" | cut -d' ' -f1)"
else
  echo "    sha256 $(sha256sum "$DIR/$NAME" | cut -d' ' -f1)"
fi
echo "done: $DIR/$STABLE -> $NAME"

# CircuitPython ships one merged image (bootloader, partition table and
# application), so the simulator starts it through the mask ROM. That ROM is
# not part of this download; tools/fetch-micropython.sh already fetches the
# same esp-rom-elfs files.
if [ ! -f "$DIR/$ROM" ]; then
  echo "note: $CHIP boots its merged image from the mask ROM"
  echo "      put $ROM in $DIR, or set LITZ_ROM_DIR to a directory that has it"
fi

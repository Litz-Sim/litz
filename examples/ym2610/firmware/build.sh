#!/bin/sh
# Rebuild firmware/ym2610-demo.bin from main.c next to this script.
#
# Needs arm-none-eabi-gcc and the repository's own litz-fixture for the
# 256-byte stage 2. The binary is committed, so running the example needs
# neither.
set -eu
cd "$(dirname "$0")"

cargo run -q --manifest-path ../../../Cargo.toml -p litz-fixture -- stage2 stage2.bin
arm-none-eabi-gcc -mcpu=cortex-m0plus -mthumb -O2 -ffreestanding -nostdlib \
    -fno-asynchronous-unwind-tables -T link.ld -o app.elf main.c vectors.S -lgcc
arm-none-eabi-objcopy -O binary app.elf app.bin

cat stage2.bin app.bin > ym2610-demo.bin
rm -f stage2.bin app.elf app.bin
echo "wrote firmware/ym2610-demo.bin"

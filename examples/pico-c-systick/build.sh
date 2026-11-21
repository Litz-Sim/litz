#!/bin/sh
# Rebuild firmware/pico-c-systick.bin from the C sources next to this script.
#
# Needs arm-none-eabi-gcc (Homebrew: arm-none-eabi-gcc, Debian: gcc-arm-none-eabi)
# and the repository's own litz-fixture for the 256-byte stage 2. The binary is
# committed, so running the example does not need this.
set -eu
cd "$(dirname "$0")"

cargo run -q --manifest-path ../../Cargo.toml -p litz-fixture -- stage2 stage2.bin
arm-none-eabi-gcc -mcpu=cortex-m0plus -mthumb -O2 -ffreestanding -nostdlib \
    -fno-asynchronous-unwind-tables -T link.ld -o app.elf main.c vectors.S -lgcc
arm-none-eabi-objcopy -O binary app.elf app.bin

# Stage 2 goes at flash offset 0 and the application's vector table at 0x100,
# which is where the ROM looks after stage 2 returns.
cat stage2.bin app.bin > firmware/pico-c-systick.bin
rm -f stage2.bin app.elf app.bin
echo "wrote firmware/pico-c-systick.bin"

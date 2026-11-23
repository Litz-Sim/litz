/* The application's vector table sits where the ROM looks for it: the 256
 * bytes right after stage 2. Code and read-only data follow in flash; `.bss`
 * lives in SRAM above the 256 bytes stage 2 was copied into, which is where a
 * real crt0 puts it. There is no `.data` initialiser: this image has no
 * initialised globals, so nothing has to be copied out of flash at boot. */
ENTRY(reset)
SECTIONS
{
  . = 0x10000100;
  .vectors : { KEEP(*(.vectors)) }
  .text : { *(.text*) *(.rodata*) }
  .bss 0x20001000 (NOLOAD) : { *(.bss*) *(COMMON) }
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.attributes) *(.comment) }
}

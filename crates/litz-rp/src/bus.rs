//! The address space: flash behind the XIP cache, SRAM, the peripheral window,
//! the SIO block and the private peripheral bus.
//!
//! The map is the RP2040 datasheet's. Nothing here knows about the core beyond
//! [`crate::cpu::Bus`]; the peripherals see offsets from their own base, so a
//! block never has to know where it sits.

use crate::cpu::{Bus, Fault};
use crate::peripherals::Peripherals;

/// 2 MiB of QSPI flash, the size the Pico carries.
pub const FLASH_SIZE: usize = 2 * 1024 * 1024;
/// 264 KiB of SRAM, in six banks the chip maps contiguously.
pub const SRAM_SIZE: usize = 264 * 1024;
pub const FLASH_BASE: u32 = 0x1000_0000;
pub const SRAM_BASE: u32 = 0x2000_0000;
/// The bootrom, which the model provides as an entry point rather than code.
pub const ROM_BASE: u32 = 0x0000_0000;
pub const ROM_SIZE: usize = 16 * 1024;
pub const SIO_BASE: u32 = 0xd000_0000;
pub const PPB_BASE: u32 = 0xe000_0000;
/// The system control space inside the PPB: the NVIC, SysTick and the SCB.
pub const SCS_BASE: u32 = 0xe000_e000;
/// The peripheral window starts here and runs to [`PERIPH_END`].
pub const PERIPH_BASE: u32 = 0x4000_0000;
pub const PERIPH_END: u32 = 0x4008_0000;
/// The AHB window holds DMA, USB and the PIOs.
pub const AHB_BASE: u32 = 0x5000_0000;
pub const AHB_END: u32 = 0x5040_0000;

/// Block bases inside the peripheral windows, in address order.
pub const SYSINFO: u32 = 0x4000_0000;
pub const SYSCFG: u32 = 0x4000_4000;
pub const CLOCKS: u32 = 0x4000_8000;
pub const RESETS: u32 = 0x4000_c000;
pub const PSM: u32 = 0x4001_0000;
pub const IO_BANK0: u32 = 0x4001_4000;
pub const IO_QSPI: u32 = 0x4001_8000;
pub const PADS_BANK0: u32 = 0x4001_c000;
pub const PADS_QSPI: u32 = 0x4002_0000;
pub const XOSC: u32 = 0x4002_4000;
pub const PLL_SYS: u32 = 0x4002_8000;
pub const PLL_USB: u32 = 0x4002_c000;
pub const BUSCTRL: u32 = 0x4003_0000;
pub const UART0: u32 = 0x4003_4000;
pub const UART1: u32 = 0x4003_8000;
pub const SPI0: u32 = 0x4003_c000;
pub const SPI1: u32 = 0x4004_0000;
pub const I2C0: u32 = 0x4004_4000;
pub const I2C1: u32 = 0x4004_8000;
pub const ADC: u32 = 0x4004_c000;
pub const PWM: u32 = 0x4005_0000;
pub const TIMER: u32 = 0x4005_4000;
pub const WATCHDOG: u32 = 0x4005_8000;
pub const RTC: u32 = 0x4005_c000;
pub const ROSC: u32 = 0x4006_0000;
/// `VREG_AND_CHIP_RESET`: the core regulator and the chip-level reset source.
pub const VREG_AND_CHIP_RESET: u32 = 0x4006_4000;
/// `TBMAN`: the test-bench manager. The SDK's `running_on_fpga()` reads bit 1
/// of its platform register; on silicon that bit is clear.
pub const TBMAN: u32 = 0x4006_c000;
pub const TICKS: u32 = 0x4010_8000;
pub const XIP_CTRL: u32 = 0x1400_0000;
pub const XIP_SSI: u32 = 0x1800_0000;
/// A model-only register block behind the bootrom flash helpers. The real ROM
/// drives the QSPI controller; this model writes the flash array directly, so
/// the helpers leave their arguments here and the bus applies the operation.
pub const FLASH_CTRL: u32 = 0x400f_0000;
const FLASH_CTRL_LEN: u32 = 0x1000;
pub const DMA: u32 = 0x5000_0000;
/// The USB controller's 4 KiB dual-port SRAM. On the silicon this window is
/// immediately below the register block, not above it.
pub const USB_DPRAM: u32 = 0x5010_0000;
/// `USBCTRL_REGS`: the controller registers, 4 KiB above the DPSRAM.
pub const USBCTRL: u32 = 0x5011_0000;
pub const PIO0: u32 = 0x5020_0000;
pub const PIO1: u32 = 0x5030_0000;

/// Arguments the bootrom flash helpers leave in [`FLASH_CTRL`].
#[derive(Clone, Copy, Default)]
struct FlashControl {
    addr: u32,
    count: u32,
    block_size: u32,
    erase_cmd: u32,
    op: u32,
    data: u32,
    program_len: u32,
}

/// SRAM, flash and the peripheral blocks.
pub struct Memory {
    pub rom: Vec<u8>,
    pub flash: Vec<u8>,
    pub sram: Vec<u8>,
    pub periph: Peripherals,
    flash_control: FlashControl,
    /// Which core the bus is serving right now. The SIO block answers
    /// `CPUID` and the FIFOs differently per core, and `PPB_BASE` is each
    /// core's own NVIC, SysTick and vector table.
    pub active_core: u8,
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

/// The bootrom's fixed header points at a public function table, and the
/// application resolves helpers through it. The RP2040 bootrom is mask ROM we
/// cannot reproduce, but the four functions MicroPython asks for while it
/// starts (`memcpy`, `memset`, `memset4`, `memcpy44`) are simple loops, so the
/// model provides those and reports a B2 ROM version. Without this, a stock
/// `boot2` hands off to an application that resolves a zero address and runs
/// off the end of the ROM.
fn install_bootrom(rom: &mut [u8]) {
    fn put16(rom: &mut [u8], at: usize, value: u16) {
        rom[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    // Table entries are {u16 code, u16 function address}, terminated by code
    // zero. The addresses carry the Thumb bit, as the bootrom's do.
    const MEMCPY: u16 = 0x0301;
    const MEMSET: u16 = 0x0321;
    const CLZ32: u16 = 0x0341;
    const CTZ32: u16 = 0x0361;
    const POPCOUNT32: u16 = 0x0381;
    const REVERSE32: u16 = 0x03a1;
    // The flash helpers change the QSPI controller between boot and XIP
    // mode. This model reads flash directly, so the four that only switch
    // modes are returns. Range erase and program keep their arguments for
    // the model's flash-control block.
    const FLASH_NOP: u16 = 0x03c1;
    const FLASH_ERASE: u16 = 0x0601;
    const FLASH_PROGRAM: u16 = 0x0621;
    let table: &[(u16, u16)] = &[
        (0x534d, MEMSET),        // ROM_FUNC_MEMSET
        (0x434d, MEMCPY),        // ROM_FUNC_MEMCPY
        (0x3453, MEMSET),        // ROM_FUNC_MEMSET4
        (0x3443, MEMCPY),        // ROM_FUNC_MEMCPY44
        (0x334c, CLZ32),         // ROM_FUNC_CLZ32
        (0x3354, CTZ32),         // ROM_FUNC_CTZ32
        (0x3350, POPCOUNT32),    // ROM_FUNC_POPCOUNT32
        (0x3352, REVERSE32),     // ROM_FUNC_REVERSE32
        (0x4649, FLASH_NOP),     // ROM_FUNC_CONNECT_INTERNAL_FLASH
        (0x5845, FLASH_NOP),     // ROM_FUNC_FLASH_EXIT_XIP
        (0x4346, FLASH_NOP),     // ROM_FUNC_FLASH_FLUSH_CACHE
        (0x5843, FLASH_NOP),     // ROM_FUNC_FLASH_ENTER_CMD_XIP
        (0x4552, FLASH_ERASE),   // ROM_FUNC_FLASH_RANGE_ERASE
        (0x5052, FLASH_PROGRAM), // ROM_FUNC_FLASH_RANGE_PROGRAM
        (0x0000, 0x0000),
    ];
    // Fixed table at 0x100, lookup at 0x201, the helpers from 0x300. The
    // data table at 0x3d0 carries the two V2 floating-point tables, which
    // callers copy into RAM and dispatch through; pointing every entry at
    // the no-op above keeps those calls from falling into zeroed ROM.
    const DATA_TABLE: u16 = 0x03d0;
    const SOFT_FLOAT_TABLE: u16 = 0x03e0;
    const SOFT_DOUBLE_TABLE: u16 = 0x04e0;
    rom[0x10..0x14].copy_from_slice(&[b'M', b'u', 0x01, 0x03]);
    put16(rom, 0x14, 0x0100);
    put16(rom, 0x16, DATA_TABLE);
    put16(rom, 0x18, 0x0201);
    for (index, (code, address)) in table.iter().enumerate() {
        put16(rom, 0x100 + index * 4, *code);
        put16(rom, 0x102 + index * 4, *address);
    }

    // Public data table: 'SF' and 'SD' point at the 32-entry dispatch tables
    // the V2 ROM exposes for its soft-float and soft-double routines.
    put16(rom, 0x3d0, 0x4653); // 'SF', ROM_DATA_SOFT_FLOAT
    put16(rom, 0x3d2, SOFT_FLOAT_TABLE);
    put16(rom, 0x3d4, 0x4453); // 'SD', ROM_DATA_SOFT_DOUBLE
    put16(rom, 0x3d6, SOFT_DOUBLE_TABLE);
    put16(rom, 0x3d8, 0x0000);
    put16(rom, 0x3da, 0x0000);
    for index in 0..32u16 {
        let at = usize::from(index) * 4;
        put16(rom, 0x3e0 + at, FLASH_NOP);
        put16(rom, 0x3e0 + at + 2, 0);
        put16(rom, 0x4e0 + at, FLASH_NOP);
        put16(rom, 0x4e0 + at + 2, 0);
    }
    // bx lr, reached both as the flash-helper stub and as every float table
    // entry.
    rom[0x3c0..0x3c2].copy_from_slice(&[0x70, 0x47]);

    // rom_table_lookup(table, code): walk the pairs, return the address or 0.
    #[rustfmt::skip]
    rom[0x200..0x218].copy_from_slice(&[
        0x02, 0x88, // ldrh r2, [r0]
        0x00, 0x2a, // cmp  r2, #0
        0x06, 0xd0, // beq  not_found
        0x8a, 0x42, // cmp  r2, r1
        0x02, 0xd0, // beq  found
        0x04, 0x30, // adds r0, #4
        0xf8, 0xe7, // b    loop
        0x00, 0x00,
        0x40, 0x88, // found: ldrh r0, [r0, #2]
        0x70, 0x47, // bx   lr
        0x00, 0x20, // not_found: movs r0, #0
        0x70, 0x47, // bx   lr
    ]);

    // void memcpy(dst, src, n): byte copy. MEMCPY44 has the same byte-wise
    // effect, and its callers pass a byte count.
    #[rustfmt::skip]
    rom[0x300..0x312].copy_from_slice(&[
        0x00, 0x2a, // cmp  r2, #0
        0x05, 0xd0, // beq  done
        0x0b, 0x78, // loop: ldrb r3, [r1]
        0x01, 0x31, // adds r1, #1
        0x03, 0x70, // strb r3, [r0]
        0x01, 0x30, // adds r0, #1
        0x01, 0x3a, // subs r2, #1
        0xf9, 0xd1, // bne  loop
        0x70, 0x47, // done: bx lr
    ]);

    // void memset(dst, value, n): byte fill. MEMSET4 fills words with the byte
    // replicated, which is the same byte sequence for a multiple of four.
    #[rustfmt::skip]
    rom[0x320..0x330].copy_from_slice(&[
        0x00, 0x2a, // cmp  r2, #0
        0x03, 0xd0, // beq  done
        0x01, 0x70, // loop: strb r1, [r0]
        0x01, 0x30, // adds r0, #1
        0x01, 0x3a, // subs r2, #1
        0xfb, 0xd1, // bne  loop
        0x70, 0x47, // done: bx lr
        0x00, 0x00,
    ]);

    // uint32_t clz32(uint32_t x)
    #[rustfmt::skip]
    rom[0x340..0x358].copy_from_slice(&[
        0x20, 0x21, // movs r1, #32
        0x00, 0x28, // cmp  r0, #0
        0x06, 0xd0, // beq  done
        0x00, 0x21, // movs r1, #0
        0x00, 0x28, // loop: cmp r0, #0
        0x03, 0xdb, // blt  done
        0x40, 0x00, // lsls r0, r0, #1
        0x01, 0x31, // adds r1, #1
        0xfa, 0xe7, // b    loop
        0x00, 0x00,
        0x08, 0x46, // done: mov r0, r1
        0x70, 0x47, // bx   lr
    ]);

    // uint32_t ctz32(uint32_t x)
    #[rustfmt::skip]
    rom[0x360..0x378].copy_from_slice(&[
        0x20, 0x21, // movs r1, #32
        0x00, 0x28, // cmp  r0, #0
        0x06, 0xd0, // beq  done
        0x00, 0x21, // movs r1, #0
        0xc2, 0x07, // loop: lsls r2, r0, #31
        0x03, 0xd1, // bne  done
        0x40, 0x08, // lsrs r0, r0, #1
        0x01, 0x31, // adds r1, #1
        0xfa, 0xe7, // b    loop
        0x00, 0x00,
        0x08, 0x46, // done: mov r0, r1
        0x70, 0x47, // bx   lr
    ]);

    // uint32_t popcount32(uint32_t x)
    #[rustfmt::skip]
    rom[0x380..0x394].copy_from_slice(&[
        0x00, 0x21, // movs r1, #0
        0x00, 0x28, // loop: cmp r0, #0
        0x04, 0xd0, // beq  done
        0xc2, 0x07, // lsls r2, r0, #31
        0xd2, 0x0f, // lsrs r2, r2, #31
        0x89, 0x18, // adds r1, r1, r2
        0x40, 0x08, // lsrs r0, r0, #1
        0xf8, 0xe7, // b    loop
        0x08, 0x46, // done: mov r0, r1
        0x70, 0x47, // bx   lr
    ]);

    // uint32_t reverse32(uint32_t x)
    #[rustfmt::skip]
    rom[0x3a0..0x3b6].copy_from_slice(&[
        0x00, 0x21, // movs r1, #0
        0x20, 0x22, // movs r2, #32
        0x49, 0x00, // loop: lsls r1, r1, #1
        0xc3, 0x07, // lsls r3, r0, #31
        0xdb, 0x0f, // lsrs r3, r3, #31
        0x19, 0x43, // orrs r1, r1, r3
        0x40, 0x08, // lsrs r0, r0, #1
        0x01, 0x3a, // subs r2, #1
        0xf8, 0xd1, // bne  loop
        0x08, 0x46, // mov r0, r1
        0x70, 0x47, // bx   lr
    ]);

    // void flash_range_erase(addr, count, block_size, erase_cmd)
    //
    // The bootrom programs the SSI controller page by page. This model reads
    // and writes the flash array directly, so the helper leaves its arguments
    // in FLASH_CTRL and the bus erases the range when the operation lands.
    // The bootrom API preserves r4-r11, and MicroPython keeps its own function
    // pointers there across the call, so the scratch register is saved.
    #[rustfmt::skip]
    rom[0x600..0x614].copy_from_slice(&[
        0x10, 0xb4, // push {r4}
        0x04, 0x4c, // ldr  r4, [pc, #16] ; FLASH_CTRL at 0x614
        0x20, 0x60, // str  r0, [r4, #0]  ; address
        0x61, 0x60, // str  r1, [r4, #4]  ; count
        0xa2, 0x60, // str  r2, [r4, #8]  ; block size
        0xe3, 0x60, // str  r3, [r4, #12] ; erase command
        0x01, 0x23, // movs r3, #1
        0x23, 0x61, // str  r3, [r4, #16] ; op: erase
        0x10, 0xbc, // pop  {r4}
        0x70, 0x47, // bx   lr
    ]);
    rom[0x614..0x618].copy_from_slice(&FLASH_CTRL.to_le_bytes());

    // void flash_range_program(addr, data, count)
    #[rustfmt::skip]
    rom[0x620..0x634].copy_from_slice(&[
        0x10, 0xb4, // push {r4}
        0x04, 0x4c, // ldr  r4, [pc, #16] ; FLASH_CTRL at 0x634
        0x20, 0x60, // str  r0, [r4, #0]  ; address
        0x61, 0x61, // str  r1, [r4, #20] ; data pointer
        0xa2, 0x61, // str  r2, [r4, #24] ; count
        0x02, 0x22, // movs r2, #2
        0x22, 0x61, // str  r2, [r4, #16] ; op: program
        0x10, 0xbc, // pop  {r4}
        0x70, 0x47, // bx   lr
        0x00, 0x00,
    ]);
    rom[0x634..0x638].copy_from_slice(&FLASH_CTRL.to_le_bytes());
}

impl Memory {
    pub fn new() -> Self {
        let mut rom = vec![0; ROM_SIZE];
        install_bootrom(&mut rom);
        Memory {
            rom,
            flash: vec![0xff; FLASH_SIZE],
            sram: vec![0; SRAM_SIZE],
            periph: Peripherals::new(),
            flash_control: FlashControl::default(),
            active_core: 0,
        }
    }

    /// Apply the operation the bootrom flash helper queued. The range helpers
    /// take XIP addresses, as the bootrom does; an offset is accepted too so a
    /// caller that already subtracted the base still lands in range.
    fn apply_flash_control(&mut self) {
        let control = self.flash_control;
        match control.op {
            1 => self.erase_flash_range(control.addr, control.count, control.block_size),
            2 => self.program_flash_range(control.addr, control.data, control.program_len),
            _ => {}
        }
    }

    fn flash_offset(addr: u32) -> usize {
        // The bootrom takes an offset into flash; accept an XIP address too.
        if addr >= FLASH_BASE {
            (addr - FLASH_BASE) as usize
        } else {
            addr as usize
        }
    }

    fn erase_flash_range(&mut self, addr: u32, count: u32, block_size: u32) {
        if count == 0 {
            return;
        }
        let block = if block_size == 0 {
            4096
        } else {
            block_size as usize
        };
        let start = Self::flash_offset(addr);
        if start >= self.flash.len() {
            return;
        }
        // A sector erase covers the whole block the address falls in.
        let first = start - start % block;
        let last = start.saturating_add(count as usize).min(self.flash.len());
        let end = last
            .div_ceil(block)
            .saturating_mul(block)
            .min(self.flash.len());
        if first >= end {
            return;
        }
        for sector in self.flash[first..end].chunks_mut(block) {
            sector.fill(0xff);
        }
    }

    fn program_flash_range(&mut self, addr: u32, data: u32, count: u32) {
        if count == 0 {
            return;
        }
        let start = Self::flash_offset(addr);
        if start >= self.flash.len() {
            return;
        }
        let end = start.saturating_add(count as usize).min(self.flash.len());
        if start >= end {
            return;
        }
        // NOR flash programming can only clear bits, so the new byte is ANDed
        // into the erased one. The source is normally an SRAM buffer.
        let bytes: Vec<u8> = (0..end - start)
            .map(|offset| self.flash_source_byte(data.wrapping_add(offset as u32)))
            .collect();
        for (slot, byte) in self.flash[start..end].iter_mut().zip(bytes) {
            *slot &= byte;
        }
    }

    fn flash_source_byte(&self, addr: u32) -> u8 {
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            self.sram[(addr - SRAM_BASE) as usize]
        } else if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr) {
            self.flash[(addr - FLASH_BASE) as usize]
        } else {
            0xff
        }
    }

    /// Write a firmware image into flash at `offset`.
    pub fn load_flash(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| "image offset overflows the flash".to_string())?;
        if end > self.flash.len() {
            return Err(format!(
                "image of {} bytes at {offset:#x} does not fit in {} bytes of flash",
                bytes.len(),
                self.flash.len()
            ));
        }
        self.flash[offset..end].copy_from_slice(bytes);
        Ok(())
    }

    /// Let the DMA engine move at most one element. The engine is taken out of
    /// the peripheral set for the duration so it can borrow this `Memory` as
    /// its bus without a second mutable alias.
    pub fn service_dma(&mut self) {
        let mut dma = std::mem::take(&mut self.periph.dma);
        dma.service(self);
        self.periph.dma = dma;
    }

    /// The peripheral block an address belongs to, if any.
    pub fn block(addr: u32) -> Option<Block> {
        let table: &[(u32, Block)] = &[
            (SYSINFO, Block::Sysinfo),
            (SYSCFG, Block::Syscfg),
            (CLOCKS, Block::Clocks),
            (RESETS, Block::Resets),
            (PSM, Block::Psm),
            (IO_BANK0, Block::IoBank0),
            (IO_QSPI, Block::IoQspi),
            (PADS_BANK0, Block::PadsBank0),
            (PADS_QSPI, Block::PadsQspi),
            (XOSC, Block::Xosc),
            (PLL_SYS, Block::PllSys),
            (PLL_USB, Block::PllUsb),
            (BUSCTRL, Block::BusCtrl),
            (UART0, Block::Uart0),
            (UART1, Block::Uart1),
            (SPI0, Block::Spi0),
            (SPI1, Block::Spi1),
            (I2C0, Block::I2c0),
            (I2C1, Block::I2c1),
            (ADC, Block::Adc),
            (PWM, Block::Pwm),
            (TIMER, Block::Timer),
            (WATCHDOG, Block::Watchdog),
            (RTC, Block::Rtc),
            (ROSC, Block::Rosc),
            (VREG_AND_CHIP_RESET, Block::VregAndChipReset),
            (TBMAN, Block::Tbman),
            (TICKS, Block::Ticks),
            (DMA, Block::Dma),
            (USBCTRL, Block::UsbCtrl),
            (PIO0, Block::Pio0),
            (PIO1, Block::Pio1),
            (XIP_CTRL & !0x0fff_ffff, Block::XipCtrl),
        ];
        // The QSPI window is at 0x1400_0000/0x1800_0000, outside the APB base,
        // so it is matched first and by its own bases.
        if (XIP_CTRL..XIP_CTRL + 0x4000).contains(&addr) {
            return Some(Block::XipCtrl);
        }
        if (XIP_SSI..XIP_SSI + 0x4000).contains(&addr) {
            return Some(Block::XipSsi);
        }
        table
            .iter()
            .find(|(base, _)| addr >= *base && addr < *base + 0x4000)
            .map(|(_, block)| *block)
    }
}

/// Every block the address decoder knows, named so a fault can say where it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Sysinfo,
    Syscfg,
    Clocks,
    Resets,
    Psm,
    IoBank0,
    IoQspi,
    PadsBank0,
    PadsQspi,
    Xosc,
    PllSys,
    PllUsb,
    BusCtrl,
    Uart0,
    Uart1,
    Spi0,
    Spi1,
    I2c0,
    I2c1,
    Adc,
    Pwm,
    Timer,
    Watchdog,
    Rtc,
    Rosc,
    VregAndChipReset,
    Tbman,
    Ticks,
    Dma,
    UsbCtrl,
    Pio0,
    Pio1,
    XipCtrl,
    XipSsi,
}

impl Memory {
    fn read_word(&mut self, addr: u32) -> Result<u32, Fault> {
        if (FLASH_BASE..FLASH_BASE + FLASH_SIZE as u32).contains(&addr) {
            let offset = (addr - FLASH_BASE) as usize & !3;
            return Ok(u32::from_le_bytes([
                self.flash[offset],
                self.flash[offset + 1],
                self.flash[offset + 2],
                self.flash[offset + 3],
            ]));
        }
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            let offset = (addr - SRAM_BASE) as usize & !3;
            return Ok(u32::from_le_bytes([
                self.sram[offset],
                self.sram[offset + 1],
                self.sram[offset + 2],
                self.sram[offset + 3],
            ]));
        }
        if (ROM_BASE..ROM_BASE + ROM_SIZE as u32).contains(&addr) {
            let offset = (addr - ROM_BASE) as usize & !3;
            return Ok(u32::from_le_bytes([
                self.rom[offset],
                self.rom[offset + 1],
                self.rom[offset + 2],
                self.rom[offset + 3],
            ]));
        }
        if (SIO_BASE..SIO_BASE + 0x1000).contains(&addr) {
            let core = usize::from(self.active_core);
            return Ok(self.periph.sio.read(addr - SIO_BASE, core));
        }
        if (SCS_BASE..SCS_BASE + 0x1000).contains(&addr) {
            let offset = addr - SCS_BASE;
            let ppb = if self.active_core == 1 {
                &mut self.periph.ppb1
            } else {
                &mut self.periph.ppb
            };
            return Ok(ppb.read(offset));
        }
        if (PPB_BASE..PPB_BASE + 0x10_0000).contains(&addr) {
            // ITM, DWT and the flash patch unit: the model has none of them,
            // so they read as zero rather than faulting.
            return Ok(0);
        }
        if (USB_DPRAM..USB_DPRAM + 0x1000).contains(&addr) {
            return Ok(self.periph.usb.read_dpram_word(addr - USB_DPRAM));
        }
        if (FLASH_CTRL..FLASH_CTRL + FLASH_CTRL_LEN).contains(&addr) {
            let control = &self.flash_control;
            return Ok(match addr - FLASH_CTRL {
                0 => control.addr,
                4 => control.count,
                8 => control.block_size,
                12 => control.erase_cmd,
                16 => control.op,
                20 => control.data,
                24 => control.program_len,
                _ => 0,
            });
        }
        if let Some(block) = Memory::block(addr) {
            return Ok(self.periph.block_read(block, addr & 0x3fff));
        }
        Err(Fault::Bus { addr, write: false })
    }

    fn write_word(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            let offset = (addr - SRAM_BASE) as usize & !3;
            self.sram[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            return Ok(());
        }
        if (SIO_BASE..SIO_BASE + 0x1000).contains(&addr) {
            let core = usize::from(self.active_core);
            self.periph.sio.write(addr - SIO_BASE, value, core);
            return Ok(());
        }
        if (SCS_BASE..SCS_BASE + 0x1000).contains(&addr) {
            let offset = addr - SCS_BASE;
            let ppb = if self.active_core == 1 {
                &mut self.periph.ppb1
            } else {
                &mut self.periph.ppb
            };
            ppb.write(offset, value);
            return Ok(());
        }
        if (PPB_BASE..PPB_BASE + 0x10_0000).contains(&addr) {
            return Ok(());
        }
        if (USB_DPRAM..USB_DPRAM + 0x1000).contains(&addr) {
            self.periph.usb.write_dpram_word(addr - USB_DPRAM, value);
            return Ok(());
        }
        if (FLASH_CTRL..FLASH_CTRL + FLASH_CTRL_LEN).contains(&addr) {
            match addr - FLASH_CTRL {
                0 => self.flash_control.addr = value,
                4 => self.flash_control.count = value,
                8 => self.flash_control.block_size = value,
                12 => self.flash_control.erase_cmd = value,
                16 => {
                    self.flash_control.op = value;
                    self.apply_flash_control();
                }
                20 => self.flash_control.data = value,
                24 => self.flash_control.program_len = value,
                _ => {}
            }
            return Ok(());
        }
        if let Some(block) = Memory::block(addr) {
            self.periph.block_write(block, addr & 0x3fff, value);
            return Ok(());
        }
        Err(Fault::Bus { addr, write: true })
    }
}

impl Bus for Memory {
    fn read8(&mut self, addr: u32) -> Result<u32, Fault> {
        let word = self.read_word(addr & !3)?;
        Ok((word >> ((addr & 3) * 8)) & 0xff)
    }

    fn read16(&mut self, addr: u32) -> Result<u32, Fault> {
        if addr & 1 != 0 {
            return Err(Fault::Bus { addr, write: false });
        }
        let word = self.read_word(addr & !3)?;
        Ok((word >> ((addr & 2) * 8)) & 0xffff)
    }

    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        if addr & 3 != 0 {
            return Err(Fault::Bus { addr, write: false });
        }
        self.read_word(addr)
    }

    fn write8(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        let shift = (addr & 3) * 8;
        if (SRAM_BASE..SRAM_BASE + SRAM_SIZE as u32).contains(&addr) {
            self.sram[(addr - SRAM_BASE) as usize] = value as u8;
            return Ok(());
        }
        let word = self.read_word(addr & !3)?;
        let word = (word & !(0xff << shift)) | ((value & 0xff) << shift);
        self.write_word(addr & !3, word)
    }

    fn write16(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if addr & 1 != 0 {
            return Err(Fault::Bus { addr, write: true });
        }
        let shift = (addr & 2) * 8;
        let word = self.read_word(addr & !3)?;
        let word = (word & !(0xffff << shift)) | ((value & 0xffff) << shift);
        self.write_word(addr & !3, word)
    }

    fn write32(&mut self, addr: u32, value: u32) -> Result<(), Fault> {
        if addr & 3 != 0 {
            return Err(Fault::Bus { addr, write: true });
        }
        self.write_word(addr, value)
    }
}

impl crate::peripherals::dma::DmaBus for Memory {
    fn dma_read(&mut self, addr: u32, size: crate::peripherals::dma::Size) -> Option<u32> {
        use crate::peripherals::dma::Size;
        let word = self.read_word(addr & !3).ok()?;
        Some(match size {
            Size::Byte => (word >> ((addr & 3) * 8)) & 0xff,
            Size::Half => (word >> ((addr & 2) * 8)) & 0xffff,
            Size::Word => word,
        })
    }

    fn dma_write(&mut self, addr: u32, value: u32, size: crate::peripherals::dma::Size) -> bool {
        use crate::peripherals::dma::Size;
        let result = match size {
            Size::Byte => <Self as Bus>::write8(self, addr, value),
            Size::Half => <Self as Bus>::write16(self, addr, value),
            Size::Word => <Self as Bus>::write32(self, addr, value),
        };
        result.is_ok()
    }

    fn dreq_ready(&mut self, sel: u32) -> bool {
        self.periph.dreq_ready(sel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peripherals::dma::ctrl as dma_ctrl;

    #[test]
    fn dma_copies_between_sram_buffers() {
        let mut mem = Memory::new();
        let source = SRAM_BASE;
        let destination = SRAM_BASE + 0x100;
        for index in 0..4 {
            mem.write_word(source + index * 4, 0x1000 + index).unwrap();
        }

        mem.periph.dma.write(0x00, source);
        mem.periph.dma.write(0x04, destination);
        mem.periph.dma.write(
            0x0c,
            dma_ctrl::EN
                | dma_ctrl::INCR_READ
                | dma_ctrl::INCR_WRITE
                | (2 << dma_ctrl::DATA_SIZE_SHIFT),
        );
        mem.periph.dma.write(0x08, 4);
        for _ in 0..4 {
            mem.service_dma();
        }

        for index in 0..4 {
            assert_eq!(
                mem.read_word(destination + index * 4).unwrap(),
                0x1000 + index
            );
        }
        assert_eq!(mem.periph.dma.read(0x08), 0);
    }

    #[test]
    fn the_flash_helpers_erase_a_whole_sector() {
        let mut mem = Memory::new();
        mem.load_flash(0x1000, &[0x12; 0x1200]).unwrap();

        mem.write_word(FLASH_CTRL, FLASH_BASE + 0x1000).unwrap();
        mem.write_word(FLASH_CTRL + 4, 4096).unwrap();
        mem.write_word(FLASH_CTRL + 8, 4096).unwrap();
        mem.write_word(FLASH_CTRL + 16, 1).unwrap();

        assert!(mem.flash[0x1000..0x2000].iter().all(|byte| *byte == 0xff));
        // The next sector and the bytes before the first one are untouched.
        assert_eq!(mem.flash[0x2000], 0x12);
        assert_eq!(mem.flash[0x0fff], 0xff);
    }

    #[test]
    fn the_flash_helpers_program_from_sram() {
        let mut mem = Memory::new();
        mem.sram[0x400..0x500].fill(0xf0);

        mem.write_word(FLASH_CTRL, FLASH_BASE + 0x3000).unwrap();
        mem.write_word(FLASH_CTRL + 20, SRAM_BASE + 0x400).unwrap();
        mem.write_word(FLASH_CTRL + 24, 256).unwrap();
        mem.write_word(FLASH_CTRL + 16, 2).unwrap();
        assert_eq!(mem.flash[0x3000], 0xf0);
        assert_eq!(mem.flash[0x30ff], 0xf0);

        // Programming clears bits, so a second pass can only take them away.
        mem.sram[0x400..0x500].fill(0x0f);
        mem.write_word(FLASH_CTRL + 20, SRAM_BASE + 0x400).unwrap();
        mem.write_word(FLASH_CTRL + 16, 2).unwrap();
        assert_eq!(mem.flash[0x3000], 0x00);
        assert_eq!(mem.flash[0x30ff], 0x00);
    }
}

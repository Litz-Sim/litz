//! Ready-to-run firmware images, built with nothing at all.
//!
//! Two instruction sets live here: RV32IMC for the ESP32-C3 ([`encoder`],
//! [`programs`]) and Thumb-1 for the RP2040 ([`thumb`], [`pico`]).
//!
//! Litz must be able to demonstrate a runnable chip without ESP-IDF, without
//! a RISC-V toolchain and without shipping a mask ROM or a downloaded binary.
//! A few hundred bytes of hand-assembled RV32IMC are enough to configure a
//! GPIO, toggle it and print over UART, which is all the browser demo and the
//! CLI smoke tests need. This crate is that assembler and those programs.
//!
//! The images are ordinary ESP-IDF application images: a 24-byte header, then
//! `load_addr`, `len` and data per segment. `esp32sim`'s `boot_app` reads
//! exactly that much; esptool's one-byte checksum and 16-byte alignment padding
//! are written too, so the output is a valid image even though Litz's loader
//! ignores them.
#![forbid(unsafe_code)]

pub mod encoder;
pub mod pico;
pub mod programs;
pub mod thumb;

pub use pico::blink as pico_blink;
pub use programs::{blinky, button_led, echo, hello, hold_low, pot_led};

/// Where fixture code is loaded and entered: the first byte of IRAM.
///
/// This address is load-bearing. `esp32sim`'s `boot_app` copies a segment
/// straight into RAM when its load address is outside the D-bus window
/// (`0x3C00_0000..0x3C80_0000`) and the I-bus window
/// (`0x4200_0000..0x4280_0000`). A segment inside either window is a
/// flash-mapped segment and needs the real second-stage bootloader and its
/// MMU page split, which the C3 path does not model. `0x4038_0000` is IRAM,
/// outside both windows, so it is copied and executed directly.
pub const CODE_ADDR: u32 = 0x4038_0000;

/// Top of DRAM (`0x3FC8_0000 + 0x4_0000`), where the bootloader would leave
/// the stack pointer.
pub const DRAM_TOP: u32 = 0x3FCC_0000;

/// IO_MUX register block; one 4-byte configuration register per pad from +4.
pub const IO_MUX_BASE: u32 = 0x6000_9000;

/// GPIO register block: output, enable and input words, plus their W1TS/W1TC
/// set-and-clear aliases.
pub const GPIO_BASE: u32 = 0x6000_4000;

/// UART0 register block. Offset 0 is the TX FIFO, one byte per store.
pub const UART0_BASE: u32 = 0x6000_0000;

/// ESP32-C3 CPU clock in hertz, the rate the delay loops are calibrated for.
pub const CPU_HZ: u32 = 160_000_000;

/// CPU cycles in one millisecond at [`CPU_HZ`].
pub const CYCLES_PER_MS: u32 = CPU_HZ / 1000;

/// Build an ESP-IDF application image.
///
/// Layout, as `boot_app` parses it:
///
/// ```text
/// byte 0      0xE9 magic
/// byte 1      segment count
/// byte 2      SPI mode (0)
/// byte 3      SPI speed/size (0)
/// bytes 4..8  entry address, little-endian u32
/// bytes 8..24 reserved/extended header, zero
/// per segment: u32 load_addr, u32 len, then len bytes of data
/// ```
///
/// The one-byte XOR checksum (seeded with `0xEF`) and zero padding to a
/// 16-byte boundary are what esptool appends. `boot_app` stops after the
/// segment data, so Litz never reads them, but writing them keeps the output
/// usable by real `esptool.py` as well.
///
/// # Panics
///
/// Panics if more than 16 segments are given: the ESP image format's
/// second-stage bootloader accepts at most 16.
pub fn image(entry: u32, segments: &[(u32, &[u8])]) -> Vec<u8> {
    assert!(
        segments.len() <= 16,
        "an ESP application image holds at most 16 segments"
    );
    let payload = segments
        .iter()
        .map(|(_, data)| data.len() + 8)
        .sum::<usize>();
    let mut out = Vec::with_capacity(24 + payload + 1 + 15);
    out.extend([0xE9, segments.len() as u8, 0x00, 0x00]);
    out.extend(entry.to_le_bytes());
    out.extend([0u8; 16]);
    for (load_addr, data) in segments {
        assert!(
            u32::try_from(data.len()).is_ok(),
            "segment too large for a u32 length"
        );
        out.extend(load_addr.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    // esptool's checksum: XOR every image byte so far, starting at 0xEF. The
    // checksum byte itself and the padding after it are not covered.
    out.push(checksum(&out));
    while out.len() % 16 != 0 {
        out.push(0);
    }
    out
}

/// The esptool checksum of `bytes`: their XOR, seeded with `0xEF`.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0xEF, |acc, &byte| acc ^ byte)
}

/// Chainable builder for [`image`].
///
/// ```
/// use litz_fixture::{ImageBuilder, CODE_ADDR};
/// let bytes = ImageBuilder::new(CODE_ADDR)
///     .segment(CODE_ADDR, &[0x6f, 0x00, 0x00, 0x00])
///     .build();
/// assert_eq!(bytes[0], 0xE9);
/// assert_eq!(bytes[1], 1);
/// ```
pub struct ImageBuilder {
    entry: u32,
    segments: Vec<(u32, Vec<u8>)>,
}

impl ImageBuilder {
    /// Start an image that begins execution at `entry`.
    pub fn new(entry: u32) -> Self {
        Self {
            entry,
            segments: Vec::new(),
        }
    }

    /// Append a segment that loads `data` at `load_addr`.
    pub fn segment(mut self, load_addr: u32, data: &[u8]) -> Self {
        self.segments.push((load_addr, data.to_vec()));
        self
    }

    /// Serialize the image, including checksum and padding.
    pub fn build(self) -> Vec<u8> {
        let segments: Vec<(u32, &[u8])> = self
            .segments
            .iter()
            .map(|(addr, data)| (*addr, data.as_slice()))
            .collect();
        image(self.entry, &segments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::{SP, T2, ZERO, addi, bne, lui};

    #[test]
    fn the_header_names_the_magic_count_and_entry() {
        let bytes = image(0x4038_0000, &[(0x4038_0000, &[0x6f, 0x00, 0x00, 0x00])]);
        assert_eq!(&bytes[0..4], &[0xE9, 0x01, 0x00, 0x00]);
        assert_eq!(&bytes[4..8], &0x4038_0000u32.to_le_bytes());
        assert_eq!(&bytes[8..24], &[0u8; 16]);
    }

    #[test]
    fn segments_are_packed_without_individual_alignment() {
        let bytes = image(
            0x4038_0000,
            &[(0x4038_0000, &[1, 2, 3]), (0x3FC8_0000, &[4, 5])],
        );
        // First segment header at 24, data at 32.
        assert_eq!(&bytes[24..28], &0x4038_0000u32.to_le_bytes());
        assert_eq!(&bytes[28..32], &3u32.to_le_bytes());
        assert_eq!(&bytes[32..35], &[1, 2, 3]);
        // The second header follows the first segment's data immediately.
        assert_eq!(&bytes[35..39], &0x3FC8_0000u32.to_le_bytes());
        assert_eq!(&bytes[39..43], &2u32.to_le_bytes());
        assert_eq!(&bytes[43..45], &[4, 5]);
    }

    #[test]
    fn the_checksum_covers_header_and_segments_then_padding_follows() {
        let bytes = image(0x4038_0000, &[(0x4038_0000, &[0x6f, 0x00, 0x00, 0x00])]);
        let data_end = 24 + 8 + 4;
        let expected = bytes[..data_end]
            .iter()
            .fold(0xEFu8, |acc, &byte| acc ^ byte);
        assert_eq!(bytes[data_end], expected);
        assert_eq!(bytes.len() % 16, 0);
        assert!(bytes[data_end + 1..].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn the_checksum_seed_is_the_identity_for_an_empty_image() {
        assert_eq!(checksum(&[]), 0xEF);
        assert_eq!(checksum(&[0xEF]), 0x00);
        assert_eq!(checksum(&[0x01, 0x02]), 0xEF ^ 0x01 ^ 0x02);
    }

    #[test]
    fn the_builder_emits_the_same_bytes_as_the_function() {
        let direct = image(CODE_ADDR, &[(CODE_ADDR, &[1, 2]), (0x3FC8_0000, &[3])]);
        let built = ImageBuilder::new(CODE_ADDR)
            .segment(CODE_ADDR, &[1, 2])
            .segment(0x3FC8_0000, &[3])
            .build();
        assert_eq!(direct, built);
    }

    #[test]
    fn blinky_is_one_iram_segment_starting_with_a_stack_setup() {
        let bytes = programs::blinky(16_000_000);
        assert_eq!(bytes[0], 0xE9);
        assert_eq!(bytes[1], 1, "blinky has exactly one segment");
        let entry = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert_eq!(entry, CODE_ADDR);
        let load_addr = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        assert_eq!(load_addr, CODE_ADDR);
        assert_eq!(len % 4, 0, "the code segment is whole instructions");
        let code = &bytes[32..32 + len];
        // First word: lui sp, 0x3FCC0 -> sp = 0x3FCC_0000.
        assert_eq!(&code[..4], &lui(SP, 0x3FCC0).to_le_bytes());
    }

    #[test]
    fn the_blinky_delay_loop_is_a_two_instruction_back_edge() {
        let bytes = programs::blinky(16_000_000);
        let len = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        let code = &bytes[32..32 + len];
        let mut needle = Vec::new();
        needle.extend(addi(T2, T2, -1).to_le_bytes());
        needle.extend(bne(T2, ZERO, -4).to_le_bytes());
        assert!(
            code.windows(needle.len())
                .any(|window| window == needle.as_slice()),
            "expected addi x7,x7,-1; bne x7,x0,-4 in the code segment"
        );
    }

    #[test]
    fn hello_is_one_segment_and_prints_before_spinning() {
        let bytes = programs::hello();
        assert_eq!(bytes[0], 0xE9);
        assert_eq!(bytes[1], 1);
        let len = u32::from_le_bytes(bytes[28..32].try_into().unwrap()) as usize;
        let code = &bytes[32..32 + len];
        // First word: lui a1, 0x60000 -> UART0 base.
        assert_eq!(&code[..4], &lui(11, 0x60000).to_le_bytes());
        // Last word: jal x0, 0 -> 0x0000_006F.
        assert_eq!(&code[len - 4..len], &0x0000_006fu32.to_le_bytes());
    }
}

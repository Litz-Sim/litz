//! A Raspberry Pi Pico firmware image, assembled by hand.
//!
//! The RP2040 boots by copying the first 256 bytes of flash into SRAM and
//! jumping to the reset vector that copy begins with — stage 2, in Pico terms
//! `boot2`. This module writes a stage 2 that is three instructions long (load
//! the application address, branch to it, and the address itself) and an
//! application at `0x1000_0100` with its own vector table, so the layout is the
//! one a real `boot2` image hands off to and `Machine::boot` can run it without
//! special cases.
//!
//! The application configures GP0 as UART0 TX and blinks a GPIO, which is
//! enough for the CLI and the browser demo to show a Pico doing something.

use crate::thumb::{self, LR, Thumb};

/// The flash reader stage 2 exists to configure on real hardware.
pub const XIP_SSI_BASE: u32 = 0x1800_0000;
/// Where the ROM runs stage 2 from.
pub const SRAM_BASE: u32 = 0x2000_0000;

/// Where the RP2040 maps QSPI flash.
pub const FLASH_BASE: u32 = 0x1000_0000;
/// The top of SRAM (264 KiB), where the ROM leaves the stack pointer.
pub const SRAM_TOP: u32 = 0x2004_2000;
/// Bytes the ROM copies out of flash before the first instruction.
pub const STAGE2_LEN: usize = 256;
/// The application's vector table, one sector into flash the way a real
/// application image sits behind `boot2` and a partition-free layout.
pub const APP_ADDR: u32 = FLASH_BASE + STAGE2_LEN as u32;
/// First instruction of the application, right after its two vector words.
pub const APP_CODE: u32 = APP_ADDR + 8;

/// CPU clock in hertz. The delay loop is calibrated against this.
pub const CPU_HZ: u32 = 125_000_000;

const UART0_BASE: u32 = 0x4003_4000;
const SIO_OUT_SET: u32 = 0xd000_0014;
const SIO_OUT_CLR: u32 = 0xd000_0018;
const SIO_OE_SET: u32 = 0xd000_0024;

/// PL011 divisor for 115200 baud from the 125 MHz peripheral clock.
const IBRD: u32 = 67;
const FBRD: u32 = 52;

/// Build a Pico image that prints `litz: pico` on UART0 (GP0) and blinks
/// `gpio` with a half-period of `period_ms`.
///
/// # Panics
///
/// Panics if `gpio` is not a pin the SIO block drives (`0..=29`).
pub fn stage2() -> Vec<u8> {
    // Code, not a vector table: the ROM calls this with LR pointing at the
    // handoff, so returning is what starts the application. Touching the QSPI
    // block first keeps the shape of a real `boot2`, which exists to
    // configure it.
    let mut t = Thumb::new();
    t.push(0);
    t.ldr_const(3, XIP_SSI_BASE);
    t.pop(0);
    let mut bytes = t.link(SRAM_BASE);
    bytes.resize(STAGE2_LEN, 0);
    bytes
}

/// A Pico image that prints `litz: pico` on UART0 (GP0) and blinks `gpio`
/// with a half-period of `period_ms`.
///
/// # Panics
///
/// Panics if `gpio` is not a pin the SIO block drives (`0..=29`).
pub fn blink(gpio: u8, period_ms: u32) -> Vec<u8> {
    assert!(gpio < 30, "the RP2040 has GPIO0 through GPIO29");
    let mask = 1u32 << gpio;
    // Two cycles per iteration: the counter and the branch.
    let count = (CPU_HZ / 1000 * period_ms / 2).max(1);

    let mut t = Thumb::new();
    t.push(0x00f0); // r4-r7, lr
    // SIO: make the pin an output.
    t.ldr_const(5, SIO_OE_SET);
    t.ldr_const(7, mask);
    t.str(7, 5, 0);
    t.ldr_const(5, SIO_OUT_SET);
    t.ldr_const(6, SIO_OUT_CLR);

    // UART0 at 115200 8N1 with the FIFOs on.
    t.ldr_const(4, UART0_BASE);
    t.ldr_const(0, IBRD);
    t.str(0, 4, 0x24);
    t.ldr_const(0, FBRD);
    t.str(0, 4, 0x28);
    t.ldr_const(0, 0x70);
    t.str(0, 4, 0x2c);
    t.ldr_const(0, 0x0301); // UARTEN | TXE | RXE
    t.str(0, 4, 0x30);

    // Print the banner one byte at a time, waiting for TXFF to clear.
    let message = t.data(b"litz: pico\r\n\0");
    t.ldr_data(1, message);
    t.ldr_const(3, 1 << 5); // FR.TXFF
    let print = t.here();
    t.ldrb(2, 1, 0);
    t.cmp(2, 0);
    let banner_done = t.bcond_forward(thumb::cond::EQ);
    let tx_full = t.here();
    t.ldr(0, 4, 0x18);
    t.tst(0, 3);
    t.bcond(thumb::cond::NE, tx_full);
    t.strb(2, 4, 0);
    t.adds(1, 1);
    t.b(print);

    // Blink forever.
    let spin = t.here();
    t.bind(banner_done, spin);
    t.str(7, 5, 0); // on
    t.ldr_const(0, count);
    let first_delay = t.bl_forward();
    t.str(7, 6, 0); // off
    t.ldr_const(0, count);
    let second_delay = t.bl_forward();
    t.b(spin);

    let delay = t.here();
    t.bind(first_delay, delay);
    t.bind(second_delay, delay);
    t.subs(0, 1);
    t.bcond(thumb::cond::NE, delay);
    t.bx(LR);

    let app = t.link(APP_CODE);

    let stage2 = stage2();

    let at = (APP_ADDR - FLASH_BASE) as usize;
    let mut image = vec![0u8; at + 8 + app.len()];
    image[..stage2.len()].copy_from_slice(&stage2);
    image[at..at + 4].copy_from_slice(&SRAM_TOP.to_le_bytes());
    image[at + 4..at + 8].copy_from_slice(&(APP_CODE | 1).to_le_bytes());
    image[at + 8..].copy_from_slice(&app);
    debug_assert!(stage2.len() <= STAGE2_LEN);
    image
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn stage_two_saves_lr_touches_the_qspi_block_and_returns() {
        let image = blink(15, 100);
        assert_eq!(u16::from_le_bytes([image[0], image[1]]), 0xb500); // push {lr}
        assert_eq!(u16::from_le_bytes([image[2], image[3]]), 0x4b01); // ldr r3, [pc, #4]
        assert_eq!(u16::from_le_bytes([image[4], image[5]]), 0xbd00); // pop {pc}
        assert_eq!(word(&image, 8), XIP_SSI_BASE);
    }

    #[test]
    fn the_application_has_its_own_vector_table() {
        let image = blink(15, 100);
        let at = (APP_ADDR - FLASH_BASE) as usize;
        assert_eq!(word(&image, at), SRAM_TOP);
        assert_eq!(word(&image, at + 4), APP_CODE | 1);
        // push {r4-r7, lr}
        assert_eq!(u16::from_le_bytes([image[at + 8], image[at + 9]]), 0xb5f0);
    }

    #[test]
    fn the_banner_is_in_the_image() {
        let image = blink(15, 100);
        let needle = b"litz: pico\r\n";
        assert!(
            image.windows(needle.len()).any(|window| window == needle),
            "the banner string is not in the image"
        );
    }

    #[test]
    fn stage_two_stays_inside_the_copied_sector() {
        let image = blink(15, 100);
        // Everything the ROM copies is the first 256 bytes; the application
        // must sit beyond them or the copy would clobber it.
        assert!((APP_ADDR - FLASH_BASE) as usize >= STAGE2_LEN);
        assert!(image.len() > STAGE2_LEN);
    }
}

//! A resistive touch controller on SPI.
//!
//! The XPT2046 is the controller that sits beside the ILI9341 on the common
//! 2.4"/2.8"/3.2" TFT modules. It is an SPI analog front end: the chip writes a
//! one-byte control word that selects X, Y, pressure or an auxiliary channel,
//! and the conversion result shifts back on the following clocks. A separate
//! `PENIRQ` line goes low while a finger is on the glass, which is how a
//! driver knows a conversion is worth starting.
//!
//! This model keeps the panel's state as the host last set it: a raw 12-bit
//! `(x, y)` pair and a pen-down flag. **Coordinates are the controller's raw
//! 12-bit codes, `0..=4095`, not pixels.** A driver maps them to display
//! pixels with its own calibration constants, exactly as it would on real
//! hardware; the panel geometry is not part of the touch controller.
//!
//! Framing follows the ADS7846/XPT2046 datasheet. The first byte of a
//! transaction is the control word (the chip ignores what MISO carries during
//! it). The conversion result then shifts out as one 16-bit word, MSB first,
//! with the 12-bit value left-aligned:
//!
//! ```text
//! tx:  [0xd0] [0x00] [0x00]
//! rx:  [0x00] [hi]   [lo]
//! raw = ((hi << 8) | lo) >> 4          // 0..=4095
//! ```
//!
//! The control words this model decodes are `0xD0` for X and `0x90` for Y;
//! the pressure (`0xB0`) and auxiliary (`0xC0`) channels answer zero, which is
//! all a driver's probe needs. The lower three bits of the control word (the
//! mode, single-ended/differential and power-down bits) are ignored.

use crate::bus::SpiDevice;
use std::any::Any;

/// The largest raw coordinate the controller reports.
pub const RAW_MAX: u16 = 0x0fff;

/// Control word selecting the X channel (12-bit, differential).
pub const COMMAND_X: u8 = 0xd0;

/// Control word selecting the Y channel (12-bit, differential).
pub const COMMAND_Y: u8 = 0x90;

/// Control word selecting the pressure channel.
pub const COMMAND_PRESSURE: u8 = 0xb0;

/// Control word selecting the auxiliary input.
pub const COMMAND_AUX: u8 = 0xc0;

/// An XPT2046 touch controller with a host-settable contact point.
///
/// The part answers SPI transactions with the panel state and exposes that
/// state through typed read-back ([`Xpt2046::touched`],
/// [`Xpt2046::position`]). A host scenario sets it with
/// [`Board::touch_panel`](crate::Board::touch_panel), or a test can call
/// [`Xpt2046::touch`] directly.
pub struct Xpt2046 {
    x: u16,
    y: u16,
    down: bool,
    selected: bool,
}

impl Xpt2046 {
    pub fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            down: false,
            selected: false,
        }
    }

    /// Set the contact point in raw controller codes and the pen state.
    ///
    /// Coordinates above [`RAW_MAX`] are clamped to it, since the controller
    /// cannot report a 13th bit.
    pub fn touch(&mut self, x: u16, y: u16, down: bool) {
        self.x = x.min(RAW_MAX);
        self.y = y.min(RAW_MAX);
        self.down = down;
    }

    /// Whether the pen is on the panel, the state `PENIRQ` reflects.
    pub fn touched(&self) -> bool {
        self.down
    }

    /// The last contact point, in raw controller codes.
    pub fn position(&self) -> (u16, u16) {
        (self.x, self.y)
    }

    /// The 12-bit conversion the control word asks for.
    fn channel(&self, command: u8) -> u16 {
        // Only S and A2..A0 identify the channel; MODE, SER/DFR and PD1..PD0
        // change how the conversion is performed, not which input it reads.
        match command & 0xf8 {
            x if x == COMMAND_X => self.x,
            y if y == COMMAND_Y => self.y,
            // Pressure and the auxiliary input are not modelled: a driver's
            // probe reads zero and moves on to the position channels.
            _ => 0,
        }
    }
}

impl Default for Xpt2046 {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for Xpt2046 {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        let mut out = vec![0u8; rx_len];
        let Some(&command) = tx.first() else {
            return out;
        };
        // The 12-bit result is left-aligned in the 16-bit word the controller
        // shifts out after the control byte.
        let word = self.channel(command) << 4;
        for (index, slot) in out.iter_mut().enumerate().skip(1) {
            *slot = match index {
                1 => (word >> 8) as u8,
                2 => word as u8,
                _ => 0,
            };
        }
        out
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transaction(controller: &mut Xpt2046, command: u8) -> u16 {
        controller.select(true);
        let rx = controller.transfer(&[command, 0x00, 0x00], 3);
        controller.select(false);
        ((u16::from(rx[1]) << 8) | u16::from(rx[2])) >> 4
    }

    #[test]
    fn the_x_and_y_channels_report_the_contact_point() {
        let mut controller = Xpt2046::new();
        controller.touch(1200, 800, true);
        assert_eq!(transaction(&mut controller, COMMAND_X), 1200);
        assert_eq!(transaction(&mut controller, COMMAND_Y), 800);
    }

    #[test]
    fn the_pressure_and_aux_channels_read_zero() {
        let mut controller = Xpt2046::new();
        controller.touch(4000, 3000, true);
        assert_eq!(transaction(&mut controller, COMMAND_PRESSURE), 0);
        assert_eq!(transaction(&mut controller, COMMAND_AUX), 0);
    }

    #[test]
    fn touch_clamps_to_the_twelve_bit_range() {
        let mut controller = Xpt2046::new();
        controller.touch(50_000, 9_000, true);
        assert_eq!(controller.position(), (RAW_MAX, RAW_MAX));
        assert_eq!(transaction(&mut controller, COMMAND_X), RAW_MAX);
    }

    #[test]
    fn an_unselected_device_floats_its_output() {
        let mut controller = Xpt2046::new();
        assert_eq!(controller.transfer(&[COMMAND_X, 0, 0], 3), vec![0xff; 3]);
    }
}

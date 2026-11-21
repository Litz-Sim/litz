//! Display capture for the ESP32-family backend.
//!
//! A CI run that fails needs a picture of what the firmware drew, not only the
//! bytes it printed. esp32sim keeps the board's framebuffer (`display`) and the
//! panel's raw memory (`gram`); this module writes whichever the board has to a
//! PNG on the host. The file write belongs here, at the edge: the simulation
//! itself never touches the filesystem.
//!
//! The machine is private to `backend`. These methods reach it through the
//! crate-private `machine_ref` accessor so every esp32sim type still stays
//! behind this crate.
#![cfg(any(
    feature = "esp32",
    feature = "esp32c3",
    feature = "esp32c6",
    feature = "esp32s3",
    feature = "esp32p4",
    feature = "esp32s31"
))]

use crate::backend::EspChip;
use esp_soc::{Soc, SocBus};
use std::path::Path;

/// A board with a framebuffer or panel memory has something worth capturing.
impl<S: Soc> EspChip<S> {
    /// Whether this board exposes either a framebuffer or TFT memory.
    ///
    /// A board with neither is not an error until a caller asks for a capture;
    /// this is the pre-check a runner uses to decide whether `--screenshot` can
    /// be honoured at all.
    pub fn has_display(&self) -> bool {
        let board = self.machine_ref().bus.board_ref();
        board.display().is_some() || board.gram().is_some()
    }

    /// Write the board's display to `path` as a PNG.
    ///
    /// The framebuffer is preferred because it is what the board model presents
    /// to a UI; a board without one but with panel memory (a GRAM-backed TFT)
    /// still gets a picture. Scale 2 keeps a small panel legible without
    /// bloating a CI artefact; esp32sim caps wide panels at 1 regardless.
    pub fn save_display(&self, path: &Path) -> Result<(), String> {
        let Some(path_text) = path.to_str() else {
            return Err(format!(
                "{}: a screenshot path must be valid UTF-8",
                path.display()
            ));
        };
        let machine = self.machine_ref();
        if machine.bus.board_ref().display().is_some() {
            return machine
                .write_tft_png(path_text, 2)
                .map_err(|e| format!("{}: {e}", path.display()));
        }
        if machine.bus.board_ref().gram().is_some() {
            return machine
                .write_gram_png(path_text)
                .map_err(|e| format!("{}: {e}", path.display()));
        }
        Err(format!(
            "{}: this board has no display or TFT memory to capture",
            path.display()
        ))
    }
}

/// The `Chip` trait's hook, which lives in the backend's trait impl and cannot
/// call the inherent method of the same name without recursing.
pub(crate) fn capture<S: Soc>(chip: &EspChip<S>, path: &Path) -> Result<(), String> {
    chip.save_display(path)
}

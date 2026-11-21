//! Booting a diagram and a set of flash images, with no project directory.
//!
//! [`runner`](crate::runner) reads a project's files; a host that already has
//! the diagram and the images in memory — the browser simulator is one — wants
//! the same construction without the file system in the way. The sequence is
//! shared so the two cannot drift: validate the diagram against the builtin
//! catalog, flatten it into a board, attach the board to the chip, place the
//! images in flash and either let the second-stage bootloader run or jump
//! straight to the application.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_esp::backend::C3;

/// The second-stage bootloader's offset in flash.
const BOOTLOADER_OFFSET: usize = 0x0;
/// The partition table's offset in flash.
const PTABLE_OFFSET: usize = 0x8000;
/// The factory application's offset in flash.
const APP_OFFSET: usize = 0x1_0000;

/// The images a boot needs. ROM and the bootloader/partition-table pair are
/// optional; the pair goes together or not at all, because the bootloader
/// looks the application up through the table.
pub struct Images<'a> {
    /// The mask ROM, or an empty slice to let the chip use its own.
    pub rom: &'a [u8],
    /// The second-stage bootloader, when the application is not booted bare.
    pub bootloader: &'a [u8],
    /// The partition table that names the application's partition.
    pub partition_table: &'a [u8],
    /// The application image; required.
    pub app: &'a [u8],
}

/// Build a board and an ESP32-C3 wired to it, then place `images` in flash.
///
/// The application is required: a chip with an empty flash has nothing to run.
/// The returned circuit is the parsed diagram, kept so part enumeration
/// reports the ids the host wrote rather than the prefixed ids the board
/// expansion invents.
pub fn boot_c3(
    diagram: &str,
    images: Images<'_>,
    flash_size: usize,
    mac: [u8; 6],
) -> Result<(C3, Circuit), String> {
    if images.app.is_empty() {
        return Err("no application image: app_len is 0".into());
    }

    let registry = Registry::builtin();
    let circuit = Circuit::from_json(diagram).map_err(|e| e.to_string())?;
    require_c3(&circuit, &registry)?;
    let flat = FlatBoard::build(&circuit, &registry).map_err(|e| e.to_string())?;
    let board = Board::new(flat).map_err(|e| e.to_string())?;

    let mut mcu = C3::new(mac, flash_size).with_board(board);
    if !images.rom.is_empty() {
        mcu.load_rom(images.rom)?;
    }

    match (
        images.bootloader.is_empty(),
        images.partition_table.is_empty(),
    ) {
        (true, true) => {
            // No bootloader: start the app the way one would.
            mcu.write_flash(APP_OFFSET, images.app)?;
            mcu.boot_app(APP_OFFSET)?;
        }
        (false, false) => {
            mcu.write_flash(BOOTLOADER_OFFSET, images.bootloader)?;
            mcu.write_flash(PTABLE_OFFSET, images.partition_table)?;
            mcu.write_flash(APP_OFFSET, images.app)?;
        }
        _ => {
            return Err("bootloader and partition table must be supplied together".into());
        }
    }

    Ok((mcu, circuit))
}

/// Reject a diagram whose board is not the chip this boot path builds.
fn require_c3(circuit: &Circuit, registry: &Registry) -> Result<(), String> {
    let board = circuit
        .parts
        .iter()
        .find(|p| {
            registry
                .get(&p.type_key)
                .and_then(|m| m.board.as_ref())
                .is_some()
        })
        .ok_or("the diagram has no development board")?;
    let info = registry
        .get(&board.type_key)
        .and_then(|m| m.board.as_ref())
        .expect("filtered above");
    let chip = info.chip.as_deref().unwrap_or("");
    if chip != "esp32c3" {
        return Err(format!(
            "chip {chip:?} is not built by this boot path; only esp32c3 is supported"
        ));
    }
    Ok(())
}

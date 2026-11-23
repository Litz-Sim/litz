//! Booting a diagram and a set of flash images, with no project directory.
//!
//! [`runner`](crate::runner) reads a project's files; a host that already has
//! the diagram and the images in memory — the browser simulator is one — wants
//! the same construction without the file system in the way. The sequence is
//! shared so the two cannot drift: validate the diagram against the builtin
//! catalog, flatten it into a board, attach the board to the chip, place the
//! images in flash and either let the second-stage bootloader run or jump
//! straight to the application.
//!
//! [`boot`] chooses the backend from the board's manifest, exactly as the
//! runner does, so every builtin chip is reachable from an in-memory host.
//! [`boot_c3`] is the older ESP32-C3-only entry point kept for callers that
//! named the chip themselves.

use std::cell::RefCell;
use std::rc::Rc;

use litz_board::{Board, Chip, FlatBoard, ImageKind};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use litz_esp::backend::{C3, C6, P4, S31};
#[cfg(not(target_family = "wasm"))]
use litz_esp::backend::{Esp32, S3};

use crate::config::{Runtime, parse_size};
use crate::image::{FirmwareLayout, detect as detect_layout};
use crate::python::{PythonRuntime, detect_bytes};

/// The second-stage bootloader's offset in flash.
const BOOTLOADER_OFFSET: usize = 0x0;
/// The partition table's offset in flash.
const PTABLE_OFFSET: usize = 0x8000;
/// The factory application's offset in flash.
const APP_OFFSET: usize = 0x1_0000;

/// Espressif's example MAC. Nothing in the simulation is MAC-specific, so a
/// fixed value keeps two runs byte-identical.
pub const DEFAULT_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56];

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

impl Images<'_> {
    /// Every image as an owned slice, so a caller can hand the boot code
    /// borrowed input and still construct the session from its own copies.
    fn owned(&self) -> OwnedImages {
        OwnedImages {
            rom: self.rom.to_vec(),
            bootloader: self.bootloader.to_vec(),
            partition_table: self.partition_table.to_vec(),
            app: self.app.to_vec(),
        }
    }
}

/// What a host can choose about a boot before the diagram is read.
#[derive(Clone, Debug)]
pub struct BootOptions {
    /// Flash size in bytes. `None` uses the board manifest's, then 4 MiB.
    pub flash_size: Option<usize>,
    /// The MAC the station reports. The default is stable so runs match.
    pub mac: [u8; 6],
    /// Solve the analog network instead of treating every net as digital.
    pub analog: bool,
    /// Catalog manifests the host supplies. A browser has no directory to
    /// load them from, so a custom part or board arrives here.
    pub manifests: Vec<ManifestText>,
}

/// One manifest the host supplies, with the board's internal schematic when
/// the manifest names one and has no directory to resolve it against.
#[derive(Clone, Debug, Default)]
pub struct ManifestText {
    pub toml: String,
    pub schematic: Option<String>,
}

impl Default for BootOptions {
    fn default() -> Self {
        Self {
            flash_size: None,
            mac: DEFAULT_MAC,
            analog: false,
            manifests: Vec::new(),
        }
    }
}

/// A booted circuit: the chip that runs it and the board it drives.
pub struct Booted {
    pub chip: Box<dyn Chip>,
    /// The same board the chip was given, kept so a host can ask about parts,
    /// probes and the display without reaching through the backend.
    pub board: Rc<RefCell<Board>>,
    /// The parsed diagram, kept so part enumeration reports the ids the host
    /// wrote rather than the prefixed ids a board expansion invents.
    pub circuit: Circuit,
}

struct OwnedImages {
    rom: Vec<u8>,
    bootloader: Vec<u8>,
    partition_table: Vec<u8>,
    app: Vec<u8>,
}

/// What a chip is given besides its own pins: the circuit's network, its
/// advertisers, and the firmware's symbol table.
pub(crate) struct Attachments<'a> {
    pub(crate) network: Option<&'a litz_net::NetworkConfig>,
    pub(crate) bluetooth: Option<&'a litz_ble::BleConfig>,
    pub(crate) elf: Option<&'a [u8]>,
    /// Bring the Bluetooth controller up even when the project asks for no
    /// beacons: a CircuitPython build initialises it on every boot.
    pub(crate) ble_controller: bool,
}

/// The builtin backend a board's `chip` name selects, with the board attached.
pub(crate) fn builtin_chip(
    name: &str,
    mac: [u8; 6],
    flash: usize,
    board: Rc<RefCell<litz_board::Board>>,
    index: u16,
    attachments: Attachments<'_>,
) -> Result<Box<dyn Chip>, String> {
    let Attachments {
        network,
        bluetooth,
        elf,
        ble_controller,
    } = attachments;
    // A radio is part of the ESP32-family adapters, not of `Chip`; the macro
    // attaches it before the machine is boxed and erased.
    macro_rules! esp {
        ($chip:expr) => {{
            let mut chip = $chip;
            if let Some(bytes) = elf {
                chip = chip.with_elf(bytes)?;
            }
            if let Some(config) = network {
                chip.attach_network(config)?;
            }
            if let Some(config) = bluetooth {
                chip.attach_ble(config)?;
            }
            // A CircuitPython build ships with Bluetooth and brings the
            // controller up on every boot. With no controller behind the
            // registers the ROM's init asserts and the interpreter never
            // reaches its own main loop, so the runtime choice carries the
            // radio with it. `attach_ble` above still decides what is on air.
            if ble_controller {
                chip.enable_full_ble(false)?;
            }
            Box::new(chip)
        }};
    }
    Ok(match name {
        "esp32" => {
            #[cfg(not(target_family = "wasm"))]
            {
                esp!(Esp32::new(mac, flash).with_shared_board(Rc::clone(&board), index))
            }
            #[cfg(target_family = "wasm")]
            {
                let _ = (mac, flash, &board, index);
                return Err(xtensa_unavailable("esp32"));
            }
        }
        "esp32c3" => esp!(C3::new(mac, flash).with_shared_board(Rc::clone(&board), index)),
        "esp32c6" => esp!(C6::new(mac, flash).with_shared_board(Rc::clone(&board), index)),
        "esp32s3" => {
            #[cfg(not(target_family = "wasm"))]
            {
                esp!(S3::new(mac, flash).with_shared_board(Rc::clone(&board), index))
            }
            #[cfg(target_family = "wasm")]
            {
                let _ = (mac, flash, &board, index);
                return Err(xtensa_unavailable("esp32s3"));
            }
        }
        "esp32p4" => esp!(P4::new(mac, flash).with_shared_board(Rc::clone(&board), index)),
        "esp32s31" => {
            esp!(S31::new(mac, flash).with_shared_board(Rc::clone(&board), index))
        }
        "atmega328p" => {
            if network.is_some() {
                return Err("atmega328p has no Wi-Fi radio; remove [sim] wifi".into());
            }
            if bluetooth.is_some() {
                return Err("atmega328p has no Bluetooth radio; remove the beacon".into());
            }
            Box::new(litz_avr::Atmega328p::new().with_shared_board(board, index))
        }
        "rp2040" => {
            if network.is_some() {
                return Err("rp2040 has no radio; remove [sim] wifi".into());
            }
            if bluetooth.is_some() {
                return Err("rp2040 has no Bluetooth radio; remove the beacon".into());
            }
            Box::new(litz_rp::Rp2040::new().with_shared_board(board, index))
        }
        other => return Err(format!("chip {other:?} is not built in")),
    })
}

/// Why an Xtensa board cannot boot in the zero-import browser build.
#[cfg(target_family = "wasm")]
fn xtensa_unavailable(name: &str) -> String {
    format!(
        "{name} runs on the Xtensa core, which executes guest blocks through a host JIT; \
         the browser module has no imports, so use the RISC-V ESP parts, the ATmega328P or \
         the RP2040, or run this diagram with the CLI"
    )
}

/// The board instance a diagram boots, with its manifest.
fn find_board<'a>(
    circuit: &Circuit,
    registry: &'a Registry,
) -> Result<&'a litz_catalog::BoardInfo, String> {
    let mut found: Vec<(&str, &litz_catalog::BoardInfo)> = Vec::new();
    for part in &circuit.parts {
        let Some(manifest) = registry.get(&part.type_key) else {
            continue;
        };
        if let Some(info) = manifest.board.as_ref() {
            found.push((part.id.as_str(), info));
        }
    }
    match found.as_slice() {
        [] => Err("the diagram has no development board".into()),
        [(_, info)] => Ok(info),
        [first, rest @ ..] => {
            let mut ids = vec![first.0];
            ids.extend(rest.iter().map(|(id, _)| *id));
            Err(format!(
                "the diagram has {} boards ({}); an in-memory session runs one",
                ids.len(),
                ids.join(", ")
            ))
        }
    }
}

/// Boot whatever chip the diagram's board names.
///
/// The images are copied before this returns, so the caller keeps ownership of
/// its buffers. A merged ESP image is recognised and written at the
/// bootloader offset, the way the runner does it; a bare application goes at
/// the chip's own application offset.
pub fn boot(diagram: &str, images: Images<'_>, options: &BootOptions) -> Result<Booted, String> {
    let images = images.owned();
    if images.app.is_empty() {
        return Err("no application image: app_len is 0".into());
    }

    let mut registry = Registry::builtin();
    for manifest in &options.manifests {
        let id = registry
            .insert_text(&manifest.toml)
            .map_err(|error| format!("catalog manifest: {error}"))?;
        if let Some(schematic) = &manifest.schematic {
            registry.set_schematic(&id, schematic.as_str());
        }
    }
    let circuit = Circuit::from_json(diagram).map_err(|e| e.to_string())?;
    let info = find_board(&circuit, &registry)?;
    let simulator = info.simulator.clone();
    let name = info.chip.as_deref().unwrap_or("esp32c3").to_string();
    let flash_size = options
        .flash_size
        .or_else(|| {
            info.flash_size
                .as_deref()
                .and_then(|text| parse_size(text).ok())
        })
        .unwrap_or(4 * 1024 * 1024);

    let flat = FlatBoard::build(&circuit, &registry).map_err(|e| e.to_string())?;
    let mut board = Board::new(flat)
        .map_err(|e| e.to_string())?
        .with_analog(options.analog);
    // Radios belong to the circuit's parts, so the chip is attached to the
    // network and the beacons the diagram already describes.
    let network = board.network().cloned();
    let bluetooth = board.bluetooth().cloned();
    if let Some(config) = network.clone() {
        board.set_network(config);
    }
    let shared = Rc::new(RefCell::new(board));

    let mut chip: Box<dyn Chip> = if let Some(simulator) = &simulator {
        #[cfg(target_arch = "wasm32")]
        {
            Box::new(litz_board::host::HostChip::open(
                &simulator.module,
                Rc::clone(&shared),
                0,
            )?)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = simulator;
            return Err(
                "this board runs a wasm guest chip, which an in-memory session does not host; \
                 install the board and run it from a project instead"
                    .into(),
            );
        }
    } else {
        let python = detect_bytes(Runtime::Native, &images.app);
        let mut chip = builtin_chip(
            &name,
            options.mac,
            flash_size,
            Rc::clone(&shared),
            0,
            Attachments {
                network: network.as_ref(),
                bluetooth: bluetooth.as_ref(),
                elf: None,
                ble_controller: python == PythonRuntime::CircuitPython,
            },
        )?;
        if info.psram {
            chip.set_psram_size(4 << 20)?;
        }
        chip
    };

    if !images.rom.is_empty() {
        chip.load_image(ImageKind::Rom, 0, &images.rom)?;
    }
    let flash = chip.info().flash;
    match (
        images.bootloader.is_empty(),
        images.partition_table.is_empty(),
    ) {
        (false, false) => {
            chip.load_image(ImageKind::Bootloader, flash.bootloader, &images.bootloader)?;
            chip.load_image(
                ImageKind::PartitionTable,
                flash.partition_table,
                &images.partition_table,
            )?;
            chip.load_image(ImageKind::Firmware, flash.app, &images.app)?;
        }
        (true, true) => {
            // A chip whose application is not at an ESP-IDF offset has no
            // merged form to detect: the whole file is the application.
            let merged = flash.app > 0
                && detect_layout(&images.app).unwrap_or(FirmwareLayout::Application)
                    == FirmwareLayout::Merged;
            if merged {
                chip.load_image(ImageKind::Bootloader, flash.bootloader, &images.app)?;
            } else {
                chip.load_image(ImageKind::Firmware, flash.app, &images.app)?;
            }
        }
        _ => {
            return Err("bootloader and partition table must be supplied together".into());
        }
    }
    chip.boot()?;

    Ok(Booted {
        chip,
        board: shared,
        circuit,
    })
}

/// Build a board and an ESP32-C3 wired to it, then place `images` in flash.
///
/// Kept for callers that know the chip is a C3; [`boot`] is the entry point
/// that reads the chip from the diagram.
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

/// One development board's images for [`boot_fleet`]. A host that runs a
/// multi-board diagram names each board's instance id and boots every chip
/// against the same flattened circuit.
pub struct BoardImage<'a> {
    /// The `diagram.json` instance id. `None` matches boards by position.
    pub id: Option<&'a str>,
    pub app: &'a [u8],
    pub rom: &'a [u8],
    pub bootloader: &'a [u8],
    pub partition_table: &'a [u8],
    pub flash_size: Option<usize>,
    pub mac: Option<[u8; 6]>,
}

/// The chips a multi-board circuit runs, with the board they share and the
/// ids of the development board instances in diagram order.
pub struct BootedFleet {
    pub chips: Vec<Box<dyn Chip>>,
    pub board: Rc<RefCell<Board>>,
    pub circuit: Circuit,
    pub ids: Vec<String>,
}

/// Every development board instance the diagram names, in part order.
fn find_boards<'a>(
    circuit: &'a Circuit,
    registry: &'a Registry,
) -> Result<Vec<(&'a str, &'a litz_catalog::BoardInfo)>, String> {
    let mut found = Vec::new();
    for part in &circuit.parts {
        let Some(manifest) = registry.get(&part.type_key) else {
            continue;
        };
        if let Some(info) = manifest.board.as_ref() {
            found.push((part.id.as_str(), info));
        }
    }
    if found.is_empty() {
        return Err("the diagram has no development board".into());
    }
    Ok(found)
}

/// Boot one chip per development board, all attached to one shared circuit.
///
/// This is the in-memory counterpart of the native runner's fleet: every board
/// gets its own images and its own index into the flattened board, so a wire
/// between two boards is an ordinary net both chips drive.
pub fn boot_fleet(
    diagram: &str,
    images: &[BoardImage<'_>],
    options: &BootOptions,
) -> Result<BootedFleet, String> {
    let mut registry = Registry::builtin();
    for manifest in &options.manifests {
        let id = registry
            .insert_text(&manifest.toml)
            .map_err(|error| format!("catalog manifest: {error}"))?;
        if let Some(schematic) = &manifest.schematic {
            registry.set_schematic(&id, schematic.as_str());
        }
    }
    let circuit = Circuit::from_json(diagram).map_err(|e| e.to_string())?;
    let boards = find_boards(&circuit, &registry)?;
    if images.is_empty() {
        return Err("no application image: app_len is 0".into());
    }
    if images.len() != boards.len() {
        return Err(format!(
            "the diagram has {} development boards but {} images were supplied",
            boards.len(),
            images.len()
        ));
    }
    let named = images.iter().any(|image| image.id.is_some());
    if named && images.iter().any(|image| image.id.is_none()) {
        return Err("boot images must either all name a board id or all omit it".into());
    }
    if named {
        for (id, _) in &boards {
            if !images.iter().any(|image| image.id == Some(*id)) {
                return Err(format!("board {id:?} has no boot image"));
            }
        }
    }

    let flat = FlatBoard::build(&circuit, &registry).map_err(|e| e.to_string())?;
    let mut board = Board::new(flat)
        .map_err(|e| e.to_string())?
        .with_analog(options.analog);
    let network = board.network().cloned();
    let bluetooth = board.bluetooth().cloned();
    if let Some(config) = network.clone() {
        board.set_network(config);
    }
    let shared = Rc::new(RefCell::new(board));

    let mut chips: Vec<Box<dyn Chip>> = Vec::with_capacity(boards.len());
    for (index, (id, info)) in boards.iter().enumerate() {
        let image = if named {
            images
                .iter()
                .find(|image| image.id == Some(*id))
                .expect("checked above")
        } else {
            &images[index]
        };
        let name = info.chip.as_deref().unwrap_or("esp32c3").to_string();
        let flash_size = image
            .flash_size
            .or_else(|| {
                info.flash_size
                    .as_deref()
                    .and_then(|text| parse_size(text).ok())
            })
            .unwrap_or(4 * 1024 * 1024);
        let mut chip: Box<dyn Chip> = if let Some(simulator) = &info.simulator {
            #[cfg(target_arch = "wasm32")]
            {
                Box::new(litz_board::host::HostChip::open(
                    &simulator.module,
                    Rc::clone(&shared),
                    index as u16,
                )?)
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = simulator;
                return Err(
                    "this board runs a wasm guest chip, which an in-memory session does not host; \
                     install the board and run it from a project instead"
                        .into(),
                );
            }
        } else {
            // A fleet boots each image independently, so each board's runtime is
            // read from its own bytes. CircuitPython initializes the Bluetooth
            // controller on boot and stalls without it, whether the session has
            // one board or several.
            let python = detect_bytes(Runtime::Native, image.app);
            let mut chip = builtin_chip(
                &name,
                image.mac.unwrap_or(options.mac),
                flash_size,
                Rc::clone(&shared),
                index as u16,
                Attachments {
                    network: network.as_ref(),
                    bluetooth: bluetooth.as_ref(),
                    elf: None,
                    ble_controller: python == PythonRuntime::CircuitPython,
                },
            )?;
            if info.psram {
                chip.set_psram_size(4 << 20)?;
            }
            chip
        };

        if !image.rom.is_empty() {
            chip.load_image(ImageKind::Rom, 0, image.rom)?;
        }
        let flash = chip.info().flash;
        match (
            image.bootloader.is_empty(),
            image.partition_table.is_empty(),
        ) {
            (false, false) => {
                chip.load_image(ImageKind::Bootloader, flash.bootloader, image.bootloader)?;
                chip.load_image(
                    ImageKind::PartitionTable,
                    flash.partition_table,
                    image.partition_table,
                )?;
                chip.load_image(ImageKind::Firmware, flash.app, image.app)?;
            }
            (true, true) => {
                if image.app.is_empty() {
                    // A wasm MCU may carry its own program.
                    chip.boot()?;
                } else {
                    let merged = flash.app > 0
                        && detect_layout(image.app).unwrap_or(FirmwareLayout::Application)
                            == FirmwareLayout::Merged;
                    if merged {
                        chip.load_image(ImageKind::Bootloader, flash.bootloader, image.app)?;
                    } else {
                        chip.load_image(ImageKind::Firmware, flash.app, image.app)?;
                    }
                    chip.boot()?;
                }
            }
            _ => {
                return Err("bootloader and partition table must be supplied together".into());
            }
        }
        chips.push(chip);
    }

    let ids = boards.iter().map(|(id, _)| (*id).to_string()).collect();
    Ok(BootedFleet {
        chips,
        board: shared,
        circuit,
        ids,
    })
}

//! A microSD card on SPI.
//!
//! Only the card's own command layer is modelled: CMD0, CMD8, the APP_CMD /
//! ACMD41 handshake, CMD16, CMD17 single-block reads and CMD24 single-block
//! writes. A firmware that speaks the SPI protocol sees a card that
//! initialises, returns the bytes of its image, and stores a block it writes
//! back into that same image.
//!
//! A card can be given a capacity and a tree of host files. `files` names a
//! directory (or a file) relative to the project; the card is formatted as a
//! FAT volume and the tree is copied on, so a firmware that mounts the card —
//! MicroPython's `machine.SDCard`, CircuitPython's `sdcardio` — finds the
//! files on its first read. `image` loads an existing image instead, and
//! `size` alone gives a blank, erased card. The formatter is deterministic:
//! the same files produce the same bytes.
//!
//! The image never comes from an arbitrary path written in a diagram. An
//! `image` or `files` attribute is accepted only as a relative path with no
//! `..`, and the resolved path is canonicalised and checked to stay inside the
//! project directory. A host that already has bytes can use
//! [`MicroSd::with_image`] and bypass the filesystem entirely, which is the
//! path the browser uses.
//!
//! Writes mutate the in-memory image only. A native runner can persist the
//! result with [`MicroSd::save_to`], or call [`MicroSd::save_changed`] at the
//! end of a run and have it do nothing when the card was never written. A
//! diagram can name a `save` path and the board writes it back on its own at
//! the end of the run.

use crate::block::{Image, parse_size};
use crate::bus::SpiDevice;
use crate::flatten::FlatPart;
use std::any::Any;
use std::collections::VecDeque;
use std::io;
use std::path::{Component, Path, PathBuf};

const BLOCK: usize = 512;
const TOKEN_START: u8 = 0xfe;
/// The idle response bit CMD0 and CMD8 report.
const R1_IDLE: u8 = 0x01;
/// A data-response token saying the card accepted the block.
const DATA_ACCEPTED: u8 = 0x05;
/// A data-response token saying the card could not store the block.
const DATA_WRITE_ERROR: u8 = 0x0b;

/// Where a CMD24 transfer is in its data phase.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WritePhase {
    /// No write is pending; bytes are commands.
    Idle,
    /// The R1 response is out; waiting for the start token.
    AwaitToken,
    /// Collecting the block's 512 data bytes.
    Data,
    /// Consuming the block's two CRC bytes.
    Crc,
}

/// The capacity a `files` attribute gets when the diagram does not say one.
pub const DEFAULT_SD_SIZE: usize = 32 * 1024 * 1024;

/// A microSD card backed by a byte image.
pub struct MicroSd {
    image: Image,
    /// Where the board writes the image at the end of a run, when the diagram
    /// asked for it.
    save: Option<PathBuf>,
    /// The six bytes of the command currently being shifted in.
    command: Vec<u8>,
    response: VecDeque<u8>,
    idle: bool,
    app_cmd: bool,
    selected: bool,
    commands: u32,
    /// The CMD24 data-phase state.
    write_phase: WritePhase,
    /// Block address accepted by CMD24, in 512-byte units.
    write_block: u32,
    /// The block being collected; exactly 512 bytes once complete.
    write_data: Vec<u8>,
    /// CRC bytes consumed so far in [`WritePhase::Crc`].
    write_crc: u8,
    /// Blocks stored since construction or the last image replacement.
    written_blocks: u64,
    /// Whether blocks were written since construction, last save or reset.
    dirty: bool,
}

impl MicroSd {
    pub fn new() -> Self {
        Self::with_image(Vec::new())
    }

    pub fn with_image(image: Vec<u8>) -> Self {
        Self::with_bytes(Image::from_bytes(image))
    }

    /// A card of `capacity` erased bytes: it initialises, reads back as 0xFF
    /// and has no filesystem until the firmware formats one.
    pub fn blank(capacity: usize) -> Self {
        Self::with_bytes(Image::blank(capacity, 0xff))
    }

    fn with_bytes(image: Image) -> Self {
        Self {
            image,
            save: None,
            command: Vec::new(),
            response: VecDeque::new(),
            idle: true,
            app_cmd: false,
            selected: false,
            commands: 0,
            write_phase: WritePhase::Idle,
            write_block: 0,
            write_data: Vec::with_capacity(BLOCK),
            write_crc: 0,
            written_blocks: 0,
            dirty: false,
        }
    }

    /// Build a card from a manifest.
    ///
    /// `image` loads bytes, `files` formats a FAT volume and copies a project
    /// tree onto it, and `size` sets the capacity (a blank card when nothing
    /// else is given). `save` names the file the board writes back at the end
    /// of a run when the guest stored a block.
    pub fn from_attrs(part: &FlatPart) -> Result<Self, String> {
        let image_text = attr(part, "image");
        let files_text = attr(part, "files");
        let size_text = attr(part, "size");
        if !image_text.is_empty() && !files_text.is_empty() {
            return Err("`image` and `files` are exclusive: load an image or build one".into());
        }
        let size = if size_text.is_empty() {
            None
        } else {
            Some(parse_size(size_text).map_err(|error| format!("size {size_text:?}: {error}"))?)
        };
        let mut card = if !image_text.is_empty() {
            let bytes = read_image(&relative_image_path(image_text)?)
                .map_err(|error| format!("image {image_text:?}: {error}"))?;
            match size {
                Some(capacity) => {
                    if bytes.len() > capacity {
                        return Err(format!(
                            "image {image_text:?} is {} bytes, larger than the {capacity}-byte card",
                            bytes.len()
                        ));
                    }
                    let mut image = Image::blank(capacity, 0);
                    image.write_at(0, &bytes);
                    Self::with_bytes(image)
                }
                None => Self::with_image(bytes),
            }
        } else if !files_text.is_empty() {
            let capacity = size.unwrap_or(DEFAULT_SD_SIZE);
            if capacity % crate::block::SECTOR != 0 {
                return Err(format!(
                    "size {capacity} is not a whole number of {} byte sectors",
                    crate::block::SECTOR
                ));
            }
            let tree = crate::fat::read_tree(Path::new(files_text))
                .map_err(|error| format!("files {files_text:?}: {error}"))?;
            let files: Vec<crate::fat::File<'_>> = tree
                .iter()
                .map(|(path, data)| crate::fat::File {
                    path,
                    data: data.as_slice(),
                })
                .collect();
            let mut image = Image::blank(capacity, 0);
            crate::fat::format(&mut image, &files)
                .map_err(|error| format!("files {files_text:?}: {error}"))?;
            Self::with_bytes(image)
        } else {
            match size {
                Some(capacity) => Self::blank(capacity),
                None => Self::new(),
            }
        };
        if let Some(text) = part.attrs.get("save").map(String::as_str).map(str::trim)
            && !text.is_empty()
        {
            card.save = Some(relative_image_path(text)?);
        }
        Ok(card)
    }

    /// The bytes on the card, materialized. A large card's untouched sectors
    /// are expanded to the fill byte, so this is for a report or a save, not
    /// for the hot path.
    pub fn image(&self) -> Vec<u8> {
        self.image.to_vec()
    }

    /// The card's capacity in bytes, which is what a `size` attribute sets.
    pub fn capacity(&self) -> usize {
        self.image.capacity()
    }

    /// The file the diagram asked the board to write at the end of a run.
    pub fn save_target(&self) -> Option<&Path> {
        self.save.as_deref()
    }

    /// Replace the card contents. The write counters and the dirty flag reset:
    /// the new image is the card's baseline.
    pub fn set_image(&mut self, image: Vec<u8>) {
        self.image = Image::from_bytes(image);
        self.write_phase = WritePhase::Idle;
        self.write_data.clear();
        self.write_crc = 0;
        self.written_blocks = 0;
        self.dirty = false;
    }

    /// Whether blocks were written since construction or the last save.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Blocks stored since construction or the last image replacement.
    pub fn written_blocks(&self) -> u64 {
        self.written_blocks
    }

    /// Take the number of blocks written since the previous call and reset the
    /// counter. The dirty flag is unaffected; [`MicroSd::save_changed`] still
    /// sees a pending change.
    pub fn take_written_blocks(&mut self) -> u64 {
        std::mem::take(&mut self.written_blocks)
    }

    /// Write the full card image to `path`: the same bytes a reader of a real
    /// card would see, including whatever boot sector or partition table the
    /// image carries.
    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        self.image.save(path)
    }

    /// Save the image only when a block was written. Returns `true` when a
    /// file was written and `false` when the card is clean, so a runner can
    /// call this unconditionally at the end of a run.
    pub fn save_changed(&mut self, path: &Path) -> io::Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        self.save_to(path)?;
        self.dirty = false;
        Ok(true)
    }

    /// How many commands the card has answered, for a diagnostic.
    pub fn commands(&self) -> u32 {
        self.commands
    }

    pub fn idle(&self) -> bool {
        self.idle
    }

    fn execute(&mut self) {
        let command = self.command[0] & 0x3f;
        let argument = u32::from_be_bytes([
            self.command[1],
            self.command[2],
            self.command[3],
            self.command[4],
        ]);
        self.commands += 1;
        let was_app = self.app_cmd;
        self.app_cmd = false;
        match command {
            0 => {
                self.idle = true;
                self.response.push_back(R1_IDLE);
            }
            8 => {
                self.idle = true;
                self.response.extend([R1_IDLE, 0x00, 0x00, 0x01, 0xaa]);
                self.app_cmd = false;
            }
            55 => {
                self.app_cmd = true;
                self.response
                    .push_back(if self.idle { R1_IDLE } else { 0x00 });
            }
            41 if was_app => {
                self.idle = false;
                self.response.push_back(0x00);
            }
            58 => {
                self.idle = false;
                self.response.extend([0x00, 0x00, 0xff, 0x80, 0x00]);
            }
            16 => self.response.push_back(0x00),
            17 => {
                let offset = argument as usize * BLOCK;
                if let Some(block) = self.image.read_at(offset, BLOCK) {
                    self.response.push_back(0x00);
                    self.response.push_back(TOKEN_START);
                    self.response.extend(block);
                    self.response.extend([0xff, 0xff]);
                } else {
                    // Parameter error: the block is outside the card.
                    self.response.push_back(0x40);
                }
            }
            24 => {
                // The write is accepted only for a block the card has room
                // for; the data itself arrives after this command.
                let end = (argument as usize)
                    .checked_mul(BLOCK)
                    .and_then(|offset| offset.checked_add(BLOCK));
                if end.is_some_and(|end| end <= self.image.capacity()) {
                    self.write_block = argument;
                    self.write_data.clear();
                    self.write_crc = 0;
                    self.write_phase = WritePhase::AwaitToken;
                    self.response.push_back(0x00);
                } else {
                    self.response.push_back(0x40);
                }
            }
            _ => self.response.push_back(0x04),
        }
    }

    /// One byte on the wire while a CMD24 data phase is pending.
    fn write_byte(&mut self, byte: u8) {
        match self.write_phase {
            WritePhase::AwaitToken => {
                if byte == TOKEN_START {
                    self.write_data.clear();
                    self.write_phase = WritePhase::Data;
                } else if byte & 0xc0 == 0x40 {
                    // A fresh command abandons the pending write rather than
                    // wedging the card on a token that never comes.
                    self.write_phase = WritePhase::Idle;
                    self.command.clear();
                    self.command.push(byte);
                }
            }
            WritePhase::Data => {
                self.write_data.push(byte);
                if self.write_data.len() == BLOCK {
                    self.write_phase = WritePhase::Crc;
                    self.write_crc = 0;
                }
            }
            WritePhase::Crc => {
                // The CRC is consumed but not checked: SPI mode leaves data
                // CRC checking off, and a firmware may send dummy bytes.
                self.write_crc += 1;
                if self.write_crc == 2 {
                    self.store_block();
                }
            }
            WritePhase::Idle => {}
        }
    }

    /// Commit the collected block and answer with a data-response token.
    fn store_block(&mut self) {
        let offset = (self.write_block as usize).checked_mul(BLOCK);
        let stored = offset.filter(|offset| {
            offset
                .checked_add(BLOCK)
                .is_some_and(|end| end <= self.image.capacity())
        });
        if let Some(offset) = stored {
            self.image.write_at(offset, &self.write_data[..BLOCK]);
            self.written_blocks += 1;
            self.dirty = true;
            // The transfer loop pops a response for the byte it just handled,
            // so the leading 0xff sits on the second CRC byte and the token is
            // shifted out on the next byte, as a real card does.
            self.response.extend([0xff, DATA_ACCEPTED, 0x00, 0xff]);
        } else {
            self.response.extend([0xff, DATA_WRITE_ERROR, 0x00, 0xff]);
        }
        self.write_phase = WritePhase::Idle;
        self.write_data.clear();
        self.write_crc = 0;
    }
}

impl Default for MicroSd {
    fn default() -> Self {
        Self::new()
    }
}

impl SpiDevice for MicroSd {
    fn select(&mut self, selected: bool) {
        self.selected = selected;
        if !selected {
            self.command.clear();
            self.response.clear();
            // `app_cmd` is card state, not transaction state: CMD55 and the
            // ACMD that follows it are normally separate chip-select windows.
        }
    }

    fn transfer(&mut self, tx: &[u8], rx_len: usize) -> Vec<u8> {
        if !self.selected {
            return vec![0xff; rx_len];
        }
        let mut out = Vec::with_capacity(rx_len.max(tx.len()));
        for &byte in tx {
            if self.write_phase == WritePhase::Idle {
                if self.command.is_empty() {
                    // A command starts with 01xxxxxx; 0xff dummy clocks and
                    // data bytes with other high bits are ignored.
                    if byte & 0xc0 == 0x40 {
                        self.command.push(byte);
                    }
                } else {
                    self.command.push(byte);
                    if self.command.len() == 6 {
                        self.execute();
                        self.command.clear();
                    }
                }
            } else {
                self.write_byte(byte);
            }
            out.push(self.response.pop_front().unwrap_or(0xff));
        }
        out.resize(rx_len, 0xff);
        out.truncate(rx_len);
        out
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

/// One attribute as text, trimmed; an undeclared attribute reads as empty.
pub(crate) fn attr<'a>(part: &'a FlatPart, name: &str) -> &'a str {
    part.attrs
        .get(name)
        .map(String::as_str)
        .unwrap_or("")
        .trim()
}

/// Validate that a manifest's image path is relative and cannot climb out of
/// its directory. Symlinks are handled later, after canonicalisation.
pub(crate) fn relative_image_path(text: &str) -> Result<PathBuf, String> {
    if text.trim().is_empty() {
        return Err("the image path is empty".into());
    }
    let path = Path::new(text);
    if path.is_absolute() {
        return Err("the image path must be relative, not absolute".into());
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.is_empty() => {}
            Component::CurDir => {}
            _ => {
                return Err(
                    "the image path must stay inside the project directory (no `..`, root or prefix)"
                        .into(),
                );
            }
        }
    }
    Ok(path.to_path_buf())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_image(relative: &Path) -> Result<Vec<u8>, String> {
    let base = std::env::current_dir().map_err(|error| format!("no current directory: {error}"))?;
    let base = base
        .canonicalize()
        .map_err(|error| format!("the project directory is not readable: {error}"))?;
    let candidate = base.join(relative);
    let candidate = candidate
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", candidate.display()))?;
    if !candidate.starts_with(&base) {
        return Err("the image resolves outside the project directory".into());
    }
    if !candidate.is_file() {
        return Err(format!("{} is not a file", candidate.display()));
    }
    std::fs::read(&candidate)
        .map_err(|error| format!("cannot read {}: {error}", candidate.display()))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn read_image(_relative: &Path) -> Result<Vec<u8>, String> {
    // A browser build has no project directory to read from; the host sets the
    // card's bytes through the model instead.
    Ok(Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(image: Vec<u8>) -> MicroSd {
        MicroSd::with_image(image)
    }

    fn exchange(card: &mut MicroSd, tx: &[u8]) -> Vec<u8> {
        card.select(true);
        let out = card.transfer(tx, tx.len());
        card.select(false);
        out
    }

    /// Send one command and return the bytes the card shifted out after the
    /// command (its first response byte arrives on the command's last byte).
    fn command(card: &mut MicroSd, index: u8, argument: u32, padding: usize) -> Vec<u8> {
        let mut tx = Vec::new();
        tx.extend_from_slice(&[0xff, 0x40 | index]);
        tx.extend_from_slice(&argument.to_be_bytes());
        tx.push(0x95); // a CRC the model does not check
        tx.extend(std::iter::repeat_n(0xff, padding));
        let reply = exchange(card, &tx);
        reply[6..].to_vec()
    }

    #[test]
    fn a_card_initialises_from_cmd0_and_cmd8() {
        let mut card = card(Vec::new());
        assert_eq!(command(&mut card, 0, 0, 1)[0], R1_IDLE);
        let reply = command(&mut card, 8, 0x1aa, 4);
        assert_eq!(&reply[..5], &[0x01, 0x00, 0x00, 0x01, 0xaa]);
        // The APP_CMD / ACMD41 handshake leaves the card ready.
        command(&mut card, 55, 0, 1);
        assert_eq!(command(&mut card, 41, 0, 1)[0], 0x00);
        assert!(!card.idle());
    }

    #[test]
    fn cmd17_reads_one_block_from_the_image() {
        let mut image = vec![0u8; 1024];
        image[0] = b'L';
        image[1] = b'i';
        image[2] = b't';
        image[3] = b'z';
        let mut card = card(image);
        let reply = command(&mut card, 17, 0, BLOCK + 4);
        assert_eq!(reply[0], 0x00, "R1 accepted");
        assert_eq!(reply[1], TOKEN_START);
        assert_eq!(&reply[2..6], b"Litz");

        let reply = command(&mut card, 17, 1, BLOCK + 4);
        assert_eq!(reply[0], 0x00);
        assert_eq!(reply[1], TOKEN_START);
        assert_eq!(reply[2], 0x00, "the second block is blank");
    }

    #[test]
    fn a_block_outside_the_image_is_refused() {
        let mut card = card(vec![0u8; 512]);
        let reply = command(&mut card, 17, 7, 2);
        assert_eq!(reply[0], 0x40, "parameter error");
    }

    #[test]
    fn an_image_path_may_not_escape_the_project_directory() {
        for bad in ["../card.img", "/etc/passwd", "a/../../card.img", "..", ""] {
            assert!(relative_image_path(bad).is_err(), "{bad:?}");
        }
        assert!(relative_image_path("card.img").is_ok());
        assert!(relative_image_path("data/card.img").is_ok());
    }

    #[test]
    fn a_blank_card_still_answers_commands() {
        let card = MicroSd::new();
        assert!(card.image().is_empty());
        assert_eq!(card.commands(), 0);
    }

    fn part(attrs: &[(&str, &str)]) -> FlatPart {
        FlatPart {
            id: "card".into(),
            type_key: "litz-microsd-card".into(),
            engine: crate::flatten::Engine::Builtin("microsd".into()),
            pins: Vec::new(),
            pin_kinds: Vec::new(),
            attrs: attrs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect(),
            buses: Vec::new(),
            device: None,
        }
    }

    /// A scratch path under the crate's `build/`, which the repository
    /// ignores, so a failed test leaves nothing in the tree.
    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new("build").join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_size_attribute_gives_a_blank_card() {
        let card = MicroSd::from_attrs(&part(&[("size", "1MB")])).unwrap();
        assert_eq!(card.capacity(), 1 << 20);
        assert_eq!(card.image().len(), 1 << 20);
        assert_eq!(card.image()[0], 0xff, "an unwritten card is erased");
        let reply = command(&mut { card }, 17, 0, BLOCK + 4);
        assert_eq!(reply[0], 0x00, "a sized card has blocks to read");
    }

    #[test]
    fn files_are_formatted_onto_the_card() {
        let dir = scratch("sd-files");
        std::fs::write(dir.join("main.py"), b"print('hi')\n").unwrap();
        let attrs = [("files", dir.to_str().unwrap()), ("size", "1MB")];
        let card = MicroSd::from_attrs(&part(&attrs)).unwrap();
        assert_eq!(card.capacity(), 1 << 20);
        let image = crate::block::Image::from_bytes(card.image());
        let files = crate::fat::read_files(&image).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "main.py");
        assert_eq!(files[0].1, b"print('hi')\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_image_can_sit_on_a_larger_card() {
        let dir = scratch("sd-image");
        let image = dir.join("card.img");
        std::fs::write(&image, b"LITZ").unwrap();
        let attrs = [("image", image.to_str().unwrap()), ("size", "1MB")];
        let card = MicroSd::from_attrs(&part(&attrs)).unwrap();
        assert_eq!(card.capacity(), 1 << 20);
        assert_eq!(&card.image()[..4], b"LITZ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_path_is_kept_for_the_board() {
        let card = MicroSd::from_attrs(&part(&[("save", "state/card.img")])).unwrap();
        assert_eq!(card.save_target(), Some(Path::new("state/card.img")));
    }

    #[test]
    fn image_and_files_are_exclusive() {
        let error = MicroSd::from_attrs(&part(&[("image", "a.img"), ("files", "dir")]))
            .err()
            .expect("one source of contents");
        assert!(error.contains("exclusive"), "{error}");
    }
}

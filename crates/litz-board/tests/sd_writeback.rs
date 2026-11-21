//! The SPI microSD card's writeback path.
//!
//! Each test drives the card through the same byte-level handshake firmware
//! uses: CMD0, CMD8, CMD55 / ACMD41 to leave idle, CMD16 for the block length,
//! then CMD24 with a start token, 512 data bytes and two CRC bytes.

use litz_board::{MicroSd, SpiDevice};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const BLOCK: usize = 512;
const TOKEN_START: u8 = 0xfe;

/// One chip-select window.
fn exchange(card: &mut MicroSd, tx: &[u8]) -> Vec<u8> {
    card.select(true);
    let out = card.transfer(tx, tx.len());
    card.select(false);
    out
}

/// Send one command and return what the card shifted out after it. The first
/// command byte is at index 1 after a dummy clock, so R1 is at index 6.
fn command(card: &mut MicroSd, index: u8, argument: u32, padding: usize) -> Vec<u8> {
    let mut tx = Vec::with_capacity(7 + padding);
    tx.push(0xff);
    tx.push(0x40 | index);
    tx.extend_from_slice(&argument.to_be_bytes());
    tx.push(0x95); // a CRC the model does not check
    tx.extend(std::iter::repeat_n(0xff, padding));
    exchange(card, &tx)[6..].to_vec()
}

/// The handshake firmware runs before it touches a card.
fn initialise(card: &mut MicroSd) {
    assert_eq!(command(card, 0, 0, 1)[0], 0x01, "CMD0 enters idle");
    let reply = command(card, 8, 0x1aa, 4);
    assert_eq!(
        &reply[..5],
        &[0x01, 0x00, 0x00, 0x01, 0xaa],
        "CMD8 echoes the voltage"
    );
    command(card, 55, 0, 1);
    assert_eq!(command(card, 41, 0x4000_0000, 1)[0], 0x00, "ACMD41 ready");
    assert_eq!(
        command(card, 16, BLOCK as u32, 1)[0],
        0x00,
        "CMD16 selected"
    );
}

/// CMD24 and its data phase in one chip-select window, as a sector write.
///
/// Returned indices: R1 at 6, the data-response token at 522 (one byte after
/// the two CRC bytes), then busy 0x00 and the ready 0xff.
fn write_block(card: &mut MicroSd, block: u32, data: &[u8]) -> Vec<u8> {
    assert_eq!(data.len(), BLOCK);
    let mut tx = Vec::with_capacity(1 + 6 + 1 + BLOCK + 2 + 4);
    tx.push(0xff);
    tx.push(0x40 | 24);
    tx.extend_from_slice(&block.to_be_bytes());
    tx.push(0x95);
    tx.push(TOKEN_START);
    tx.extend_from_slice(data);
    tx.extend([0xff, 0xff]); // CRC, accepted but not checked in SPI mode
    tx.extend([0xff; 4]); // room for the token, busy and ready bytes
    exchange(card, &tx)
}

fn read_block(card: &mut MicroSd, block: u32) -> Vec<u8> {
    let reply = command(card, 17, block, BLOCK + 4);
    assert_eq!(reply[0], 0x00, "CMD17 accepted");
    assert_eq!(reply[1], TOKEN_START, "start token");
    reply[2..2 + BLOCK].to_vec()
}

/// A card with a boot-sector signature, so tests can see that writes and saves
/// preserve the parts of the image they never touch.
fn card_image() -> Vec<u8> {
    let mut image = vec![0u8; 4 * BLOCK];
    image[..4].copy_from_slice(b"LITZ");
    image[510] = 0x55;
    image[511] = 0xaa;
    image
}

fn block_three() -> Vec<u8> {
    let mut data = vec![0x5a_u8; BLOCK];
    data[..5].copy_from_slice(b"hello");
    data[BLOCK - 1] = 0xa5;
    data
}

static NEXT_SCRATCH: AtomicU32 = AtomicU32::new(0);

/// A uniquely named file under the repository's `build/` directory.
fn scratch_path(tag: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build");
    std::fs::create_dir_all(&dir).expect("create build/");
    let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(
        "sd-writeback-{tag}-{}-{serial}.img",
        std::process::id()
    ))
}

#[test]
fn a_cmd24_block_is_stored_and_read_back() {
    let mut card = MicroSd::with_image(card_image());
    initialise(&mut card);
    let data = block_three();

    let reply = write_block(&mut card, 3, &data);
    assert_eq!(reply[6], 0x00, "CMD24 accepted");
    assert_eq!(reply[522], 0x05, "data accepted token");
    assert_eq!(reply[523], 0x00, "card busy");
    assert_eq!(reply[524], 0xff, "card ready");

    let start = 3 * BLOCK;
    assert_eq!(
        &card.image()[start..start + BLOCK],
        &data[..],
        "the block is stored in the image"
    );
    assert_ne!(
        card.image()[start..start + BLOCK],
        vec![0u8; BLOCK],
        "the bytes really changed from the blank block"
    );
    // Bytes outside the written block are untouched.
    assert_eq!(&card.image()[..4], b"LITZ");
    assert_eq!(&card.image()[510..512], &[0x55, 0xaa]);

    assert_eq!(read_block(&mut card, 3), data, "CMD17 reads it back");

    assert!(card.dirty(), "a write marks the card dirty");
    assert_eq!(card.written_blocks(), 1);
    assert_eq!(card.take_written_blocks(), 1, "one block written");
    assert_eq!(card.take_written_blocks(), 0, "the counter drained");
    assert!(card.dirty(), "draining the counter does not clean the card");
}

#[test]
fn cmd24_outside_the_image_is_refused() {
    let mut card = MicroSd::with_image(vec![0u8; BLOCK]);
    initialise(&mut card);
    let before = card.image().to_vec();

    let reply = write_block(&mut card, 1, &block_three());
    assert_eq!(reply[6], 0x40, "parameter error, as CMD17 reports it");

    assert_eq!(card.image(), &before[..]);
    assert_eq!(card.written_blocks(), 0);
    assert!(!card.dirty());
}

#[test]
fn save_to_writes_the_whole_card_image() {
    let mut card = MicroSd::with_image(card_image());
    initialise(&mut card);
    let data = block_three();
    let reply = write_block(&mut card, 3, &data);
    assert_eq!(reply[522], 0x05);

    let path = scratch_path("save");
    card.save_to(&path).expect("save the image");
    let on_disk = std::fs::read(&path).expect("read the saved image");
    assert_eq!(
        on_disk,
        card.image(),
        "the file holds the same bytes a reader of the card would see"
    );
    assert_eq!(&on_disk[3 * BLOCK..3 * BLOCK + 5], b"hello");
    assert_eq!(&on_disk[510..512], &[0x55, 0xaa], "boot signature kept");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn save_changed_does_nothing_until_a_block_is_written() {
    let path = scratch_path("changed");
    let mut card = MicroSd::with_image(card_image());
    initialise(&mut card);

    // Only reads happened: no file appears and the call reports false.
    let _ = read_block(&mut card, 0);
    assert!(!card.save_changed(&path).expect("a clean save is ok"));
    assert!(!path.exists(), "an unchanged card writes no file");

    let data = block_three();
    let reply = write_block(&mut card, 3, &data);
    assert_eq!(reply[522], 0x05);

    assert!(card.save_changed(&path).expect("a dirty save is ok"));
    let on_disk = std::fs::read(&path).expect("read the saved image");
    assert_eq!(on_disk, card.image());

    assert!(
        !card.save_changed(&path).expect("a second save is ok"),
        "nothing changed since the first save"
    );
    assert_eq!(
        std::fs::read(&path).expect("read the saved image"),
        card.image()
    );

    let _ = std::fs::remove_file(&path);
}

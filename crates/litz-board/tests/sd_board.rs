//! A microSD card wired to a board, and the host's export of what was written.
//!
//! The card tests drive the model directly; this one goes through the board's
//! SPI bus, the same path a chip's controller takes, and then has the board
//! write the resulting image to a file.

use litz_board::{Board, FlatBoard, MicroSd};
use litz_catalog::Registry;
use litz_diagram::Circuit;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const BLOCK: usize = 512;

const DIAGRAM: &str = r#"{
  "version": 1,
  "parts": [
    {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
    {"id": "card", "type": "litz-microsd-card", "attrs": {}}
  ],
  "connections": [
    ["esp:GPIO2", "card:SCK", "", []],
    ["esp:GPIO3", "card:DI", "", []],
    ["esp:GPIO4", "card:DO", "", []],
    ["esp:GPIO5", "card:CS", "", []],
    ["esp:3V3", "card:VCC", "", []],
    ["esp:GND.1", "card:GND", "", []]
  ]
}"#;

fn board() -> Board {
    let registry = Registry::builtin();
    let circuit = Circuit::from_json(DIAGRAM).unwrap();
    let flat = FlatBoard::build(&circuit, &registry).unwrap();
    Board::new(flat).unwrap()
}

/// The controller and chip select the card answers on. A chip select is the
/// GPIO the board wired to CS, not an index, so it is looked up.
fn card_select(board: &Board) -> (u8, u8) {
    for (&(_, controller), spi) in &board.buses().spi {
        for select in spi.chip_selects() {
            if spi.device_as::<MicroSd>(select).is_some() {
                return (controller, select);
            }
        }
    }
    panic!("the board has no microSD card");
}

fn exchange(board: &mut Board, tx: &[u8]) -> Vec<u8> {
    let (controller, select) = card_select(board);
    board.spi_transfer(0, controller, select, tx, tx.len())
}

fn command(board: &mut Board, index: u8, argument: u32, padding: usize) -> Vec<u8> {
    let mut tx = vec![0xff, 0x40 | index];
    tx.extend_from_slice(&argument.to_be_bytes());
    tx.push(0x95);
    tx.extend(std::iter::repeat_n(0xff, padding));
    exchange(board, &tx)[6..].to_vec()
}

fn initialise(board: &mut Board) {
    assert_eq!(command(board, 0, 0, 1)[0], 0x01);
    command(board, 8, 0x1aa, 4);
    command(board, 55, 0, 1);
    assert_eq!(command(board, 41, 0x4000_0000, 1)[0], 0x00);
    assert_eq!(command(board, 16, BLOCK as u32, 1)[0], 0x00);
}

fn write_block(board: &mut Board, block: u32, data: &[u8]) {
    let mut tx = vec![0xff, 0x40 | 24];
    tx.extend_from_slice(&block.to_be_bytes());
    tx.push(0x95);
    tx.push(0xfe);
    tx.extend_from_slice(data);
    tx.extend([0xff, 0xff]);
    tx.extend([0xff; 4]);
    let reply = exchange(board, &tx);
    assert_eq!(reply[6], 0x00, "CMD24 accepted");
    assert_eq!(reply[522], 0x05, "data accepted");
}

static NEXT: AtomicU32 = AtomicU32::new(0);

fn scratch_path() -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build");
    std::fs::create_dir_all(&dir).unwrap();
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("sd-board-{}-{serial}.img", std::process::id()))
}

#[test]
fn a_written_block_reaches_the_exported_image_through_the_board() {
    let mut board = board();
    let mut image = vec![0u8; 4 * BLOCK];
    image[..4].copy_from_slice(b"LITZ");
    {
        let (controller, select) = card_select(&board);
        let spi = board.buses_mut().spi_mut(0, controller);
        let card = spi
            .device_as_mut::<MicroSd>(select)
            .expect("the card is attached");
        card.set_image(image);
    }
    initialise(&mut board);
    let data = vec![0xa5u8; BLOCK];
    write_block(&mut board, 3, &data);

    let path = scratch_path();
    let saved = board.save_sd_images(&path).unwrap();
    assert_eq!(saved, vec![path.display().to_string()]);

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), 4 * BLOCK);
    assert_eq!(&bytes[..4], b"LITZ", "untouched sectors survive");
    assert_eq!(&bytes[3 * BLOCK..4 * BLOCK], &data[..]);
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_board_that_wrote_nothing_writes_no_file() {
    let mut board = board();
    let mut image = vec![0u8; 4 * BLOCK];
    image[510] = 0x55;
    image[511] = 0xaa;
    {
        let (controller, select) = card_select(&board);
        let spi = board.buses_mut().spi_mut(0, controller);
        spi.device_as_mut::<MicroSd>(select)
            .expect("the card is attached")
            .set_image(image);
    }
    initialise(&mut board);
    let path = scratch_path();
    let saved = board.save_sd_images(&path).unwrap();
    assert!(saved.is_empty(), "a clean card creates no file");
    assert!(!path.exists());
}

#[test]
fn a_board_without_a_card_says_so() {
    let json = r#"{"version":1,"parts":[{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}],"connections":[]}"#;
    let circuit = Circuit::from_json(json).unwrap();
    let mut board = Board::new(FlatBoard::build(&circuit, &Registry::builtin()).unwrap()).unwrap();
    let error = board.save_sd_images(Path::new("unused.img")).unwrap_err();
    assert!(error.contains("no SPI microSD card"), "{error}");
}

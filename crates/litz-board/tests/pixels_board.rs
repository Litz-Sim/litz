//! Board-level screenshots: a diagram id reaches the display model, and the
//! copied pixels encode and compare like any other framebuffer.

use litz_board::{Board, FlatBoard};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;

fn circuit(parts: &[&str], connections: &[&str]) -> Circuit {
    let json = [
        r#"{"version":1,"parts":["#,
        &parts.join(","),
        r#"],"connections":["#,
        &connections.join(","),
        r#"]}"#,
    ]
    .concat();
    Circuit::from_json(&json).unwrap()
}

fn board(parts: &[&str], connections: &[&str]) -> Board {
    let circuit = circuit(parts, connections);
    let registry = Registry::builtin();
    Board::new(FlatBoard::build(&circuit, &registry).expect("the diagram flattens"))
        .expect("the board builds")
}

fn part(id: &str, type_key: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{{}}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

fn spi(board: &mut Board, select: u8, tx: &[u8]) {
    board.spi_transfer(0, 0, select, tx, tx.len());
}

#[test]
fn part_framebuffer_returns_an_ili9341_in_rgb8() {
    let display = part("tft", "litz-ili9341");
    let mut board = board(
        &[BOARD, &display],
        &[
            &wire("esp:GPIO7", "tft:CS"),
            &wire("esp:GPIO8", "tft:SCK"),
            &wire("esp:GPIO9", "tft:MOSI"),
            &wire("esp:GPIO10", "tft:MISO"),
            &wire("esp:GPIO18", "tft:DC"),
            &wire("esp:GPIO19", "tft:RESET"),
            &wire("esp:3V3", "tft:VCC"),
            &wire("esp:GND.1", "tft:GND"),
        ],
    );
    // One red pixel at (5, 9).
    spi(&mut board, 7, &[0x00, 0x2a, 0x00, 0x05, 0x00, 0x05]);
    spi(&mut board, 7, &[0x00, 0x2b, 0x00, 0x09, 0x00, 0x09]);
    spi(&mut board, 7, &[0x00, 0x2c]);
    spi(&mut board, 7, &[0x40, 0xf8, 0x00]);

    let image = board.part_framebuffer("tft").expect("the TFT is a display");
    assert_eq!((image.width(), image.height()), (240, 320));
    assert_eq!(image.pixel(5, 9), Some([255, 0, 0, 255]));
    assert_eq!(image.pixel(0, 0), Some([0, 0, 0, 255]));

    let png = image.to_png();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    assert_eq!(
        u32::from_be_bytes(png[16..20].try_into().unwrap()),
        image.width()
    );
    assert_eq!(
        u32::from_be_bytes(png[20..24].try_into().unwrap()),
        image.height()
    );

    assert!(board.part_framebuffer("nope").is_none(), "unknown id");
    assert!(board.part_framebuffer("esp").is_none(), "the board itself");
}

#[test]
fn part_framebuffer_returns_an_ssd1306_in_page_order() {
    let oled = part("oled", "litz-ssd1306");
    let mut board = board(
        &[BOARD, &oled],
        &[
            &wire("esp:GPIO8", "oled:SDA"),
            &wire("esp:GPIO9", "oled:SCL"),
            &wire("esp:3V3", "oled:VCC"),
            &wire("esp:GND.1", "oled:GND"),
        ],
    );
    board
        .buses_mut()
        .i2c_mut(0, 0)
        .write(0x3c, &[0x00, 0xb0, 0x00, 0x10]);
    board
        .buses_mut()
        .i2c_mut(0, 0)
        .write(0x3c, &[0x40, 0xaa, 0x0f]);

    let image = board
        .part_framebuffer("oled")
        .expect("the OLED is a display");
    assert_eq!((image.width(), image.height()), (128, 64));
    // 0xaa is bits 1, 3, 5, 7 in page 0 column 0; 0x0f is bits 0..3 in column 1.
    assert_eq!(image.pixel(0, 0), Some([0, 0, 0, 255]));
    assert_eq!(image.pixel(0, 1), Some([255, 255, 255, 255]));
    assert_eq!(image.pixel(1, 0), Some([255, 255, 255, 255]));
    assert_eq!(image.pixel(1, 4), Some([0, 0, 0, 255]));
}

#[test]
fn part_framebuffer_returns_the_epaper_glass() {
    let paper = part("paper", "litz-epaper-1in54");
    let mut board = board(
        &[BOARD, &paper],
        &[
            &wire("esp:GPIO7", "paper:CS"),
            &wire("esp:GPIO8", "paper:CLK"),
            &wire("esp:GPIO9", "paper:DIN"),
            &wire("esp:3V3", "paper:VCC"),
            &wire("esp:GND.1", "paper:GND"),
        ],
    );
    // The panel boots all white; a refresh with 0x80 lights only (0, 0).
    let booted = board.part_framebuffer("paper").expect("the panel");
    assert_eq!((booted.width(), booted.height()), (200, 200));
    assert_eq!(booted.pixel(1, 0), Some([255, 255, 255, 255]));

    spi(&mut board, 7, &[0x00, 0x10]);
    spi(&mut board, 7, &[0x40, 0x80]);
    spi(&mut board, 7, &[0x00, 0x11, 0x12]);

    let refreshed = board.part_framebuffer("paper").expect("the panel");
    assert_eq!(refreshed.pixel(0, 0), Some([255, 255, 255, 255]));
    assert_eq!(refreshed.pixel(1, 0), Some([0, 0, 0, 255]));
}

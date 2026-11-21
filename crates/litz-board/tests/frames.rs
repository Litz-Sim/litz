//! Whole-frame read-back.
//!
//! Every display model exposes its pixels as one slice plus a counter that
//! says whether they changed. These tests build each part the way the engine
//! does — through the builtin catalog and a flattened board — draw through the
//! part's public bus interface, then read the framebuffer back and check the
//! change counter moved.

use litz_board::{Board, Epd, FlatBoard, Ili9341, Ssd1306};
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

fn part(id: &str, type_key: &str, attrs: &str) -> String {
    format!(r#"{{"id":"{id}","type":"{type_key}","attrs":{attrs}}}"#)
}

fn wire(from: &str, to: &str) -> String {
    format!(r#"["{from}","{to}","",[]]"#)
}

fn spi(board: &mut Board, select: u8, tx: &[u8]) -> Vec<u8> {
    board.spi_transfer(0, 0, select, tx, tx.len())
}

fn ili_model(board: &Board) -> &Ili9341 {
    board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Ili9341>(7))
        .expect("the ILI9341 is on spi0 select GPIO7")
}

fn ssd_model(board: &Board) -> &Ssd1306 {
    board
        .buses()
        .i2c
        .get(&(0, 0))
        .and_then(|bus| bus.device_as::<Ssd1306>(0x3c))
        .expect("the SSD1306 is on i2c0 at 0x3c")
}

fn epd_model(board: &Board) -> &Epd {
    board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Epd>(7))
        .expect("the e-paper is on spi0 select GPIO7")
}

#[test]
fn an_ili9341_framebuffer_is_row_major_rgb565() {
    let display = part("tft", "litz-ili9341", "{}");
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
    let before = ili_model(&board).frame_version();

    // A 2x2 window at (5, 9) painted red, green, blue, white in raster order.
    spi(&mut board, 7, &[0x00, 0x2a, 0x00, 0x05, 0x00, 0x06]); // columns 5..6
    spi(&mut board, 7, &[0x00, 0x2b, 0x00, 0x09, 0x00, 0x0a]); // rows 9..10
    spi(&mut board, 7, &[0x00, 0x2c]);
    spi(
        &mut board,
        7,
        &[0x40, 0xf8, 0x00, 0x07, 0xe0, 0x00, 0x1f, 0xff, 0xff],
    );

    let model = ili_model(&board);
    let (pixels, width, height) = model.framebuffer();
    assert_eq!((width, height), (240, 320));
    assert_eq!(pixels.len(), 240 * 320, "one RGB565 word per pixel");

    let at = |x: usize, y: usize| pixels[y * width as usize + x];
    assert_eq!(at(5, 9), 0xf800, "red");
    assert_eq!(at(6, 9), 0x07e0, "green");
    assert_eq!(at(5, 10), 0x001f, "blue");
    assert_eq!(at(6, 10), 0xffff, "white");
    assert_eq!(at(0, 0), 0, "untouched pixels stay black");
    assert_eq!(model.pixel(6, 10), Some(0xffff));

    assert!(
        model.frame_version() > before,
        "drawing bumped the frame version"
    );
}

#[test]
fn an_ili9341_fill_bumps_the_frame_version() {
    // `fill` is the model's own fast path; constructing it directly is the
    // only way to call a mutating method on a builtin device.
    let mut panel = Ili9341::new();
    let before = panel.frame_version();
    panel.fill(0x1234);

    let (pixels, width, height) = panel.framebuffer();
    assert_eq!((width, height), (240, 320));
    assert!(pixels.iter().all(|&color| color == 0x1234));
    assert!(
        panel.frame_version() > before,
        "fill bumped the frame version"
    );
}

#[test]
fn an_ssd1306_framebuffer_is_page_major_lsb_first() {
    let oled = part("oled", "litz-ssd1306", "{}");
    let mut board = board(
        &[BOARD, &oled],
        &[
            &wire("esp:GPIO8", "oled:SDA"),
            &wire("esp:GPIO9", "oled:SCL"),
            &wire("esp:3V3", "oled:VCC"),
            &wire("esp:GND.1", "oled:GND"),
        ],
    );
    let before = ssd_model(&board).frame_version();

    // Page 0, column 0, then two data bytes.
    assert!(
        board
            .buses_mut()
            .i2c_mut(0, 0)
            .write(0x3c, &[0x00, 0xb0, 0x00, 0x10])
    );
    assert!(
        board
            .buses_mut()
            .i2c_mut(0, 0)
            .write(0x3c, &[0x40, 0xaa, 0x0f])
    );

    let model = ssd_model(&board);
    let (bytes, width, height) = model.framebuffer();
    assert_eq!((width, height), (128, 64));
    assert_eq!(bytes.len(), 128 * 8, "one byte per page column");
    assert_eq!(bytes[0], 0xaa, "page 0, column 0");
    assert_eq!(bytes[1], 0x0f, "page 0, column 1");
    assert_eq!(bytes[128], 0, "page 1 is untouched");

    // GDDRAM packs bit 0 as the top row of the page, so 0xaa reads top-down as
    // 0,1,0,1,0,1,0,1 and 0x0f lights the first four rows of column 1.
    assert!(!model.pixel(0, 0), "bit 0 clear");
    assert!(model.pixel(0, 1), "bit 1 set");
    assert!(!model.pixel(0, 6), "bit 6 clear");
    assert!(model.pixel(0, 7), "bit 7 is the bottom row of the page");
    assert!(model.pixel(1, 0), "bit 0 set in the second byte");

    assert!(
        model.frame_version() > before,
        "writing GDDRAM bumped the frame version"
    );
}

#[test]
fn an_epaper_refresh_bumps_the_frame_version() {
    let panel = part("paper", "litz-epaper-1in54", "{}");
    let mut board = board(
        &[BOARD, &panel],
        &[
            &wire("esp:GPIO7", "paper:CS"),
            &wire("esp:GPIO8", "paper:CLK"),
            &wire("esp:GPIO9", "paper:DIN"),
            &wire("esp:3V3", "paper:VCC"),
            &wire("esp:GND.1", "paper:GND"),
        ],
    );
    let before = epd_model(&board).frame_version();
    assert!(epd_model(&board).pixel(1, 0), "the panel boots all white");

    spi(&mut board, 7, &[0x00, 0x10]); // start a frame
    spi(&mut board, 7, &[0x40, 0x80]); // one lit pixel in the pending frame
    assert_eq!(
        epd_model(&board).frame_version(),
        before,
        "a pending frame is not a refresh"
    );
    assert!(
        epd_model(&board).pixel(1, 0),
        "the glass still shows the old frame"
    );

    spi(&mut board, 7, &[0x00, 0x11, 0x12]); // stop, refresh
    let model = epd_model(&board);
    assert!(
        model.frame_version() > before,
        "the refresh bumped the frame version"
    );
    assert!(model.pixel(0, 0), "bit 7 of the first byte is lit");
    assert!(!model.pixel(1, 0), "the rest of the refreshed page is dark");
}

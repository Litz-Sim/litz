//! An XPT2046 touch controller on the board's SPI bus.
//!
//! The controller is a bus part like the ADC or the display: the manifest puts
//! it on `spi0`, the diagram's wires say which chip select and clock it uses,
//! and the runtime hands SPI transactions to it. A host scenario presses the
//! panel with [`Board::touch_panel`] and a driver's X/Y reads return the point.

use litz_board::touch::{COMMAND_AUX, COMMAND_PRESSURE, COMMAND_X, COMMAND_Y};
use litz_board::{Board, FlatBoard, Xpt2046};
use litz_catalog::Registry;
use litz_diagram::Circuit;

const BOARD: &str = r#"{"id":"esp","type":"board-esp32-c3-devkitm-1","attrs":{}}"#;
const TOUCH: &str = r#"{"id":"touch","type":"litz-xpt2046","attrs":{}}"#;

/// The touch controller's CS on GPIO7; it shares the clock and data lines with
/// the bus.
const TOUCH_WIRES: &[&str] = &[
    r#"["esp:GPIO7","touch:CS","",[]]"#,
    r#"["esp:GPIO8","touch:CLK","",[]]"#,
    r#"["esp:GPIO9","touch:DIN","",[]]"#,
    r#"["esp:GPIO10","touch:DOUT","",[]]"#,
    r#"["esp:GPIO6","touch:IRQ","",[]]"#,
    r#"["esp:3V3","touch:VCC","",[]]"#,
    r#"["esp:GND.1","touch:GND","",[]]"#,
];

/// The built-in catalog plus the XPT2046 manifest this change adds.
///
/// The manifest is loaded from the catalog's own file so the test parses the
/// same text a user's catalog would.
fn registry() -> Registry {
    let mut registry = Registry::builtin();
    registry
        .insert_text(include_str!(
            "../../litz-catalog/builtin/parts/litz-xpt2046.toml"
        ))
        .expect("the XPT2046 manifest parses");
    registry
}

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

fn flat_with(
    registry: &Registry,
    parts: &[&str],
    connections: &[&str],
) -> Result<FlatBoard, String> {
    FlatBoard::build(&circuit(parts, connections), registry).map_err(|e| e.to_string())
}

fn board(parts: &[&str], connections: &[&str]) -> Board {
    Board::new(flat_with(&registry(), parts, connections).expect("the diagram flattens"))
        .expect("the board builds")
}

/// One X/Y conversion through the board's SPI controller: the control word,
/// then two clocks during which the 12-bit result shifts out left-aligned.
fn read_channel(board: &mut Board, chip_select: u8, command: u8) -> u16 {
    let rx = board.spi_transfer(0, 0, chip_select, &[command, 0x00, 0x00], 3);
    ((u16::from(rx[1]) << 8) | u16::from(rx[2])) >> 4
}

#[test]
fn a_touch_point_reads_back_on_the_x_and_y_channels() {
    let mut board = board(&[BOARD, TOUCH], TOUCH_WIRES);
    board
        .touch_panel(1200, 800, true)
        .expect("the panel takes a touch");

    assert_eq!(read_channel(&mut board, 7, COMMAND_X), 1200);
    assert_eq!(read_channel(&mut board, 7, COMMAND_Y), 800);
    assert_eq!(
        read_channel(&mut board, 7, COMMAND_PRESSURE),
        0,
        "the pressure channel is not modelled"
    );
    assert_eq!(read_channel(&mut board, 7, COMMAND_AUX), 0);
}

#[test]
fn touch_panel_reaches_the_part_and_tracks_the_pen() {
    let mut board = board(&[BOARD, TOUCH], TOUCH_WIRES);
    board
        .touch_panel(300, 400, true)
        .expect("the panel takes a touch");

    {
        let model = board
            .buses()
            .spi
            .get(&(0, 0))
            .and_then(|spi| spi.device_as::<Xpt2046>(7))
            .expect("the XPT2046 is on spi0 select GPIO7");
        assert!(model.touched());
        assert_eq!(model.position(), (300, 400));
    }

    board
        .touch_panel(300, 400, false)
        .expect("the panel is still there");
    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Xpt2046>(7))
        .expect("the XPT2046 is on spi0 select GPIO7");
    assert!(!model.touched());
}

#[test]
fn a_board_without_a_touch_controller_refuses_a_touch() {
    let mut board = board(&[BOARD], &[]);
    let error = board
        .touch_panel(1, 2, true)
        .expect_err("there is no panel");
    assert!(error.contains("XPT2046"), "{error}");
}

#[test]
fn touch_panel_finds_the_controller_beside_a_display() {
    let display = r#"{"id":"tft","type":"litz-ili9341","attrs":{}}"#;
    let wires = [
        // The display is on the lower chip select; the touch controller shares
        // the clock and data lines but has its own select.
        r#"["esp:GPIO5","tft:CS","",[]]"#,
        r#"["esp:GPIO8","tft:SCK","",[]]"#,
        r#"["esp:GPIO9","tft:MOSI","",[]]"#,
        r#"["esp:GPIO10","tft:MISO","",[]]"#,
        r#"["esp:GPIO18","tft:DC","",[]]"#,
        r#"["esp:GPIO19","tft:RESET","",[]]"#,
        r#"["esp:3V3","tft:VCC","",[]]"#,
        r#"["esp:GND.1","tft:GND","",[]]"#,
        r#"["esp:GPIO7","touch:CS","",[]]"#,
        r#"["esp:GPIO8","touch:CLK","",[]]"#,
        r#"["esp:GPIO9","touch:DIN","",[]]"#,
        r#"["esp:GPIO10","touch:DOUT","",[]]"#,
        r#"["esp:GPIO6","touch:IRQ","",[]]"#,
        r#"["esp:3V3","touch:VCC","",[]]"#,
        r#"["esp:GND.1","touch:GND","",[]]"#,
    ];
    let mut board = board(&[BOARD, display, TOUCH], &wires);
    board
        .touch_panel(2000, 1000, true)
        .expect("the panel takes a touch");

    let model = board
        .buses()
        .spi
        .get(&(0, 0))
        .and_then(|spi| spi.device_as::<Xpt2046>(7))
        .expect("the touch controller is on GPIO7");
    assert_eq!(model.position(), (2000, 1000));
    assert!(
        board
            .buses()
            .spi
            .get(&(0, 0))
            .and_then(|spi| spi.device_as::<Xpt2046>(5))
            .is_none(),
        "the display's select is not a touch controller"
    );
    assert_eq!(read_channel(&mut board, 7, COMMAND_X), 2000);
    assert_eq!(read_channel(&mut board, 7, COMMAND_Y), 1000);
}

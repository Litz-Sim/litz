//! One running machine plus the state the ABI exposes about it.
//!
//! Construction is [`litz_engine::machine::boot_c3`], the same path the CLI
//! takes; the only difference is where the bytes come from. A browser holds
//! them in host memory, a project holds them in files.
//!
//! Everything the exports read back after that — simulated time, serial text,
//! electrical conflicts, part state — lives on this struct.

use litz_core::PartState;
use litz_diagram::Circuit;
use litz_engine::config::DecodeSection;
use litz_engine::machine::{self, Images};
use litz_esp::backend::C3;

/// 4 MiB of external flash, the devkit default.
const FLASH_SIZE: usize = 4 * 1024 * 1024;
/// Espressif's example MAC. Nothing in the simulation is MAC-specific yet, so
/// a fixed value keeps two runs byte-identical.
const DEFAULT_MAC: [u8; 6] = [0x24, 0x0a, 0xc4, 0x12, 0x34, 0x56];

/// Serial text kept for the host. A runaway firmware can print without bound;
/// past this many bytes the oldest output is dropped, because a browser tab
/// must not be exhausted by a guest loop.
const SERIAL_CAP: usize = 1024 * 1024;

/// The net number `litz-board` uses for a fault that is not tied to one net
/// (the fixed-point solver did not converge).
const NO_NET: u16 = u16::MAX;

pub(crate) struct Session {
    mcu: C3,
    circuit: Circuit,
    /// UART0 bytes since the host last drained them.
    serial: Vec<u8>,
    /// Protocols the host asked to read off the probed channels, with the
    /// text it spelled them with, so the host can match its own list to the
    /// document even when two decoders share a label.
    decodes: Vec<(String, DecodeSection)>,
}

impl Session {
    /// Build the machine. All images are owned slices by the time this runs.
    ///
    /// The application is required: a session with an empty flash has nothing
    /// to run. ROM and the bootloader/partition-table pair are optional; the
    /// pair goes together or not at all, because the bootloader looks the
    /// application up through the table.
    pub(crate) fn new(
        diagram: &str,
        rom: &[u8],
        bootloader: &[u8],
        ptable: &[u8],
        app: &[u8],
    ) -> Result<Self, String> {
        let (mcu, circuit) = machine::boot_c3(
            diagram,
            Images {
                rom,
                bootloader,
                partition_table: ptable,
                app,
            },
            FLASH_SIZE,
            DEFAULT_MAC,
        )?;

        Ok(Self {
            mcu,
            circuit,
            serial: Vec::new(),
            decodes: Vec::new(),
        })
    }

    /// The parsed diagram. Kept so part enumeration reports the ids the host
    /// wrote, not the prefixed ids a board expansion invents.
    pub(crate) fn diagram(&self) -> &Circuit {
        &self.circuit
    }

    /// Simulated milliseconds since reset, saturating at `u32::MAX`
    /// (about 49.7 days of simulated time).
    pub(crate) fn ms(&self) -> u32 {
        (self.mcu.seconds() * 1000.0) as u32
    }

    /// Advance at most `steps` scheduling steps, collect UART0 and report the
    /// clock. One scheduling step is not one instruction or one microsecond;
    /// the host decides how much simulated time a call should cover by its
    /// step budget.
    pub(crate) fn run(&mut self, steps: u32) -> u32 {
        let _ = self.mcu.run(u64::from(steps));
        let chunk = self.mcu.take_uart0();
        append_capped(&mut self.serial, &chunk, SERIAL_CAP);
        self.ms()
    }

    /// Remove up to `max` bytes from the front of the serial log.
    /// Begin recording net transitions for the waveform export. Cheap enough
    /// to leave on for a session; the host asks for the dump when it wants it.
    pub(crate) fn start_trace(&mut self) {
        if let Some(board) = self.mcu.board() {
            board.borrow_mut().start_trace();
        }
    }

    pub(crate) fn trace_len(&self) -> usize {
        self.mcu
            .board()
            .map_or(0, |board| board.borrow().trace_len())
    }

    /// Watch a point in the circuit for the scope panel.
    pub(crate) fn add_probe(&mut self, name: &str, point: &str) -> Result<(), String> {
        let Some(board) = self.mcu.board() else {
            return Err("the session has no board".into());
        };
        board.borrow_mut().add_probe(name, point)
    }

    /// Add a protocol decoder, spelled the way `--decode` spells it
    /// (`uart:rx=tx,baud=115200`). Every channel it names must already be a
    /// probe, so the host adds its probes first.
    pub(crate) fn add_decode(&mut self, spec: &str) -> Result<(), String> {
        let section: DecodeSection = spec.parse()?;
        let Some(board) = self.mcu.board() else {
            return Err("the session has no board".into());
        };
        litz_engine::decode::check(&section, board.borrow().probes())?;
        self.decodes.push((spec.to_string(), section));
        Ok(())
    }

    /// Every probe's record and measurements, as JSON.
    ///
    /// One document per poll keeps the browser side free of a binary layout:
    /// a channel is its name, the point it watches, the measurements, and two
    /// arrays of `[tick_ns, value]` samples (levels as 0/1/2, volts as f64).
    /// The document is small because a record only holds changes, not samples
    /// on a fixed grid.
    /// The board's display as RGB565, with its size and change counter.
    pub(crate) fn display(&mut self) -> Option<(u32, u32, Vec<u16>, u64)> {
        self.mcu.board_model().display()
    }

    /// The display's change counter, without building a frame.
    pub(crate) fn display_version(&mut self) -> u64 {
        self.mcu.board_model().display_version()
    }

    /// A touch at panel pixels. A board with no controller ignores it.
    /// Touch the panel at raw controller codes. False when the board has no
    /// touch controller, because a finger on a panel that is not there changes
    /// nothing and the host should not be told otherwise.
    pub(crate) fn touch(&mut self, x: u16, y: u16, down: bool) -> bool {
        let Some(board) = self.mcu.board() else {
            return false;
        };
        board.borrow_mut().touch_panel(x, y, down).is_ok()
    }

    pub(crate) fn scope_json(&self) -> String {
        let Some(board) = self.mcu.board() else {
            return r#"{"ms":0,"probes":[],"decodes":[]}"#.to_string();
        };
        let board = board.borrow();
        let channels = board.scope();
        let mut out = format!(r#"{{"ms":{},"probes":["#, self.ms());
        for (index, channel) in channels.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let measured = channel.measured;
            out.push_str(&format!(
                r#"{{"name":{},"point":{},"net":{},"edges":{},"high_ns":{},"low_ns":{},"frequency_hz":{},"duty":{},"min_volts":{},"max_volts":{}"#,
                json_string(&channel.probe.name),
                json_string(&channel.probe.point),
                channel.probe.net,
                measured.edges,
                measured.high_ns,
                measured.low_ns,
                json_option(measured.frequency_hz),
                json_option(measured.duty),
                json_option(measured.min_volts),
                json_option(measured.max_volts),
            ));
            out.push_str(r#","digital":["#);
            for (index, (at, level)) in channel.series.digital.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&format!("[{at},{}]", level_code(*level)));
            }
            out.push_str(r#"],"volts":["#);
            for (index, (at, volts)) in channel.series.volts.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&format!("[{at},{volts:.6}]"));
            }
            out.push_str("]}");
        }
        out.push_str("],\"decodes\":[");
        for (index, (spec, section)) in self.decodes.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let frames = litz_engine::decode::frames(section, &channels).unwrap_or_default();
            out.push_str(&format!(
                r#"{{"spec":{},"kind":{},"name":{},"frames":["#,
                json_string(spec),
                json_string(&section.kind),
                json_string(&section.label()),
            ));
            for (index, frame) in frames.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&format!(
                    r#"{{"at":{},"end":{},"text":{},"error":{}}}"#,
                    frame.at,
                    frame.end,
                    json_string(&frame.text),
                    frame
                        .error
                        .as_deref()
                        .map_or("null".to_string(), json_string),
                ));
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }

    pub(crate) fn trace_vcd(&self) -> String {
        self.mcu
            .board()
            .map(|board| board.borrow().vcd())
            .unwrap_or_default()
    }

    pub(crate) fn take_serial(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.serial.len());
        self.serial.drain(..n).collect()
    }

    pub(crate) fn conflict_count(&self) -> u32 {
        self.mcu.conflicts().len() as u32
    }

    /// Text for one conflict. A fault that belongs to no particular net (a
    /// board that would not settle) is reported without the `net` prefix.
    pub(crate) fn conflict_text(&self, index: u32) -> Option<String> {
        let conflicts = self.mcu.conflicts();
        let conflict = conflicts.get(index as usize)?;
        Some(if conflict.net == NO_NET {
            format!("board: {}", conflict.detail)
        } else {
            format!("net {}: {}", conflict.net, conflict.detail)
        })
    }

    /// Part-state code for the ABI, or `-1` when the diagram has no such part.
    pub(crate) fn state_of(&self, id: &str) -> i32 {
        match self.mcu.state(id) {
            None => -1,
            Some(state) => state_code(state),
        }
    }

    /// The number behind a state code, or `0` when the state has none.
    pub(crate) fn value_of(&self, id: &str) -> f64 {
        self.mcu.state(id).map(state_value).unwrap_or(0.0)
    }

    /// Number of parts the diagram declares. Board-expanded parts such as
    /// `esp/led_pwr` are readable by id but are not enumerated here.
    pub(crate) fn part_count(&self) -> u32 {
        self.diagram().parts.len() as u32
    }

    pub(crate) fn part_id(&self, index: u32) -> Option<String> {
        self.diagram()
            .parts
            .get(index as usize)
            .map(|part| part.id.clone())
    }

    /// Hold a pushbutton pressed or released.
    ///
    /// The press goes through the board, not around it: the part closes its
    /// contacts, the nets re-resolve, and the chip sees the new level only if
    /// the circuit routes the switch to a pin. That is why a switch in series
    /// with an LED lights it here as well.
    pub(crate) fn press(&mut self, id: &str, pressed: bool) -> Result<(), String> {
        if self.mcu.state(id).is_none() {
            return Err(format!("no part {id:?} in the diagram"));
        }
        if self.mcu.press(id, pressed) {
            Ok(())
        } else {
            Err(format!(
                "part {id:?} does not take a press; it is not a switch"
            ))
        }
    }

    /// Move a continuous control such as a potentiometer's knob.
    pub(crate) fn set_control(&mut self, id: &str, permille: u16) -> Result<(), String> {
        if self.mcu.state(id).is_none() {
            return Err(format!("no part {id:?} in the diagram"));
        }
        if self.mcu.set_control(id, permille) {
            Ok(())
        } else {
            Err(format!("part {id:?} has no continuous control"))
        }
    }

    /// Bytes typed into the serial monitor, straight into UART0's receive FIFO.
    pub(crate) fn write_serial(&mut self, data: &[u8]) {
        self.mcu.write_serial(data);
    }
}

/// Map a part state onto the ABI's integer codes.
pub(crate) fn state_code(state: PartState) -> i32 {
    match state {
        PartState::None => 0,
        PartState::Led { lit: false } => 1,
        PartState::Led { lit: true } => 2,
        PartState::Button { pressed: false } => 3,
        PartState::Button { pressed: true } => 4,
        PartState::Switch { on: false } => 5,
        PartState::Switch { on: true } => 6,
        PartState::Tone {
            sounding: false, ..
        } => 7,
        PartState::Tone { sounding: true, .. } => 8,
        // These two carry a number rather than a flag; the host reads it with
        // `litz_part_value`, and the code only says which kind it is.
        PartState::Servo { .. } => 9,
        PartState::Segments { .. } => 10,
        PartState::Knob { .. } => 11,
        PartState::Radio { .. } => 12,
    }
}

/// The number behind a state code: servo degrees, a segment mask, a tone rate.
pub(crate) fn state_value(state: PartState) -> f64 {
    match state {
        PartState::Servo { degrees } => f64::from(degrees),
        PartState::Segments { mask } => f64::from(mask),
        PartState::Knob { permille } => f64::from(permille),
        PartState::Tone { hz, .. } => f64::from(hz),
        _ => 0.0,
    }
}

/// Append serial bytes, dropping the oldest output once `cap` is exceeded.
///
/// Trimming from the front keeps the most recent output, which is what a
/// serial monitor shows when it scrolls.
fn append_capped(buffer: &mut Vec<u8>, bytes: &[u8], cap: usize) {
    if bytes.is_empty() {
        return;
    }
    buffer.extend_from_slice(bytes);
    if buffer.len() > cap {
        let excess = buffer.len() - cap;
        buffer.drain(..excess);
    }
}

/// A session driven the way the browser drives it: a fixed step budget per
/// call, serial drained between calls, part state read back by id. This is the
/// native half of the ABI, so a failure here is a failure the page would hit.
#[cfg(test)]
mod driven {
    use super::*;

    const BLINKY: &str = r#"{
      "version": 1,
      "parts": [
        {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
        {"id": "r1", "type": "litz-resistor", "attrs": {"value": "220"}},
        {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
      ],
      "connections": [
        ["esp:GPIO8", "r1:1", "", []],
        ["r1:2", "led1:A", "", []],
        ["led1:C", "esp:GND.1", "", []]
      ]
    }"#;

    const BUTTON: &str = r#"{
      "version": 1,
      "parts": [
        {"id": "esp", "type": "board-esp32-c3-devkitm-1", "attrs": {}},
        {"id": "btn1", "type": "litz-pushbutton", "attrs": {}}
      ],
      "connections": [
        ["esp:GPIO0", "btn1:1.l", "", []],
        ["btn1:2.l", "esp:GND.1", "", []]
      ]
    }"#;

    fn session(diagram: &str) -> Session {
        let app = litz_fixture::blinky(16_000_000);
        Session::new(diagram, &[], &[], &[], &app).expect("session")
    }

    #[test]
    fn the_demo_firmware_blinks_and_prints() {
        let mut s = session(BLINKY);
        let mut serial = String::new();
        let mut saw_lit = false;
        let mut saw_dark = false;
        for _ in 0..600 {
            s.run(100_000);
            serial.push_str(&String::from_utf8_lossy(&s.take_serial(SERIAL_CAP)));
            match s.state_of("led1") {
                2 => saw_lit = true,
                1 => saw_dark = true,
                other => panic!("unexpected LED state code {other}"),
            }
            if saw_lit && saw_dark && serial.contains("litz: blinky") {
                break;
            }
        }
        assert!(serial.contains("litz: blinky"), "serial was {serial:?}");
        assert!(saw_lit, "the LED never lit");
        assert!(saw_dark, "the LED never turned off again");
        assert_eq!(s.conflict_count(), 0);
    }

    #[test]
    fn the_trace_is_a_vcd_of_the_nets_the_firmware_moved() {
        let mut s = session(BLINKY);
        s.start_trace();
        for _ in 0..20 {
            s.run(100_000);
        }
        assert!(
            s.trace_len() > 0,
            "the firmware toggled a pin, so the trace is not empty"
        );
        let vcd = s.trace_vcd();
        assert!(vcd.starts_with("$timescale 1ns $end"), "{vcd}");
        assert!(vcd.contains("gpio8"), "{vcd}");
        assert!(vcd.contains("#"), "the dump has timestamps: {vcd}");
    }

    #[test]
    fn the_scope_document_reports_a_probes_record() {
        let mut s = session(BLINKY);
        s.add_probe("led", "esp:GPIO8").expect("the point exists");
        s.start_trace();
        for _ in 0..20 {
            s.run(100_000);
        }
        let json = s.scope_json();
        assert!(json.contains(r#""name":"led""#), "{json}");
        assert!(json.contains(r#""point":"esp:GPIO8""#), "{json}");
        assert!(json.contains(r#""digital":[["#), "{json}");
        assert!(json.contains(r#""edges":"#), "{json}");
        assert!(
            s.add_probe("led", "esp:GPIO8").is_err(),
            "a channel name is used once"
        );
    }

    #[test]
    fn a_decoder_needs_a_probe_and_appears_in_the_scope_document() {
        let mut s = session(BLINKY);
        assert!(
            s.add_decode("uart:rx=led,baud=9600").is_err(),
            "the channel is not a probe yet"
        );
        s.add_probe("led", "esp:GPIO8").expect("the point exists");
        s.add_decode("uart:rx=led").expect("the channel exists");
        s.start_trace();
        for _ in 0..20 {
            s.run(100_000);
        }
        let json = s.scope_json();
        assert!(
            json.contains(r#""decodes":[{"spec":"uart:rx=led","kind":"uart","name":"uart led""#),
            "{json}"
        );
        assert!(json.contains(r#""frames":["#), "{json}");
    }

    #[test]
    fn an_unknown_part_is_minus_one_and_refuses_a_press() {
        let mut s = session(BLINKY);
        assert_eq!(s.state_of("nope"), -1);
        assert!(s.press("nope", true).is_err());
        assert!(s.press("led1", true).is_err(), "an LED has no press input");
    }

    #[test]
    fn pressing_a_button_moves_the_part_and_the_net() {
        let mut s = session(BUTTON);
        assert_eq!(s.state_of("btn1"), 3);
        s.press("btn1", true).expect("press");
        assert_eq!(s.state_of("btn1"), 4);
        s.press("btn1", false).expect("release");
        assert_eq!(s.state_of("btn1"), 3);
    }

    #[test]
    fn an_empty_application_is_refused_before_anything_runs() {
        let err = match Session::new(BLINKY, &[], &[], &[], &[]) {
            Ok(_) => panic!("an empty application must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("no application image"), "{err}");
    }

    #[test]
    fn a_lone_bootloader_is_refused() {
        let app = litz_fixture::blinky(16_000_000);
        let err = match Session::new(BLINKY, &[], &[0xE9], &[], &app) {
            Ok(_) => panic!("a lone bootloader must be refused"),
            Err(e) => e,
        };
        assert!(err.contains("must be supplied together"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_has_its_own_code() {
        assert_eq!(state_code(PartState::None), 0);
        assert_eq!(state_code(PartState::Led { lit: false }), 1);
        assert_eq!(state_code(PartState::Led { lit: true }), 2);
        assert_eq!(state_code(PartState::Button { pressed: false }), 3);
        assert_eq!(state_code(PartState::Button { pressed: true }), 4);
        assert_eq!(state_code(PartState::Switch { on: true }), 6);
        assert_eq!(state_code(PartState::Servo { degrees: 90 }), 9);
        assert_eq!(state_value(PartState::Servo { degrees: 90 }), 90.0);
        assert_eq!(state_value(PartState::Segments { mask: 0x7f }), 127.0);
    }

    #[test]
    fn serial_keeps_everything_under_the_cap() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, b"hello", 8);
        append_capped(&mut buffer, b"!!", 8);
        assert_eq!(buffer.as_slice(), &b"hello!!"[..]);
    }

    #[test]
    fn serial_drops_the_oldest_bytes_once_full() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, b"abcdef", 4);
        assert_eq!(buffer.as_slice(), &b"cdef"[..]);
    }

    #[test]
    fn one_chunk_larger_than_the_cap_keeps_its_tail() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, b"0123456789", 4);
        assert_eq!(buffer.as_slice(), &b"6789"[..]);
    }

    #[test]
    fn serial_is_exactly_the_cap_when_it_fits() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, b"1234", 4);
        assert_eq!(buffer.as_slice(), &b"1234"[..]);
    }

    #[test]
    fn a_zero_cap_keeps_nothing() {
        let mut buffer = Vec::new();
        append_capped(&mut buffer, b"data", 0);
        assert!(buffer.is_empty());
    }
}

/// A level as the scope JSON encodes it: low, high, high impedance.
fn level_code(level: litz_core::Level) -> u8 {
    match level {
        litz_core::Level::Low => 0,
        litz_core::Level::High => 1,
        litz_core::Level::HighZ => 2,
    }
}

/// A number, or `null` when the measurement does not exist.
fn json_option(value: Option<f64>) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{value:.6}"),
        _ => "null".to_string(),
    }
}

/// A JSON string literal. Probe names come from the host, so quotes and
/// control characters are escaped rather than trusted.
fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

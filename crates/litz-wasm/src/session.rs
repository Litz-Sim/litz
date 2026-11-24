//! One running machine plus the state the ABI exposes about it.
//!
//! Construction is [`litz_engine::machine::boot`], the same path the CLI
//! takes; the only difference is where the bytes come from. A browser holds
//! them in host memory, a project holds them in files.
//!
//! The backend is chosen from the diagram's board manifest, so the ABI reaches
//! every builtin chip: the ESP32 family, the ATmega328P and the RP2040.
//!
//! Everything the exports read back after that — simulated time, serial text,
//! electrical conflicts, part state, the display — lives on this struct.

use std::cell::RefCell;
use std::rc::Rc;

use litz_board::{Board, Chip, Stop};
use litz_core::PartState;
use litz_diagram::Circuit;
use litz_engine::config::DecodeSection;
use litz_engine::machine::{self, BoardImage, BootOptions, Images};

/// Serial text kept for the host. A runaway firmware can print without bound;
/// past this many bytes the oldest output is dropped, because a browser tab
/// must not be exhausted by a guest loop.
const SERIAL_CAP: usize = 1024 * 1024;

/// The net number `litz-board` uses for a fault that is not tied to one net
/// (the fixed-point solver did not converge).
const NO_NET: u16 = u16::MAX;

pub(crate) struct Session {
    /// One chip per development board in diagram order.
    chips: Vec<Box<dyn Chip>>,
    /// The diagram ids of those boards, in the same order.
    board_ids: Vec<String>,
    /// The same board every chip drives, kept so probes, the display and the
    /// trace can be asked without another trait method for each.
    board: Rc<RefCell<Board>>,
    /// The parsed diagram, kept so part enumeration reports the ids the host
    /// wrote, not the prefixed ids a board expansion invents.
    diagram: Circuit,
    /// Console bytes since the host last drained them, merged in board order.
    serial: Vec<u8>,
    /// One console backlog per board, so a host can read a board on its own.
    consoles: Vec<Vec<u8>>,
    /// Protocols the host asked to read off the probed channels, with the
    /// text it spelled them with, so the host can match its own list to the
    /// document even when two decoders share a label.
    decodes: Vec<(String, DecodeSection)>,
    /// How the last `run` ended, per board; the merged stop is the most
    /// significant one any board reported.
    stops: Vec<Stop>,
}

impl Session {
    /// Build the machine with default options. All images are borrowed; the
    /// boot copies them before returning.
    pub(crate) fn new(
        diagram: &str,
        rom: &[u8],
        bootloader: &[u8],
        partition_table: &[u8],
        app: &[u8],
    ) -> Result<Self, String> {
        Self::with_options(
            diagram,
            Images {
                rom,
                bootloader,
                partition_table,
                app,
            },
            &BootOptions::default(),
        )
    }

    /// Build the machine with the host's flash size, MAC and analog choice.
    pub(crate) fn with_options(
        diagram: &str,
        images: Images<'_>,
        options: &BootOptions,
    ) -> Result<Self, String> {
        if images.app.is_empty() {
            return Err("no application image: app_len is 0".into());
        }
        let board = BoardImage {
            id: None,
            app: images.app,
            rom: images.rom,
            bootloader: images.bootloader,
            partition_table: images.partition_table,
            flash_size: options.flash_size,
            mac: Some(options.mac),
        };
        Self::with_fleet(diagram, &[board], options)
    }

    /// Build one chip per development board, sharing one circuit.
    pub(crate) fn with_fleet(
        diagram: &str,
        images: &[BoardImage<'_>],
        options: &BootOptions,
    ) -> Result<Self, String> {
        let booted = machine::boot_fleet(diagram, images, options)?;
        let count = booted.chips.len();
        Ok(Self {
            chips: booted.chips,
            board_ids: booted.ids,
            board: booted.board,
            diagram: booted.circuit,
            serial: Vec::new(),
            consoles: vec![Vec::new(); count],
            decodes: Vec::new(),
            stops: vec![Stop::Budget; count],
        })
    }

    /// Simulated milliseconds since reset, saturating at `u32::MAX`
    /// (about 49.7 days of simulated time).
    pub(crate) fn ms(&self) -> u32 {
        let seconds = self
            .chips
            .iter()
            .map(|chip| chip.seconds())
            .fold(0.0f64, f64::max);
        (seconds * 1000.0) as u32
    }

    /// Advance at most `steps` scheduling steps, collect the console and
    /// report the clock. One scheduling step is not one instruction or one
    /// microsecond; the host decides how much simulated time a call should
    /// cover by its step budget.
    pub(crate) fn run(&mut self, steps: u32) -> u32 {
        for (index, chip) in self.chips.iter_mut().enumerate() {
            let result = chip.run(u64::from(steps));
            self.stops[index] = result.stop;
            let chunk = chip.take_console();
            if !chunk.is_empty() {
                append_capped(&mut self.serial, &chunk, SERIAL_CAP);
                if let Some(console) = self.consoles.get_mut(index) {
                    append_capped(console, &chunk, SERIAL_CAP);
                }
            }
        }
        self.ms()
    }

    /// How the last run ended: 0 budget, 1 idle, 2 halted, 3 trap, 4 reset,
    /// 5 breakpoint. With several chips the most significant stop wins.
    pub(crate) fn stop_code(&self) -> u32 {
        let stop = self
            .stops
            .iter()
            .copied()
            .max_by_key(|stop| stop_rank(*stop))
            .unwrap_or(Stop::Budget);
        stop_code(stop)
    }

    pub(crate) fn board_count(&self) -> u32 {
        self.chips.len() as u32
    }

    pub(crate) fn board_id(&self, index: u32) -> Option<String> {
        self.board_ids.get(index as usize).cloned()
    }

    pub(crate) fn board_chip_info_json(&self, index: u32) -> Option<String> {
        let chip = self.chips.get(index as usize)?;
        let info = chip.info();
        Some(format!(
            r#"{{"name":{},"cpuHz":{},"flash":{{"size":{},"bootloader":{},"partitionTable":{},"app":{}}}}}"#,
            json_string(&info.name),
            info.cpu_hz,
            info.flash.size,
            info.flash.bootloader,
            info.flash.partition_table,
            info.flash.app,
        ))
    }

    pub(crate) fn board_gpio_level(&self, index: u32, pin: u8) -> Option<litz_core::Level> {
        self.chips.get(index as usize)?.gpio_level(pin)
    }

    pub(crate) fn take_board_serial(&mut self, index: u32, max: usize) -> Option<Vec<u8>> {
        let console = self.consoles.get_mut(index as usize)?;
        let n = max.min(console.len());
        Some(console.drain(..n).collect())
    }

    pub(crate) fn board_display(&mut self, index: u32) -> Option<(u32, u32, Vec<u16>, u64)> {
        self.chips.get_mut(index as usize)?.display()
    }

    pub(crate) fn board_display_version(&mut self, index: u32) -> u64 {
        self.chips
            .get_mut(index as usize)
            .map(|chip| chip.display_version())
            .unwrap_or(0)
    }

    pub(crate) fn write_serial_board(&mut self, index: u32, data: &[u8]) -> bool {
        match self.chips.get_mut(index as usize) {
            Some(chip) => {
                chip.write_console(data);
                true
            }
            None => false,
        }
    }

    pub(crate) fn debug_available(&self, index: u32) -> bool {
        self.chips
            .get(index as usize)
            .map(|chip| chip.debug_available())
            .unwrap_or(false)
    }

    pub(crate) fn debug_pc(&self, index: u32) -> Option<u32> {
        self.chips.get(index as usize)?.debug_pc()
    }

    pub(crate) fn debug_registers(&self, index: u32) -> Option<[u32; 33]> {
        self.chips.get(index as usize)?.debug_register_file()
    }

    pub(crate) fn debug_memory(&mut self, index: u32, addr: u32, len: usize) -> Option<Vec<u8>> {
        Some(
            self.chips
                .get_mut(index as usize)?
                .debug_read_memory(addr, len),
        )
    }

    pub(crate) fn debug_disassemble(
        &mut self,
        index: u32,
        addr: u32,
        count: usize,
    ) -> Option<String> {
        Some(
            self.chips
                .get_mut(index as usize)?
                .debug_disassemble(addr, count),
        )
    }

    pub(crate) fn debug_set_breakpoints(&mut self, index: u32, pcs: &[u32]) -> bool {
        match self.chips.get_mut(index as usize) {
            Some(chip) => {
                chip.debug_set_breakpoints(pcs);
                true
            }
            None => false,
        }
    }

    pub(crate) fn debug_step(&mut self, index: u32) -> Option<u32> {
        let chip = self.chips.get_mut(index as usize)?;
        let stop = chip.debug_step()?;
        self.stops[index as usize] = stop;
        let chunk = chip.take_console();
        if !chunk.is_empty() {
            append_capped(&mut self.serial, &chunk, SERIAL_CAP);
            append_capped(&mut self.consoles[index as usize], &chunk, SERIAL_CAP);
        }
        Some(stop_code(stop))
    }

    pub(crate) fn debug_run(&mut self, index: u32, steps: u32) -> Option<u32> {
        let chip = self.chips.get_mut(index as usize)?;
        let result = chip.run(u64::from(steps));
        self.stops[index as usize] = result.stop;
        let chunk = chip.take_console();
        if !chunk.is_empty() {
            append_capped(&mut self.serial, &chunk, SERIAL_CAP);
            append_capped(&mut self.consoles[index as usize], &chunk, SERIAL_CAP);
        }
        Some(stop_code(result.stop))
    }

    /// The backend's own explanation for a trap or fault, if it has one.
    pub(crate) fn fault(&self) -> Option<String> {
        self.chips.iter().find_map(|chip| chip.fault())
    }

    /// The chip's name, clock and flash layout, as the host spells them.
    pub(crate) fn chip_info_json(&self) -> String {
        let info = self.chips[0].info();
        format!(
            r#"{{"name":{},"cpuHz":{},"flash":{{"size":{},"bootloader":{},"partitionTable":{},"app":{}}}}}"#,
            json_string(&info.name),
            info.cpu_hz,
            info.flash.size,
            info.flash.bootloader,
            info.flash.partition_table,
            info.flash.app,
        )
    }

    /// Begin recording net transitions for the waveform export. Cheap enough
    /// to leave on for a session; the host asks for the dump when it wants it.
    pub(crate) fn start_trace(&mut self) {
        self.board.borrow_mut().start_trace();
    }

    pub(crate) fn trace_len(&self) -> usize {
        self.board.borrow().trace_len()
    }

    /// Watch a point in the circuit for the scope panel.
    pub(crate) fn add_probe(&mut self, name: &str, point: &str) -> Result<(), String> {
        self.board.borrow_mut().add_probe(name, point)
    }

    /// Add a protocol decoder, spelled the way `--decode` spells it
    /// (`uart:rx=tx,baud=115200`). Every channel it names must already be a
    /// probe, so the host adds its probes first.
    pub(crate) fn add_decode(&mut self, spec: &str) -> Result<(), String> {
        let section: DecodeSection = spec.parse()?;
        litz_engine::decode::check(&section, self.board.borrow().probes())?;
        self.decodes.push((spec.to_string(), section));
        Ok(())
    }

    /// The board's display as RGB565, with its size and change counter.
    pub(crate) fn display(&mut self) -> Option<(u32, u32, Vec<u16>, u64)> {
        self.board_display(0)
    }

    /// The display's change counter, without building a frame.
    pub(crate) fn display_version(&mut self) -> u64 {
        self.board_display_version(0)
    }

    /// One part's display as RGBA8888, with its size and change counter.
    ///
    /// The board hands back a [`litz_board::Framebuffer`], which is RGB8 for
    /// every whole-frame model. Reading it through `pixel()` gives one RGBA
    /// path for all of them, in the channel order the PNG writer uses. `None`
    /// when the id names no part with a framebuffer.
    pub(crate) fn part_display(&mut self, part: &str) -> Option<(u32, u32, Vec<u8>, u64)> {
        let board = self.board.borrow();
        let framebuffer = board.part_framebuffer(part)?;
        let version = board.part_framebuffer_version(part);
        let width = framebuffer.width();
        let height = framebuffer.height();
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&framebuffer.pixel(x, y).unwrap_or([0, 0, 0, 255]));
            }
        }
        Some((width, height, pixels, version))
    }

    /// One part's display change counter, without copying a frame.
    pub(crate) fn part_display_version(&mut self, part: &str) -> u64 {
        self.board.borrow().part_framebuffer_version(part)
    }

    /// Touch the panel at raw controller codes. False when the board has no
    /// touch controller, because a finger on a panel that is not there changes
    /// nothing and the host should not be told otherwise.
    pub(crate) fn touch(&mut self, x: u16, y: u16, down: bool) -> bool {
        self.board.borrow_mut().touch_panel(x, y, down).is_ok()
    }

    pub(crate) fn scope_json(&self) -> String {
        let board = self.board.borrow();
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
        self.board.borrow().vcd()
    }

    pub(crate) fn take_serial(&mut self, max: usize) -> Vec<u8> {
        let n = max.min(self.serial.len());
        self.serial.drain(..n).collect()
    }

    pub(crate) fn conflict_count(&self) -> u32 {
        self.chips[0].conflicts().len() as u32
    }

    /// Text for one conflict. A fault that belongs to no particular net (a
    /// board that would not settle) is reported without the `net` prefix.
    pub(crate) fn conflict_text(&self, index: u32) -> Option<String> {
        let conflicts = self.chips[0].conflicts();
        let conflict = conflicts.get(index as usize)?;
        Some(if conflict.net == NO_NET {
            format!("board: {}", conflict.detail)
        } else {
            format!("net {}: {}", conflict.net, conflict.detail)
        })
    }

    /// Part-state code for the ABI, or `-1` when the diagram has no such part.
    pub(crate) fn state_of(&self, id: &str) -> i32 {
        match self.chips[0].state(id) {
            None => -1,
            Some(state) => state_code(state),
        }
    }

    /// The number behind a state code, or `0` when the state has none.
    pub(crate) fn value_of(&self, id: &str) -> f64 {
        self.chips[0].state(id).map(state_value).unwrap_or(0.0)
    }

    /// Number of parts the diagram declares. Board-expanded parts such as
    /// `esp/led_pwr` are readable by id but are not enumerated here.
    pub(crate) fn part_count(&self) -> u32 {
        self.diagram.parts.len() as u32
    }

    pub(crate) fn part_id(&self, index: u32) -> Option<String> {
        self.diagram
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
        if self.chips[0].state(id).is_none() {
            return Err(format!("no part {id:?} in the diagram"));
        }
        if self.chips[0].press(id, pressed) {
            Ok(())
        } else {
            Err(format!(
                "part {id:?} does not take a press; it is not a switch"
            ))
        }
    }

    /// Move a continuous control such as a potentiometer's knob.
    pub(crate) fn set_control(&mut self, id: &str, permille: u16) -> Result<(), String> {
        if self.chips[0].state(id).is_none() {
            return Err(format!("no part {id:?} in the diagram"));
        }
        if self.chips[0].set_control(id, permille) {
            Ok(())
        } else {
            Err(format!("part {id:?} has no continuous control"))
        }
    }

    /// Set a named control (`temperature`, `lux`), which is how a scenario
    /// drives a sensor that has no knob. A board id is a control too: `power`
    /// is its power switch, so it is tried before the part lookup that would
    /// otherwise report "no part".
    pub(crate) fn set_named_control(
        &mut self,
        id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        if self.chips[0].set_named_control(id, name, value) {
            return Ok(());
        }
        if self.chips[0].state(id).is_none() {
            return Err(format!("no part {id:?} in the diagram"));
        }
        Err(format!("part {id:?} has no control {name:?}"))
    }

    /// Put a host-supplied file tree on a storage part's FAT volume.
    pub(crate) fn set_storage_files(
        &mut self,
        part: &str,
        files: &[(String, Vec<u8>)],
    ) -> Result<(), String> {
        self.board.borrow_mut().set_storage_files(part, files)
    }

    /// Every file on a storage part's volume, as the guest left it.
    pub(crate) fn storage_files(&mut self, part: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.board.borrow_mut().storage_files(part)
    }

    /// The MCU's own flash as the run left it, for a host that wants to keep
    /// what the firmware wrote.
    pub(crate) fn flash_image(&self, board: usize) -> Option<Vec<u8>> {
        self.chips.get(board)?.flash_image()
    }

    /// The level on one of a part's own pins, when the part has it.
    pub(crate) fn part_pin_level(&self, id: &str, pin: Option<&str>) -> Option<litz_core::Level> {
        self.chips[0].part_pin_level(id, pin)
    }

    /// The level the chip sees on one of its own pads, by GPIO number.
    pub(crate) fn gpio_level(&self, pin: u8) -> Option<litz_core::Level> {
        self.chips[0].gpio_level(pin)
    }

    /// The firmware's own free-heap figure, when the backend can ask it.
    pub(crate) fn sample_heap(&mut self) -> Result<u32, String> {
        self.chips[0].sample_heap()
    }

    /// Bytes typed into the serial monitor, straight into the console's
    /// receive path.
    pub(crate) fn write_serial(&mut self, data: &[u8]) {
        self.chips[0].write_console(data);
    }
}

/// Rank stops so a trap on one board outranks a budget stop on another.
fn stop_rank(stop: Stop) -> u8 {
    match stop {
        Stop::Budget => 0,
        Stop::Idle => 1,
        Stop::Halted => 2,
        Stop::Breakpoint(_) => 3,
        Stop::Reset => 4,
        Stop::Trap => 5,
    }
}

/// The ABI's stop code: 0 budget, 1 idle, 2 halted, 3 trap, 4 reset,
/// 5 breakpoint.
fn stop_code(stop: Stop) -> u32 {
    match stop {
        Stop::Budget => 0,
        Stop::Idle => 1,
        Stop::Halted => 2,
        Stop::Trap => 3,
        Stop::Reset => 4,
        Stop::Breakpoint(_) => 5,
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

/// A session driven the way a browser drives it: a fixed step budget per
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

    const PICO: &str = r#"{
      "version": 1,
      "parts": [
        {"id": "pico", "type": "board-raspberry-pi-pico", "attrs": {}},
        {"id": "r1", "type": "litz-resistor", "attrs": {"value": "330"}},
        {"id": "led1", "type": "litz-led", "attrs": {"color": "green"}}
      ],
      "connections": [
        ["pico:GP15", "r1:1", "", []],
        ["r1:2", "led1:A", "", []],
        ["led1:C", "pico:GND.1", "", []],
        ["pico:GP0", "$serialMonitor:RX", "", []]
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

    #[test]
    fn a_pico_diagram_boots_the_rp2040_backend() {
        let app = litz_fixture::pico_blink(15, 50);
        let mut s = Session::new(PICO, &[], &[], &[], &app).expect("pico session");
        let info = s.chip_info_json();
        assert!(info.contains(r#""name":"rp2040""#), "{info}");
        let mut serial = String::new();
        for _ in 0..600 {
            s.run(100_000);
            serial.push_str(&String::from_utf8_lossy(&s.take_serial(SERIAL_CAP)));
            if serial.contains("litz: pico") {
                break;
            }
        }
        assert!(serial.contains("litz: pico"), "serial was {serial:?}");
    }

    #[test]
    fn the_chip_and_its_pins_are_readable() {
        let s = session(BUTTON);
        let info = s.chip_info_json();
        assert!(info.contains(r#""name":"esp32c3""#), "{info}");
        assert!(info.contains(r#""app":65536"#), "{info}");
        assert!(
            s.gpio_level(0).is_some(),
            "GPIO0 is wired to the button, so the chip sees it"
        );
        assert!(s.part_pin_level("nope", None).is_none());
    }

    /// The browser debug calls, answered by two different backends through the
    /// same ABI: a RISC-V ESP32-C3 and the RP2040. A page that talks to one
    /// talks to the other.
    #[test]
    fn the_debug_abi_answers_on_the_esp_and_the_pico() {
        let pico = Session::new(PICO, &[], &[], &[], &litz_fixture::pico_blink(15, 50))
            .expect("pico session");
        for mut s in [session(BUTTON), pico] {
            assert!(s.debug_available(0), "every backend answers the debug call");
            let pc = s.debug_pc(0).expect("a stopped core has a pc");
            let file = s.debug_registers(0).expect("the register file is 33 words");
            assert_eq!(file[32], pc, "the pc is repeated last");
            assert_eq!(
                s.debug_memory(0, pc, 4).map(|bytes| bytes.len()),
                Some(4),
                "the pc is in memory the core can read"
            );
            assert!(
                s.debug_disassemble(0, pc, 2)
                    .is_some_and(|text| text.contains(':')),
                "a listing names the address it decodes"
            );
            assert!(
                s.debug_set_breakpoints(0, &[pc]),
                "the backend takes the breakpoint set"
            );
            s.debug_set_breakpoints(0, &[]);
            assert!(s.debug_step(0).is_some(), "one step reports why it ended");
        }
    }

    #[test]
    fn a_stop_is_reported_for_the_last_run() {
        let mut s = session(BLINKY);
        s.run(1_000);
        assert_eq!(s.stop_code(), 0, "a budget stop");
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

/// A level as the scope JSON and the pin exports encode it: low, high, high
/// impedance.
pub(crate) fn level_code(level: litz_core::Level) -> i32 {
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

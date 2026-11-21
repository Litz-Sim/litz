//! WebAssembly C ABI for Litz.
//!
//! A plain HTML page loads this module with `WebAssembly.instantiate` and
//! talks to the exports below; there is no wasm-bindgen layer and nothing in
//! the core knows about JavaScript. The same exports work from a Node host or
//! under WASI, because the contract is only linear memory and integers.
//!
//! Pointers are byte offsets into the module's linear memory. A host allocates
//! a buffer with [`litz_alloc`], writes the bytes, passes `(ptr, len)` to an
//! export, and releases the buffer with [`litz_free`]. Strings are UTF-8;
//! output strings are truncated to the caller's capacity and the number of
//! bytes actually written is returned.
//!
//! | export | signature | contract |
//! | --- | --- | --- |
//! | [`litz_alloc`] | `(len: u32) -> u32` | allocate `len` bytes; `0` on failure |
//! | [`litz_free`] | `(ptr: u32, len: u32)` | release a buffer from `litz_alloc` |
//! | [`litz_new`] | `(diagram, rom, boot, ptable, app) -> u32` | build a session; `0` on failure |
//! | [`litz_drop`] | `(handle: u32)` | free a session's slot |
//! | [`litz_last_error`] | `(dst: u32, cap: u32) -> u32` | UTF-8 text of the last failure |
//! | [`litz_run`] | `(handle: u32, steps: u32) -> u32` | advance, return simulated ms |
//! | [`litz_ms`] | `(handle: u32) -> u32` | simulated ms since reset |
//! | [`litz_conflict_count`] | `(handle: u32) -> u32` | electrical faults on the board |
//! | [`litz_conflict`] | `(handle, index, dst, cap) -> u32` | text of one fault |
//! | [`litz_serial_take`] | `(handle, dst, cap) -> u32` | drain up to `cap` serial bytes |
//! | [`litz_part_state`] | `(handle, id, id_len) -> i32` | `-1` unknown, `0` none, `1`/`2` LED off/on, `3`/`4` button up/down |
//! | [`litz_press`] | `(handle, id, id_len, pressed: u32)` | press/release a pushbutton |
//! | [`litz_set_control`] | `(handle, id, id_len, permille: u32)` | turn a knob or a potentiometer, 0–1000 |
//! | [`litz_part_value`] | `(handle, id, id_len) -> f64` | a control's current value |
//! | [`litz_serial_write`] | `(handle, ptr, len)` | bytes from the host terminal into UART0 RX |
//! | [`litz_trace_start`] | `(handle: u32)` | begin recording net transitions |
//! | [`litz_trace_len`] | `(handle: u32) -> u32` | recorded transitions |
//! | [`litz_trace_vcd`] | `(handle, dst, cap) -> u32` | the recording as a VCD |
//! | [`litz_probe_add`] | `(handle, name, name_len, point, point_len) -> i32` | watch a point; `1` on success |
//! | [`litz_scope_len`] | `(handle: u32) -> u32` | bytes of the scope document |
//! | [`litz_scope_json`] | `(handle, dst, cap) -> u32` | every probe's record as JSON |
//! | [`litz_decode_add`] | `(handle, spec, len) -> i32` | decode a protocol off the probes; `1` on success |
//! | [`litz_part_count`] | `(handle: u32) -> u32` | parts declared in the diagram |
//! | [`litz_part_id`] | `(handle, index, dst, cap) -> u32` | id of the `index`-th part |
//! | [`litz_display_info`] | `(handle: u32) -> u32` | `width << 16 \| height`, or 0 without a display |
//! | [`litz_display_version`] | `(handle: u32) -> u32` | display change counter |
//! | [`litz_display_rgba`] | `(handle, dst, cap) -> u32` | the frame as RGBA8888, bytes written |
//! | [`litz_touch`] | `(handle, x, y, down) -> i32` | touch panel pixels; `1` on success |
//!
//! A session owns one ESP32-C3, its flattened board and its flash. `litz_run`
//! advances it by at most `steps` scheduling steps — the host decides how much
//! simulated time one call should cover — and returns the simulated clock in
//! milliseconds, saturating at `u32::MAX`. Serial output is collected
//! automatically and capped at 1 MiB, oldest bytes first, so a firmware that
//! prints in a loop cannot exhaust the host.
//!
//! Every failing export stores a message in a thread-local string; read it
//! with [`litz_last_error`], which does not clear it. The other exports clear
//! it when they succeed. Session handles are indices into a thread-local
//! table, with 0 reserved as "no session", so a handle is valid until
//! [`litz_drop`].
// One module opts back into `unsafe`: raw host pointers cannot be handled
// otherwise. A crate-level `forbid(unsafe_code)` cannot be relaxed in a child
// module, so the boundary uses `deny` plus one documented `allow`.
#![deny(unsafe_code)]

mod memory;
mod session;

use std::cell::RefCell;

use session::Session;

thread_local! {
    /// Live sessions. Index 0 is never occupied: handle 0 means "none".
    static SESSIONS: RefCell<Vec<Option<Session>>> = const { RefCell::new(Vec::new()) };
    /// The message the next [`litz_last_error`] returns.
    static LAST_ERROR: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Allocate `len` bytes of host-visible linear memory.
///
/// Returns a byte offset, or 0 when the allocation cannot be made. A
/// zero-length request returns a non-null offset so the host can form an empty
/// typed array from it; freeing that pointer with length 0 does nothing.
/// The C ABI itself. Everything here is `extern "C"` and reached by the
/// host through the module's export table; the `allow` is for
/// `#[unsafe(no_mangle)]`, which edition 2024 counts as unsafe code because
/// overriding a symbol is the linker's business.
pub mod abi {
    #![allow(unsafe_code)]

    use super::*;

    #[unsafe(no_mangle)]
    pub extern "C" fn litz_alloc(len: u32) -> u32 {
        let ptr = memory::alloc(len);
        if ptr == 0 {
            set_error(format!("allocation of {len} bytes failed"));
        }
        ptr
    }

    /// Release a buffer previously returned by [`litz_alloc`].
    ///
    /// `len` must be the same length that was allocated. Freeing the same buffer
    /// twice, or with a different length, is outside the contract.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_free(ptr: u32, len: u32) {
        memory::free(ptr, len);
    }

    /// Build a session from a diagram and firmware images.
    ///
    /// `rom`, `boot` (the second-stage bootloader) and `ptable` (the partition
    /// table) may each be absent by passing `(0, 0)`; the application is required.
    /// With a bootloader and partition table the images are placed at the ESP-IDF
    /// offsets and the bootloader runs; without them the application is flashed at
    /// 0x1_0000 and started directly. Supplying only one of the bootloader and
    /// partition table is an error, because the bootloader cannot find the
    /// application without the table.
    ///
    /// Returns a session handle, or 0 on failure with the reason in
    /// [`litz_last_error`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_new(
        diagram_ptr: u32,
        diagram_len: u32,
        rom_ptr: u32,
        rom_len: u32,
        boot_ptr: u32,
        boot_len: u32,
        ptable_ptr: u32,
        ptable_len: u32,
        app_ptr: u32,
        app_len: u32,
    ) -> u32 {
        clear_error();
        let diagram = match memory::to_string(diagram_ptr, diagram_len, "diagram") {
            Ok(text) => text,
            Err(message) => return error_u32(message),
        };
        let rom = memory::to_vec(rom_ptr, rom_len);
        let bootloader = memory::to_vec(boot_ptr, boot_len);
        let ptable = memory::to_vec(ptable_ptr, ptable_len);
        let app = memory::to_vec(app_ptr, app_len);

        match Session::new(&diagram, &rom, &bootloader, &ptable, &app) {
            Ok(session) => register(session),
            Err(message) => error_u32(message),
        }
    }

    /// Drop a session and release everything it holds.
    ///
    /// Dropping an already-free handle sets an error and changes nothing.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_drop(handle: u32) {
        clear_error();
        if handle == 0 {
            set_error(missing_session(handle));
            return;
        }
        let dropped = SESSIONS.with(|sessions| {
            let mut sessions = sessions.borrow_mut();
            match sessions.get_mut(handle as usize) {
                Some(slot) if slot.is_some() => {
                    *slot = None;
                    true
                }
                _ => false,
            }
        });
        if !dropped {
            set_error(missing_session(handle));
        }
    }

    /// Copy the last error message into the host buffer, truncated to `cap`.
    ///
    /// Returns the number of bytes written. The message is not cleared; a
    /// successful call to any other export clears it.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_last_error(dst_ptr: u32, cap: u32) -> u32 {
        LAST_ERROR.with(|message| memory::copy_out(dst_ptr, cap, message.borrow().as_bytes()))
    }

    /// Advance the session by at most `steps` scheduling steps.
    ///
    /// Returns simulated milliseconds since reset. Calls that pass 0 may still
    /// deliver pending input; the return value is the clock either way.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_run(handle: u32, steps: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.run(steps)) {
            Some(ms) => ms,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Simulated milliseconds since reset.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_ms(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.ms()) {
            Some(ms) => ms,
            None => error_u32(missing_session(handle)),
        }
    }

    /// How many electrical conflicts the board currently reports.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_conflict_count(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.conflict_count()) {
            Some(count) => count,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy the text of conflict `index` into the host buffer.
    ///
    /// Returns the number of bytes written. An index past the end writes nothing,
    /// returns 0, and sets an error; check [`litz_conflict_count`] first.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_conflict(handle: u32, index: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.conflict_text(index)) {
            Some(Some(text)) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            Some(None) => error_u32(format!("no conflict at index {index}")),
            None => error_u32(missing_session(handle)),
        }
    }

    /// Drain up to `cap` bytes of UART0 output into the host buffer.
    ///
    /// Returns the number of bytes written; anything that did not fit stays
    /// queued, so the host can call again. A capacity of 0 drains nothing.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_serial_take(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        if !has_session(handle) {
            return error_u32(missing_session(handle));
        }
        if dst_ptr == 0 || cap == 0 {
            return 0;
        }
        match with_session(handle, |session| session.take_serial(cap as usize)) {
            Some(chunk) => memory::copy_out(dst_ptr, cap, &chunk),
            None => error_u32(missing_session(handle)),
        }
    }

    /// The board's display size as `width << 16 | height`, or 0 when the
    /// circuit has no display.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_display_info(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.display()) {
            Some(Some((width, height, _, _))) => (width << 16) | (height & 0xffff),
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// A counter that grows when the display's pixels change, so a host can
    /// skip a frame that did not move.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_display_version(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.display_version()) {
            Some(version) => version as u32,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy the display as RGBA8888, one byte per channel, into `dst`.
    /// Returns the bytes written; a buffer smaller than `width * height * 4`
    /// receives nothing and gets 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_display_rgba(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        if !has_session(handle) {
            return error_u32(missing_session(handle));
        }
        let Some(Some((width, height, pixels, _))) =
            with_session(handle, |session| session.display())
        else {
            return 0;
        };
        let needed = width as usize * height as usize * 4;
        if dst_ptr == 0 || (cap as usize) < needed {
            return 0;
        }
        let rgba = to_rgba(&pixels);
        memory::copy_out(dst_ptr, cap, &rgba)
    }

    /// Deliver a touch at panel pixels. Returns 1, or 0 when the board has no
    /// touch controller or the session is gone.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_touch(handle: u32, x: u32, y: u32, down: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| {
            session.touch(x as u16, y as u16, down != 0)
        }) {
            Some(true) => 1,
            Some(false) => 0,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Expand RGB565 to RGBA8888, replicating the high bits so 31 maps to 255.
    fn to_rgba(pixels: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(pixels.len() * 4);
        for &pixel in pixels {
            let r = ((pixel >> 11) & 0x1f) as u8;
            let g = ((pixel >> 5) & 0x3f) as u8;
            let b = (pixel & 0x1f) as u8;
            out.push((r << 3) | (r >> 2));
            out.push((g << 2) | (g >> 4));
            out.push((b << 3) | (b >> 2));
            out.push(0xff);
        }
        out
    }

    /// Begin recording net transitions for [`litz_trace_vcd`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_trace_start(handle: u32) {
        clear_error();
        if with_session(handle, |session| session.start_trace()).is_none() {
            let _ = error_u32(missing_session(handle));
        }
    }

    /// Number of recorded transitions, so a host can size its buffer.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_trace_len(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.trace_len()) {
            Some(len) => len as u32,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy the recorded waveform as a VCD. Returns the bytes written; the
    /// caller sizes its buffer from [`litz_trace_len`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_trace_vcd(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.trace_vcd()) {
            Some(text) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            None => error_u32(missing_session(handle)),
        }
    }

    /// Watch a point in the circuit for the scope panel.
    ///
    /// `name` is the channel label; `point` is `<board>:<GPIO>` (`esp:GPIO8`,
    /// `b:4`) or `<part>:<pin>` (`led1:A`). Returns 1 on success, 0 with the
    /// reason in [`litz_last_error`]. Recording starts with
    /// [`litz_trace_start`], which a host already calls before it runs.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_probe_add(
        handle: u32,
        name_ptr: u32,
        name_len: u32,
        point_ptr: u32,
        point_len: u32,
    ) -> i32 {
        clear_error();
        let name = match memory::to_string(name_ptr, name_len, "probe name") {
            Ok(name) => name,
            Err(message) => return error_i32(message),
        };
        let point = match memory::to_string(point_ptr, point_len, "probe point") {
            Ok(point) => point,
            Err(message) => return error_i32(message),
        };
        match with_session(handle, |session| session.add_probe(&name, &point)) {
            Some(Ok(())) => 1,
            Some(Err(message)) => error_i32(message),
            None => error_i32(missing_session(handle)),
        }
    }

    /// Byte length of the scope document, so the host can size its buffer.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_scope_len(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.scope_json().len()) {
            Some(len) => len.min(u32::MAX as usize) as u32,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Every probe's record as JSON: name, point, measurements, and the
    /// `[tick_ns, value]` samples (levels as 0/1/2, volts as f64), followed by
    /// the frames of every decoder added with [`litz_decode_add`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_scope_json(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.scope_json()) {
            Some(text) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            None => error_u32(missing_session(handle)),
        }
    }

    /// Add a protocol decoder over the probed channels.
    ///
    /// The spec is the same text `--decode` takes:
    /// `uart:rx=tx[,baud=N]`, `i2c:sda=a,scl=b`, or
    /// `spi:sck=c,mosi=d,cs=e[,miso=f][,mode=N]`. Every channel must already
    /// be a probe. Returns 1 on success, 0 with the reason in
    /// [`litz_last_error`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_decode_add(handle: u32, spec_ptr: u32, spec_len: u32) -> i32 {
        clear_error();
        let spec = match memory::to_string(spec_ptr, spec_len, "decode spec") {
            Ok(spec) => spec,
            Err(message) => return error_i32(message),
        };
        match with_session(handle, |session| session.add_decode(&spec)) {
            Some(Ok(())) => 1,
            Some(Err(message)) => error_i32(message),
            None => error_i32(missing_session(handle)),
        }
    }

    /// State of one named part.
    ///
    /// `-1` means the diagram has no such part; `0` is a part with nothing to
    /// report; `1`/`2` are LED off/on; `3`/`4` are button up/down. Board-expanded
    /// ids such as `esp/led_pwr` are addressable here; the board instance itself
    /// has no runtime state, so its id reports `-1`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_state(handle: u32, id_ptr: u32, id_len: u32) -> i32 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => return error_i32(message),
        };
        match with_session(handle, |session| session.state_of(&id)) {
            Some(code) => code,
            None => error_i32(missing_session(handle)),
        }
    }

    /// The number behind a state code: servo degrees, a segment mask, a tone
    /// rate. Parts whose state is a flag report 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_value(handle: u32, id_ptr: u32, id_len: u32) -> f64 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(_) => return 0.0,
        };
        with_session(handle, |session| session.value_of(&id)).unwrap_or(0.0)
    }

    /// Press (`pressed != 0`) or release a named pushbutton.
    ///
    /// The level is applied to the chip pin the button is wired to, which is the
    /// same host input path the CLI uses. Releasing holds the pin at its idle
    /// level rather than floating it, which is enough for a pin the firmware reads
    /// as an input. A part that is not a pushbutton, or is not wired to a GPIO,
    /// sets an error and is left alone.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_press(handle: u32, id_ptr: u32, id_len: u32, pressed: u32) {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => {
                set_error(message);
                return;
            }
        };
        match with_session(handle, |session| session.press(&id, pressed != 0)) {
            Some(Ok(())) => {}
            Some(Err(message)) => set_error(message),
            None => set_error(missing_session(handle)),
        }
    }

    /// Move a continuous control such as a potentiometer, in permille of travel.
    ///
    /// Nothing is applied to the chip until the next `litz_run`: the wiper
    /// voltage is published to the ADC at the start of a run, which is the
    /// first moment firmware could sample it.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_set_control(handle: u32, id_ptr: u32, id_len: u32, permille: u32) {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => {
                set_error(message);
                return;
            }
        };
        let permille = permille.min(1000) as u16;
        match with_session(handle, |session| session.set_control(&id, permille)) {
            Some(Ok(())) => {}
            Some(Err(message)) => set_error(message),
            None => set_error(missing_session(handle)),
        }
    }

    /// Bytes the person typed into the serial monitor, delivered to UART0.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_serial_write(handle: u32, ptr: u32, len: u32) {
        clear_error();
        if len == 0 {
            return;
        }
        let data = memory::to_vec(ptr, len);
        match with_session(handle, |session| session.write_serial(&data)) {
            Some(()) => {}
            None => set_error(missing_session(handle)),
        }
    }

    /// How many parts the diagram declares.
    ///
    /// This is the top-level list, not the parts a board expands to; use the
    /// diagram's own ids to read board-internal state.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_count(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.part_count()) {
            Some(count) => count,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy the id of the `index`-th diagram part into the host buffer.
    ///
    /// Returns the number of bytes written, or 0 when `index` is past the end.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_id(handle: u32, index: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.part_id(index)) {
            Some(Some(id)) => memory::copy_out(dst_ptr, cap, id.as_bytes()),
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Run one TLS 1.3 handshake inside this module.
    ///
    /// This is the browser build's proof that an HTTPS service works there:
    /// `tools/tls-probe.mjs` instantiates the module with an empty import
    /// object and calls this export, which drives a client and a server through
    /// a request and checks both plaintexts. Returns the number of bytes that
    /// crossed the wire, or 0 with the reason in [`litz_last_error`].
    ///
    /// Only built with `--features tls-probe`, so the shipped module does not
    /// carry an export that has nothing to do with a diagram.
    #[cfg(feature = "tls-probe")]
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_tls_probe() -> u32 {
        clear_error();
        match tls_probe() {
            Ok(wire) => wire as u32,
            Err(message) => error_u32(message),
        }
    }
}

#[cfg(feature = "tls-probe")]
fn tls_probe() -> Result<usize, String> {
    use litz_net::tls;

    let config = tls::Config::new()?;
    let mut server = config.server()?;
    let mut client = config.client(tls::SERVER_NAME)?;
    let mut to_server = client.output();
    let mut to_client = Vec::new();
    let mut wire = Vec::new();
    let mut server_plain = Vec::new();
    let mut client_plain = Vec::new();
    let mut sent = false;
    for _ in 0..32 {
        if !to_server.is_empty() {
            wire.extend_from_slice(&to_server);
            server.feed(&to_server)?;
            to_server.clear();
            server_plain.extend(server.take_plain());
            to_client.extend(server.output());
        }
        if !to_client.is_empty() {
            wire.extend_from_slice(&to_client);
            client.feed(&to_client)?;
            to_client.clear();
            client_plain.extend(client.take_plain());
            to_server.extend(client.output());
        }
        if !client.handshaking() && !sent {
            to_server.extend(client.send(b"GET / HTTP/1.1\r\n\r\n"));
            sent = true;
        }
        if server_plain.len() >= 18 {
            to_client.extend(server.send(b"HTTP/1.1 200 OK\r\n\r\n"));
        }
        if client_plain.len() >= 19 {
            break;
        }
    }
    if server_plain != b"GET / HTTP/1.1\r\n\r\n" {
        return Err(format!(
            "the server read {:?}",
            String::from_utf8_lossy(&server_plain)
        ));
    }
    if client_plain != b"HTTP/1.1 200 OK\r\n\r\n" {
        return Err(format!(
            "the client read {:?}",
            String::from_utf8_lossy(&client_plain)
        ));
    }
    Ok(wire.len())
}

// -------------------------------------------------------------------- internals

/// Store an error message for [`litz_last_error`].
fn set_error(message: impl Into<String>) {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = message.into());
}

/// Forget the previous error; called by exports that can succeed.
fn clear_error() {
    LAST_ERROR.with(|slot| slot.borrow_mut().clear());
}

fn error_u32(message: impl Into<String>) -> u32 {
    set_error(message);
    0
}

fn error_i32(message: impl Into<String>) -> i32 {
    set_error(message);
    -1
}

fn missing_session(handle: u32) -> String {
    format!("no such session: handle {handle}")
}

/// Move a new session into the first free slot and return its handle.
///
/// Slot 0 stays empty so that 0 can mean failure. Slots are reused after a
/// drop, which keeps a page that creates and destroys sessions from growing
/// the table without bound.
fn register(session: Session) -> u32 {
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        if sessions.is_empty() {
            sessions.push(None);
        }
        let free = sessions
            .iter()
            .enumerate()
            .skip(1)
            .find_map(|(index, slot)| slot.is_none().then_some(index));
        match free {
            Some(index) => {
                sessions[index] = Some(session);
                index as u32
            }
            None => {
                sessions.push(Some(session));
                (sessions.len() - 1) as u32
            }
        }
    })
}

/// Whether the handle names a live session.
fn has_session(handle: u32) -> bool {
    if handle == 0 {
        return false;
    }
    SESSIONS.with(|sessions| {
        sessions
            .borrow()
            .get(handle as usize)
            .is_some_and(|slot| slot.is_some())
    })
}

/// Run `f` against a live session, or `None` when the handle is not one.
fn with_session<R>(handle: u32, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    if handle == 0 {
        return None;
    }
    SESSIONS.with(|sessions| {
        let mut sessions = sessions.borrow_mut();
        sessions.get_mut(handle as usize)?.as_mut().map(f)
    })
}

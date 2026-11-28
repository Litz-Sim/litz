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
//! | [`litz_new_json`] | `(json_ptr, json_len) -> u32` | build a session from images, options and manifests |
//! | [`litz_part_count`] | `(handle: u32) -> u32` | parts declared in the diagram |
//! | [`litz_part_id`] | `(handle, index, dst, cap) -> u32` | id of the `index`-th part |
//! | [`litz_chip_info`] | `(handle, dst, cap) -> u32` | the backend's name, clock and flash layout as JSON |
//! | [`litz_stop`] | `(handle: u32) -> u32` | how the last run ended |
//! | [`litz_fault`] | `(handle, dst, cap) -> u32` | the backend's trap text |
//! | [`litz_part_pin_level`] | `(handle, id, id_len, pin, pin_len) -> i32` | a part pin's level |
//! | [`litz_part_voltage`] | `(handle, id, id_len, pin, pin_len) -> f64` | a part pin's volts, `NaN` when floating |
//! | [`litz_part_current`] | `(handle, id, id_len) -> f64` | a part's current in amps, `NaN` when unsolved |
//! | [`litz_gpio_level`] | `(handle, pin: u32) -> i32` | a chip pad's level |
//! | [`litz_set_named_control`] | `(handle, id, name, value) -> i32` | set `temperature`, `lux`, ... |
//! | [`litz_heap`] | `(handle: u32) -> i32` | the firmware's free heap, or -1 |
//! | [`litz_guest_console`] | `(chip, ptr, len)` | console bytes from a host chip's run |
//! | [`litz_guest_fault`] | `(chip, ptr, len)` | a host chip's stop reason |
//! | [`litz_guest_call`] | `(chip, op, args, out, cap) -> u32` | one 0.2 host call, answered as JSON |
//! | [`litz_display_info`] | `(handle: u32) -> u32` | `width << 16 \| height`, or 0 without a display |
//! | [`litz_display_version`] | `(handle: u32) -> u32` | display change counter |
//! | [`litz_display_rgba`] | `(handle, dst, cap) -> u32` | the frame as RGBA8888, bytes written |
//! | [`litz_part_display_info`] | `(handle, id, id_len) -> u32` | `width << 16 \| height` of a part's panel, or 0 |
//! | [`litz_part_display_version`] | `(handle, id, id_len) -> u32` | a part's panel change counter |
//! | [`litz_part_display_rgba`] | `(handle, id, id_len, dst, cap) -> u32` | a part's frame as RGBA8888, bytes written |
//! | [`litz_touch`] | `(handle, x, y, down) -> i32` | touch panel pixels; `1` on success |
//!
//! A session owns one board, its flattened circuit and its flash. The backend
//! is chosen from the board the diagram names: every builtin chip boots this
//! way, and a board whose manifest names a wasm component is hosted by the
//! JavaScript side and reached through the `litz_host` imports. `litz_run`
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

/// Give a host-instantiated `litz:mcu@0.2.0` chip its circuit side. The
/// factory builds the same `HostState` the native loader uses, so bus, ADC,
/// Wi-Fi and BLE answers do not depend on which host runs the component.
#[cfg(target_arch = "wasm32")]
fn ensure_guest_factory() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        litz_board::host::set_guest_factory(|_module, board, index| {
            Some(Box::new(litz_mcu_wasm::HostState::attached(board, index)))
        });
    });
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

    use serde::Deserialize;

    use super::*;
    use crate::session::level_code;

    /// The JSON `litz_new_json` takes.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SessionSpec {
        diagram: String,
        #[serde(default)]
        app: Option<ImageRef>,
        #[serde(default)]
        boards: Vec<BoardSpec>,
        #[serde(default)]
        rom: Option<ImageRef>,
        #[serde(default)]
        boot: Option<ImageRef>,
        #[serde(default)]
        ptable: Option<ImageRef>,
        #[serde(default)]
        flash_size: Option<u32>,
        #[serde(default)]
        mac: Option<String>,
        #[serde(default)]
        analog: Option<bool>,
        /// Catalog manifests as TOML text, or `{toml, schematic}` for a board
        /// whose internal schematic the host also supplies.
        #[serde(default)]
        manifests: Vec<ManifestSpec>,
        /// Files to format onto a storage part's FAT volume, keyed by the
        /// part's diagram id. The browser's counterpart of the `files`
        /// attribute, which names a directory the module cannot read.
        #[serde(default)]
        storage: std::collections::BTreeMap<String, Vec<StorageFileSpec>>,
    }

    /// One file for a storage part's volume.
    #[derive(Deserialize)]
    struct StorageFileSpec {
        path: String,
        data: ImageRef,
    }

    /// One development board's images for a multi-board session.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct BoardSpec {
        #[serde(default)]
        id: Option<String>,
        app: ImageRef,
        #[serde(default)]
        rom: Option<ImageRef>,
        #[serde(default)]
        boot: Option<ImageRef>,
        #[serde(default)]
        ptable: Option<ImageRef>,
        #[serde(default)]
        flash_size: Option<u32>,
        #[serde(default)]
        mac: Option<String>,
    }

    /// One board's images, owned so the fleet can borrow them after the
    /// JSON spec has been consumed.
    struct OwnedBoard {
        id: Option<String>,
        app: Vec<u8>,
        rom: Vec<u8>,
        boot: Vec<u8>,
        ptable: Vec<u8>,
        flash_size: Option<usize>,
        mac: Option<[u8; 6]>,
    }

    /// A manifest the host handed over.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum ManifestSpec {
        Text(String),
        Bundle {
            toml: String,
            #[serde(default)]
            schematic: Option<String>,
        },
    }

    /// A host buffer named by a byte offset and a length.
    #[derive(Deserialize)]
    struct ImageRef {
        ptr: u32,
        len: u32,
    }

    /// `24:0a:c4:12:34:56` or `24-0a-c4-12-34-56`.
    fn parse_mac(text: &str) -> Result<[u8; 6], String> {
        let octets: Vec<u8> = text
            .split([':', '-'])
            .map(|octet| {
                u8::from_str_radix(octet, 16)
                    .map_err(|_| format!("invalid MAC {text:?}: expected six hex octets"))
            })
            .collect::<Result<_, _>>()?;
        octets
            .try_into()
            .map_err(|_| format!("invalid MAC {text:?}: expected six hex octets"))
    }

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
    /// The backend is chosen from the board the diagram names, so every
    /// builtin chip boots the same way: the ESP32 family, the AVR parts, the
    /// RP2040, the STC89C52RC and the CH32V003.
    ///
    /// With a bootloader and partition table the images are placed at the
    /// chip's own offsets and the bootloader runs; without them the
    /// application is placed at the application offset, and a merged ESP image
    /// is recognised and written at the bootloader offset. Supplying only one
    /// of the bootloader and partition table is an error, because the
    /// bootloader cannot find the application without the table.
    ///
    /// This entry point uses default flash size, MAC and analog settings.
    /// [`litz_new_json`] takes those from the host.
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

    /// Build a session from a JSON document.
    ///
    /// The document names the images by their buffers in linear memory, so the
    /// host writes them with [`litz_alloc`] first and frees them afterwards:
    ///
    /// ```json
    /// {
    ///   "diagram": "{ ... }",
    ///   "app":  { "ptr": 1024, "len": 131072 },
    ///   "rom":  { "ptr": 4096, "len": 65536 },
    ///   "boot": null,
    ///   "ptable": null,
    ///   "flashSize": 4194304,
    ///   "mac": "24:0a:c4:12:34:56",
    ///   "analog": false
    /// }
    /// ```
    ///
    /// `app` is required; `rom`, `boot`, `ptable`, `flashSize`, `mac` and
    /// `analog` are optional and fall back to the board's manifest and the
    /// module's defaults. Unknown keys are an error so a typo is reported
    /// rather than silently ignored.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_new_json(json_ptr: u32, json_len: u32) -> u32 {
        clear_error();
        #[cfg(target_arch = "wasm32")]
        ensure_guest_factory();
        let text = match memory::to_string(json_ptr, json_len, "session options") {
            Ok(text) => text,
            Err(message) => return error_u32(message),
        };
        let spec: SessionSpec = match serde_json::from_str(&text) {
            Ok(spec) => spec,
            Err(error) => return error_u32(format!("session options: {error}")),
        };
        let SessionSpec {
            diagram,
            app,
            boards,
            rom,
            boot,
            ptable,
            flash_size,
            mac: mac_text,
            analog,
            manifests,
            storage,
        } = spec;
        let mac = match mac_text.as_deref().map(parse_mac) {
            None => litz_engine::machine::DEFAULT_MAC,
            Some(Ok(mac)) => mac,
            Some(Err(message)) => return error_u32(message),
        };
        if flash_size == Some(0) {
            return error_u32("flashSize must be positive");
        }
        let options = litz_engine::machine::BootOptions {
            flash_size: flash_size.map(|size| size as usize),
            mac,
            analog: analog.unwrap_or(false),
            manifests: manifests
                .into_iter()
                .map(|manifest| match manifest {
                    ManifestSpec::Text(toml) => litz_engine::machine::ManifestText {
                        toml,
                        schematic: None,
                    },
                    ManifestSpec::Bundle { toml, schematic } => {
                        litz_engine::machine::ManifestText { toml, schematic }
                    }
                })
                .collect(),
        };

        let session = if boards.is_empty() {
            let Some(app) = app else {
                return error_u32("no application image: app_len is 0");
            };
            let rom = rom
                .map(|r| memory::to_vec(r.ptr, r.len))
                .unwrap_or_default();
            let boot = boot
                .map(|r| memory::to_vec(r.ptr, r.len))
                .unwrap_or_default();
            let ptable = ptable
                .map(|r| memory::to_vec(r.ptr, r.len))
                .unwrap_or_default();
            let app = memory::to_vec(app.ptr, app.len);
            Session::with_options(
                &diagram,
                litz_engine::machine::Images {
                    rom: &rom,
                    bootloader: &boot,
                    partition_table: &ptable,
                    app: &app,
                },
                &options,
            )
        } else {
            let mut owned = Vec::with_capacity(boards.len());
            for spec in boards {
                let mac = match spec.mac.as_deref().map(parse_mac) {
                    None => None,
                    Some(Ok(mac)) => Some(mac),
                    Some(Err(message)) => return error_u32(message),
                };
                if spec.flash_size == Some(0) {
                    return error_u32("flashSize must be positive");
                }
                owned.push(OwnedBoard {
                    id: spec.id,
                    app: memory::to_vec(spec.app.ptr, spec.app.len),
                    rom: spec
                        .rom
                        .map(|r| memory::to_vec(r.ptr, r.len))
                        .unwrap_or_default(),
                    boot: spec
                        .boot
                        .map(|r| memory::to_vec(r.ptr, r.len))
                        .unwrap_or_default(),
                    ptable: spec
                        .ptable
                        .map(|r| memory::to_vec(r.ptr, r.len))
                        .unwrap_or_default(),
                    flash_size: spec.flash_size.map(|size| size as usize),
                    mac,
                });
            }
            let images: Vec<litz_engine::machine::BoardImage<'_>> = owned
                .iter()
                .map(|board| litz_engine::machine::BoardImage {
                    id: board.id.as_deref(),
                    app: &board.app,
                    rom: &board.rom,
                    bootloader: &board.boot,
                    partition_table: &board.ptable,
                    flash_size: board.flash_size,
                    mac: board.mac,
                })
                .collect();
            Session::with_fleet(&diagram, &images, &options)
        };
        match session {
            Ok(mut session) => {
                for (part, files) in storage {
                    let files: Vec<(String, Vec<u8>)> = files
                        .into_iter()
                        .map(|file| (file.path, memory::to_vec(file.data.ptr, file.data.len)))
                        .collect();
                    if let Err(message) = session.set_storage_files(&part, &files) {
                        return error_u32(message);
                    }
                }
                register(session)
            }
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

    /// One part's display size as `width << 16 | height`, or 0 when the id
    /// names no part with a whole-frame panel.
    ///
    /// `part` is the diagram id (`paper`, `oled`, `tft`), not the catalog type
    /// key. The covered engines are the ILI9341, ST7735, SSD1306, SH1106, the
    /// e-paper panels, a MAX7219 chain and the Nokia PCD8544.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_display_info(handle: u32, part_ptr: u32, part_len: u32) -> u32 {
        clear_error();
        let part = match memory::to_string(part_ptr, part_len, "part id") {
            Ok(part) => part,
            Err(message) => return error_u32(message),
        };
        match with_session(handle, |session| session.part_display(&part)) {
            Some(Some((width, height, _, _))) => (width << 16) | (height & 0xffff),
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// One part's display change counter, so a host can skip a frame that did
    /// not move. 0 when the id names no part with a framebuffer.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_display_version(handle: u32, part_ptr: u32, part_len: u32) -> u32 {
        clear_error();
        let part = match memory::to_string(part_ptr, part_len, "part id") {
            Ok(part) => part,
            Err(message) => return error_u32(message),
        };
        match with_session(handle, |session| session.part_display_version(&part)) {
            Some(version) => version as u32,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy one part's display as RGBA8888, one byte per channel, into `dst`.
    /// Returns the bytes written; a buffer smaller than `width * height * 4`
    /// receives nothing and gets 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_display_rgba(
        handle: u32,
        part_ptr: u32,
        part_len: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        let part = match memory::to_string(part_ptr, part_len, "part id") {
            Ok(part) => part,
            Err(message) => return error_u32(message),
        };
        let Some(frame) = with_session(handle, |session| session.part_display(&part)) else {
            return error_u32(missing_session(handle));
        };
        let Some((width, height, pixels, _)) = frame else {
            return 0;
        };
        let needed = width as usize * height as usize * 4;
        if dst_ptr == 0 || (cap as usize) < needed {
            return 0;
        }
        memory::copy_out(dst_ptr, cap, &pixels)
    }

    /// A file list as one buffer: `u32` count, then per file a `u32` path
    /// length, the UTF-8 path, a `u32` data length and the bytes. JSON would
    /// inflate a card's files by an order of magnitude; this stays copyable
    /// and the host parses it in one pass.
    fn storage_frame(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (path, data) in files {
            out.extend_from_slice(&(path.len() as u32).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
        }
        out
    }

    /// Bytes of the file list [`litz_storage_files_copy`] would write, or 0
    /// with an error set when the part is not a readable storage part.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_storage_files_len(handle: u32, part_ptr: u32, part_len: u32) -> u32 {
        clear_error();
        let part = match memory::to_string(part_ptr, part_len, "part id") {
            Ok(part) => part,
            Err(message) => return error_u32(message),
        };
        match with_session(handle, |session| session.storage_files(&part)) {
            Some(Ok(files)) => storage_frame(&files).len() as u32,
            Some(Err(message)) => error_u32(message),
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy a storage part's files as the framing above into `dst`. Returns
    /// the bytes written; a buffer that is too small receives nothing.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_storage_files_copy(
        handle: u32,
        part_ptr: u32,
        part_len: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        let part = match memory::to_string(part_ptr, part_len, "part id") {
            Ok(part) => part,
            Err(message) => return error_u32(message),
        };
        let files = match with_session(handle, |session| session.storage_files(&part)) {
            Some(Ok(files)) => files,
            Some(Err(message)) => return error_u32(message),
            None => return error_u32(missing_session(handle)),
        };
        let frame = storage_frame(&files);
        if dst_ptr == 0 || (cap as usize) < frame.len() {
            return 0;
        }
        memory::copy_out(dst_ptr, cap, &frame)
    }

    /// Bytes in the MCU's own flash image for board `index`, or 0.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_flash_len(handle: u32, board: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.flash_image(board as usize)) {
            Some(Some(image)) => image.len() as u32,
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// Copy board `index`'s flash image into `dst`. Returns the bytes written.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_flash_copy(handle: u32, board: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        let image = match with_session(handle, |session| session.flash_image(board as usize)) {
            Some(Some(image)) => image,
            Some(None) => return 0,
            None => return error_u32(missing_session(handle)),
        };
        if dst_ptr == 0 || (cap as usize) < image.len() {
            return 0;
        }
        memory::copy_out(dst_ptr, cap, &image)
    }

    /// Number of development boards the session booted.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_count(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.board_count()) {
            Some(count) => count,
            None => error_u32(missing_session(handle)),
        }
    }

    /// The `diagram.json` id of board `index`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_id(handle: u32, index: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.board_id(index)) {
            Some(Some(id)) => memory::copy_out(dst_ptr, cap, id.as_bytes()),
            Some(None) => error_u32(format!("no board at index {index}")),
            None => error_u32(missing_session(handle)),
        }
    }

    /// One board's chip name, clock and flash layout as JSON.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_chip_info(handle: u32, index: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.board_chip_info_json(index)) {
            Some(Some(text)) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            Some(None) => error_u32(format!("no board at index {index}")),
            None => error_u32(missing_session(handle)),
        }
    }

    /// The level board `index` sees on one of its own pads.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_gpio_level(handle: u32, index: u32, pin: u32) -> i32 {
        clear_error();
        let Ok(pin) = u8::try_from(pin) else {
            return error_i32(format!("GPIO number {pin} is past the chip's last pin"));
        };
        match with_session(handle, |session| {
            session.board_gpio_level(index, pin).map(level_code)
        }) {
            Some(Some(code)) => code,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Drain one board's console; the merged stream is `litz_serial_take`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_serial_take_board(
        handle: u32,
        index: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        if !has_session(handle) {
            return error_u32(missing_session(handle));
        }
        if dst_ptr == 0 || cap == 0 {
            return 0;
        }
        match with_session(handle, |session| {
            session.take_board_serial(index, cap as usize)
        }) {
            Some(Some(chunk)) => memory::copy_out(dst_ptr, cap, &chunk),
            Some(None) => error_u32(format!("no board at index {index}")),
            None => error_u32(missing_session(handle)),
        }
    }

    /// Bytes typed into one board's console receive path.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_serial_write_board(handle: u32, index: u32, ptr: u32, len: u32) -> i32 {
        clear_error();
        let data = memory::to_vec(ptr, len);
        match with_session(handle, |session| session.write_serial_board(index, &data)) {
            Some(true) => 1,
            Some(false) => error_i32(format!("no board at index {index}")),
            None => error_i32(missing_session(handle)),
        }
    }

    /// One board's display size as `width << 16 | height`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_display_info(handle: u32, index: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.board_display(index)) {
            Some(Some((width, height, _, _))) => (width << 16) | (height & 0xffff),
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// One board's display change counter.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_display_version(handle: u32, index: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.board_display_version(index)) {
            Some(version) => version as u32,
            None => error_u32(missing_session(handle)),
        }
    }

    /// One board's display as RGBA8888.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_board_display_rgba(
        handle: u32,
        index: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        if !has_session(handle) {
            return error_u32(missing_session(handle));
        }
        let Some(Some((width, height, pixels, _))) =
            with_session(handle, |session| session.board_display(index))
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

    /// Whether board `index` has a browser-facing debugger.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_available(handle: u32, index: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| session.debug_available(index)) {
            Some(true) => 1,
            Some(false) => 0,
            None => error_i32(missing_session(handle)),
        }
    }

    /// The stopped core's program counter, or -1.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_pc(handle: u32, index: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| session.debug_pc(index)) {
            Some(Some(pc)) => pc as i32,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// 32 integer registers then pc, as little-endian u32s.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_registers(handle: u32, index: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        let Some(Some(file)) = with_session(handle, |session| session.debug_registers(index))
        else {
            return 0;
        };
        let needed = 33 * 4;
        if dst_ptr == 0 || (cap as usize) < needed {
            return 0;
        }
        let mut bytes = Vec::with_capacity(needed);
        for word in file {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        memory::copy_out(dst_ptr, cap, &bytes)
    }

    /// Bytes readable at `addr`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_memory(
        handle: u32,
        index: u32,
        addr: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        let Some(Some(bytes)) = with_session(handle, |session| {
            session.debug_memory(index, addr, cap as usize)
        }) else {
            return 0;
        };
        memory::copy_out(dst_ptr, cap, &bytes)
    }

    /// Disassemble `count` instructions at `addr`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_disassemble(
        handle: u32,
        index: u32,
        addr: u32,
        count: u32,
        dst_ptr: u32,
        cap: u32,
    ) -> u32 {
        clear_error();
        let Some(Some(text)) = with_session(handle, |session| {
            session.debug_disassemble(index, addr, count as usize)
        }) else {
            return 0;
        };
        memory::copy_out(dst_ptr, cap, text.as_bytes())
    }

    /// Replace the armed breakpoints with `count` addresses at `ptr`.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_breakpoints(handle: u32, index: u32, ptr: u32, count: u32) -> i32 {
        clear_error();
        let bytes = memory::to_vec(ptr, count.saturating_mul(4));
        if bytes.len() != count as usize * 4 {
            return error_i32("breakpoint list is truncated");
        }
        let pcs: Vec<u32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
        match with_session(handle, |session| session.debug_set_breakpoints(index, &pcs)) {
            Some(true) => 1,
            Some(false) => error_i32(format!("no board at index {index}")),
            None => error_i32(missing_session(handle)),
        }
    }

    /// Execute exactly one instruction on one board.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_step(handle: u32, index: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| session.debug_step(index)) {
            Some(Some(code)) => code as i32,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Run one board until a breakpoint or the step budget.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_debug_run(handle: u32, index: u32, steps: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| session.debug_run(index, steps)) {
            Some(Some(code)) => code as i32,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
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

    /// The backend the diagram booted, as JSON.
    ///
    /// `{"name":"esp32c3","cpuHz":160000000,"flash":{"size":4194304,
    /// "bootloader":0,"partitionTable":32768,"app":65536}}`. The chip name,
    /// its clock and where its images sit are what a host needs to label a
    /// run without repeating the board manifest.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_chip_info(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.chip_info_json()) {
            Some(text) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            None => error_u32(missing_session(handle)),
        }
    }

    /// How the last [`litz_run`] ended.
    ///
    /// `0` step budget, `1` idle (nothing to do until an input changes),
    /// `2` halted, `3` trap, `4` reset, `5` breakpoint.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_stop(handle: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.stop_code()) {
            Some(code) => code,
            None => error_u32(missing_session(handle)),
        }
    }

    /// The backend's own explanation for a trap or fault, if it has one.
    ///
    /// Returns the bytes written; an empty string when the chip has nothing
    /// to report.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_fault(handle: u32, dst_ptr: u32, cap: u32) -> u32 {
        clear_error();
        match with_session(handle, |session| session.fault()) {
            Some(Some(text)) => memory::copy_out(dst_ptr, cap, text.as_bytes()),
            Some(None) => 0,
            None => error_u32(missing_session(handle)),
        }
    }

    /// The level on one of a named part's pins.
    ///
    /// `pin` may be empty to ask for the part's first pin. `-1` means the
    /// diagram has no such part or pin; `0`/`1`/`2` are low, high and high
    /// impedance.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_pin_level(
        handle: u32,
        id_ptr: u32,
        id_len: u32,
        pin_ptr: u32,
        pin_len: u32,
    ) -> i32 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => return error_i32(message),
        };
        let pin = match memory::to_string(pin_ptr, pin_len, "pin name") {
            Ok(pin) => pin,
            Err(message) => return error_i32(message),
        };
        let pin = if pin.is_empty() {
            None
        } else {
            Some(pin.as_str())
        };
        match with_session(handle, |session| {
            session.part_pin_level(&id, pin).map(level_code)
        }) {
            Some(Some(code)) => code,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Voltage on one of a named part's pins, in volts, from the analog solve.
    ///
    /// `NaN` means the pin is floating, the part or pin does not exist, or the
    /// netlist did not solve: a floating node is not 0 V. This is the reading
    /// a voltmeter's `+` lead takes.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_voltage(
        handle: u32,
        id_ptr: u32,
        id_len: u32,
        pin_ptr: u32,
        pin_len: u32,
    ) -> f64 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => {
                set_error(message);
                return f64::NAN;
            }
        };
        let pin = match memory::to_string(pin_ptr, pin_len, "pin name") {
            Ok(pin) => pin,
            Err(message) => {
                set_error(message);
                return f64::NAN;
            }
        };
        if pin.is_empty() {
            set_error("part voltage needs a pin name");
            return f64::NAN;
        }
        with_session(handle, |session| session.part_voltage(&id, &pin))
            .flatten()
            .unwrap_or(f64::NAN)
    }

    /// Current through one of a named part's main element, in amperes.
    ///
    /// `NaN` when the part is not in the analog solve. The sign follows the
    /// part's own orientation: for a resistor (and either mode of a
    /// multimeter) positive current flows from its first pin to its second.
    /// This is the reading an ammeter takes when it sits in that path.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_part_current(handle: u32, id_ptr: u32, id_len: u32) -> f64 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => {
                set_error(message);
                return f64::NAN;
            }
        };
        with_session(handle, |session| session.part_current(&id))
            .flatten()
            .unwrap_or(f64::NAN)
    }

    /// The level the chip sees on one of its own pads, by GPIO number.
    ///
    /// `-1` means the chip has no such pad or is not attached to a board;
    /// otherwise `0`/`1`/`2` as in [`litz_part_pin_level`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_gpio_level(handle: u32, pin: u32) -> i32 {
        clear_error();
        let Ok(pin) = u8::try_from(pin) else {
            return error_i32(format!("GPIO number {pin} is past the chip's last pin"));
        };
        match with_session(handle, |session| session.gpio_level(pin).map(level_code)) {
            Some(Some(code)) => code,
            Some(None) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Set a named control on a part (`temperature`, `lux`, `pressed`).
    ///
    /// Returns 1 on success, 0 when the part has no such control, and -1 when
    /// the diagram has no such part. The reason is in [`litz_last_error`].
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_set_named_control(
        handle: u32,
        id_ptr: u32,
        id_len: u32,
        name_ptr: u32,
        name_len: u32,
        value_ptr: u32,
        value_len: u32,
    ) -> i32 {
        clear_error();
        let id = match memory::to_string(id_ptr, id_len, "part id") {
            Ok(id) => id,
            Err(message) => return error_i32(message),
        };
        let name = match memory::to_string(name_ptr, name_len, "control name") {
            Ok(name) => name,
            Err(message) => return error_i32(message),
        };
        let value = match memory::to_string(value_ptr, value_len, "control value") {
            Ok(value) => value,
            Err(message) => return error_i32(message),
        };
        match with_session(handle, |session| {
            session.set_named_control(&id, &name, &value)
        }) {
            Some(Ok(())) => 1,
            Some(Err(message)) => {
                let code = if message.starts_with("no part") {
                    -1
                } else {
                    0
                };
                set_error(message);
                code
            }
            None => error_i32(missing_session(handle)),
        }
    }

    /// The firmware's own free-heap figure, or -1 when this backend cannot
    /// call into the guest to ask.
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_heap(handle: u32) -> i32 {
        clear_error();
        match with_session(handle, |session| session.sample_heap()) {
            Some(Ok(bytes)) => bytes as i32,
            Some(Err(_)) => -1,
            None => error_i32(missing_session(handle)),
        }
    }

    /// Console bytes a host chip wrote while the module was inside its run.
    ///
    /// The host's chip adapter calls this from the guest's `host.console`
    /// import; the module is suspended in `chip_run`, so the bytes are
    /// buffered and the session drains them on its next `litz_serial_take`.
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_guest_console(chip: u32, ptr: u32, len: u32) {
        litz_board::host::push_console(chip, &memory::to_vec(ptr, len));
    }

    /// A host chip's explanation for stopping, from the guest's `host.fault`.
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_guest_fault(chip: u32, ptr: u32, len: u32) {
        let detail = String::from_utf8_lossy(&memory::to_vec(ptr, len)).into_owned();
        litz_board::host::push_fault(chip, detail);
    }

    /// One `litz:mcu@0.2.0` host call from a transpiled guest.
    ///
    /// `op` is the contract's camelCase function name and `args` a JSON
    /// object; the reply is a JSON envelope, `{"ok":true,"value":...}` or
    /// `{"ok":false,"error":"..."}`. The return value is the reply's full
    /// length, even when it exceeds `cap`, so a host that got a short buffer
    /// knows how much to allocate and calls again.
    #[cfg(target_arch = "wasm32")]
    #[unsafe(no_mangle)]
    pub extern "C" fn litz_guest_call(
        chip: u32,
        op_ptr: u32,
        op_len: u32,
        args_ptr: u32,
        args_len: u32,
        out_ptr: u32,
        cap: u32,
    ) -> u32 {
        let reply = match (
            memory::to_string(op_ptr, op_len, "host call name"),
            memory::to_string(args_ptr, args_len, "host call arguments"),
        ) {
            (Ok(op), Ok(args)) => match litz_board::host::guest_call(chip, &op, &args) {
                Ok(value) => format!(r#"{{"ok":true,"value":{value}}}"#),
                Err(error) => serde_json::json!({ "ok": false, "error": error }).to_string(),
            },
            (Err(error), _) | (_, Err(error)) => {
                serde_json::json!({ "ok": false, "error": error }).to_string()
            }
        };
        memory::copy_out(out_ptr, cap, reply.as_bytes());
        reply.len() as u32
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

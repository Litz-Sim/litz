//! The Python runtimes a firmware can speak, and the staging text pasted into
//! their REPLs.
//!
//! A board configured with a Python runtime boots a stock interpreter instead
//! of the project's own application. The runner never imports or executes the
//! guest's Python itself: it waits for the REPL prompt, sends the raw-REPL
//! control bytes and pastes a program, so everything here is pure bytes and
//! strings. Keeping it separate from the run loop is what makes the
//! handshake testable without booting a chip.
//!
//! MicroPython and CircuitPython share the friendly-REPL prompt and the raw
//! paste framing. They differ in the file the interpreter runs on boot —
//! CircuitPython looks for `code.py`, MicroPython for `main.py` — and in
//! whether a program may already be running when the paste arrives.

use crate::config::Runtime;

/// The friendly-REPL prompt both runtimes print. It is the only sign the
/// interpreter is ready; matching a banner would tie the runner to one build's
/// wording.
pub const REPL_PROMPT: &str = ">>> ";

/// Which interpreter a board's console speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PythonRuntime {
    MicroPython,
    CircuitPython,
}

impl PythonRuntime {
    /// The bytes that interrupt a running program, if the runtime can be
    /// running one. CircuitPython starts `code.py` itself, so its REPL is busy
    /// until Ctrl-C is sent; a stock MicroPython boot lands at the prompt with
    /// nothing to interrupt.
    pub fn interrupt_sequence(self) -> &'static [u8] {
        match self {
            PythonRuntime::MicroPython => &[],
            PythonRuntime::CircuitPython => &[0x03],
        }
    }

    /// The entry file the runtime executes on boot.
    pub fn entry_name(self) -> &'static str {
        match self {
            PythonRuntime::MicroPython => "main.py",
            PythonRuntime::CircuitPython => "code.py",
        }
    }

    /// The entry names the runtime accepts, in preference order.
    ///
    /// CircuitPython falls back to `main.py` when a project stages that name
    /// instead, so a project choosing the MicroPython convention still runs.
    pub fn entry_candidates(self) -> &'static [&'static str] {
        match self {
            PythonRuntime::MicroPython => &["main.py"],
            PythonRuntime::CircuitPython => &["code.py", "main.py"],
        }
    }

    /// Ctrl-A enters MicroPython's raw REPL, where the interpreter does not
    /// echo or line-edit, so the pasted bytes arrive exactly as written.
    pub fn raw_paste_begin(self) -> &'static [u8] {
        &[0x01]
    }

    /// Ctrl-D compiles and runs the raw paste. Both runtimes use the same
    /// framing; the parameter keeps the staging sequence one shape at the call
    /// site even though the bytes do not differ.
    pub fn raw_paste_end(self) -> &'static [u8] {
        &[0x04]
    }
}

/// `PythonRuntime::entry_name` as a free function, so a call site that already
/// holds the runtime can read `entry_name(runtime)`.
pub fn entry_name(runtime: PythonRuntime) -> &'static str {
    runtime.entry_name()
}

/// `PythonRuntime::entry_candidates` as a free function.
pub fn entry_candidates(runtime: PythonRuntime) -> &'static [&'static str] {
    runtime.entry_candidates()
}

/// `PythonRuntime::interrupt_sequence` as a free function.
pub fn interrupt_sequence(runtime: PythonRuntime) -> &'static [u8] {
    runtime.interrupt_sequence()
}

/// `PythonRuntime::raw_paste_begin` as a free function.
pub fn raw_paste_begin(runtime: PythonRuntime) -> &'static [u8] {
    runtime.raw_paste_begin()
}

/// `PythonRuntime::raw_paste_end` as a free function.
pub fn raw_paste_end(runtime: PythonRuntime) -> &'static [u8] {
    runtime.raw_paste_end()
}

/// Which interpreter a board's console speaks, from the configured runtime and
/// a firmware hint: its file name or a captured banner.
///
/// An explicit `runtime = "circuitpython"` wins. Otherwise a hint that names
/// CircuitPython wins over the MicroPython default, because the image is what
/// will actually boot; a MicroPython build or an image that says nothing about
/// CircuitPython stays MicroPython.
pub fn detect(runtime: Runtime, hint: &str) -> PythonRuntime {
    detect_bytes(runtime, hint.as_bytes())
}

/// `detect` for a firmware image or a console banner the caller already holds
/// as bytes. The scan is case-insensitive and does not require the hint to be
/// UTF-8.
pub fn detect_bytes(runtime: Runtime, hint: &[u8]) -> PythonRuntime {
    if runtime == Runtime::CircuitPython || mentions_circuitpython(hint) {
        PythonRuntime::CircuitPython
    } else {
        PythonRuntime::MicroPython
    }
}

const CIRCUITPYTHON: &[u8] = b"circuitpython";

/// Whether a file name or banner names CircuitPython. Case-insensitive because
/// a path may be lowercased while the banner is `Adafruit CircuitPython`.
fn mentions_circuitpython(hint: &[u8]) -> bool {
    hint.windows(CIRCUITPYTHON.len())
        .any(|window| window.eq_ignore_ascii_case(CIRCUITPYTHON))
}

/// The Python a multi-file project pastes. It writes every staged file through
/// `open(..., 'wb')`, creating the directories the path needs, then runs the
/// entry file in the REPL's own globals so `__name__` stays `__main__`.
///
/// The shape is shared with the MicroPython path in `runner.rs`; the difference
/// is only which name is passed as `entry`. Neither runtime needs a `boot.py`:
/// CircuitPython runs the entry itself, and wiring files through `boot.py`
/// would execute them before the paste had a chance to finish.
///
/// An empty `scripts` list returns an empty string because there is nothing to
/// stage; the caller pastes a single file directly, as `runner.rs` does.
pub fn stage_program(entry: &str, scripts: &[(&str, &[u8])]) -> String {
    if scripts.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(concat!(
        "def _litz_mkdir(path):\n",
        "    import os\n",
        "    cur = ''\n",
        "    for part in path.split('/')[:-1]:\n",
        "        cur = part if not cur else cur + '/' + part\n",
        "        try:\n",
        "            os.mkdir(cur)\n",
        "        except OSError:\n",
        "            pass\n",
        "def _litz_stage(name, data):\n",
        "    _litz_mkdir(name)\n",
        "    f = open(name, 'wb')\n",
        "    f.write(data)\n",
        "    f.close()\n",
    ));
    for (name, bytes) in scripts {
        out.push_str("_litz_stage(");
        out.push_str(&py_string(name));
        out.push_str(", (");
        out.push_str(&py_bytes(bytes));
        out.push_str("))\n");
    }
    out.push_str("exec(open(");
    out.push_str(&py_string(entry));
    out.push_str(").read())\n");
    out
}

/// A Python string literal for ASCII paths. The stage validator rejects
/// anything that is not a plain relative path, so unusual characters become
/// `?` rather than an escape a build's parser might not share.
fn py_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('\'');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ if ch.is_ascii_graphic() || ch == ' ' => out.push(ch),
            _ => out.push('?'),
        }
    }
    out.push('\'');
    out
}

/// A Python bytes literal, split into adjacent literals so one long file does
/// not produce one endless line. Python concatenates adjacent literals, so the
/// newline between chunks is whitespace, not a change in value.
fn py_bytes(data: &[u8]) -> String {
    if data.is_empty() {
        return "b''".into();
    }
    let mut out = String::with_capacity(data.len() * 4);
    for (index, chunk) in data.chunks(64).enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str("b'");
        for &byte in chunk {
            match byte {
                b'\\' => out.push_str("\\\\"),
                b'\'' => out.push_str("\\'"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                0x20..=0x7e => out.push(byte as char),
                _ => out.push_str(&format!("\\x{byte:02x}")),
            }
        }
        out.push('\'');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_prefers_circuitpython_and_defaults_to_micropython() {
        // An explicit runtime always wins.
        assert_eq!(
            detect(Runtime::CircuitPython, "ESP32_GENERIC_C3.bin"),
            PythonRuntime::CircuitPython
        );
        assert_eq!(
            detect(Runtime::Micropython, "firmware.bin"),
            PythonRuntime::MicroPython
        );
        assert_eq!(
            detect(Runtime::Native, "micropython.bin"),
            PythonRuntime::MicroPython
        );

        // A firmware name or banner that says CircuitPython identifies the
        // image even when the project left the runtime at its default.
        assert_eq!(
            detect(Runtime::Native, "adafruit-circuitpython-esp32s3.bin"),
            PythonRuntime::CircuitPython
        );
        assert_eq!(
            detect_bytes(
                Runtime::Micropython,
                b"Adafruit CircuitPython 10.3.2 on 2026-01-01; ESP32-S3\r\n"
            ),
            PythonRuntime::CircuitPython
        );
    }

    #[test]
    fn circuitpython_runs_code_py_with_main_py_as_the_fallback() {
        assert_eq!(PythonRuntime::MicroPython.entry_name(), "main.py");
        assert_eq!(PythonRuntime::CircuitPython.entry_name(), "code.py");
        // The free functions are the form a caller with a plain runtime uses.
        assert_eq!(entry_name(PythonRuntime::CircuitPython), "code.py");
        assert_eq!(PythonRuntime::MicroPython.entry_candidates(), ["main.py"]);
        assert_eq!(
            PythonRuntime::CircuitPython.entry_candidates(),
            ["code.py", "main.py"]
        );
    }

    #[test]
    fn only_circuitpython_needs_an_interrupt_before_the_paste() {
        assert!(PythonRuntime::MicroPython.interrupt_sequence().is_empty());
        assert_eq!(PythonRuntime::CircuitPython.interrupt_sequence(), &[0x03u8]);
        assert!(interrupt_sequence(PythonRuntime::MicroPython).is_empty());
        assert_eq!(interrupt_sequence(PythonRuntime::CircuitPython), &[0x03u8]);
        // Raw-REPL framing is shared; the call site still names the runtime so
        // one staging sequence covers both.
        for runtime in [PythonRuntime::MicroPython, PythonRuntime::CircuitPython] {
            assert_eq!(runtime.raw_paste_begin(), &[0x01u8]);
            assert_eq!(runtime.raw_paste_end(), &[0x04u8]);
        }
    }

    #[test]
    fn a_multi_file_paste_writes_every_module_then_runs_the_entry() {
        let program = stage_program(
            "code.py",
            &[
                ("code.py", b"import util\nprint(util.N)\n"),
                ("lib/util.py", b"N = 42\n"),
            ],
        );
        assert!(program.contains("_litz_stage('code.py', (b'import util\\nprint(util.N)\\n'))"));
        assert!(program.contains("_litz_stage('lib/util.py', (b'N = 42\\n'))"));
        assert!(program.ends_with("exec(open('code.py').read())\n"));
        assert!(program.contains("os.mkdir(cur)"), "directories are created");
        // CircuitPython must not need a boot.py hook to see the files.
        assert!(!program.contains("boot.py"));
    }

    #[test]
    fn an_empty_paste_has_nothing_to_run() {
        assert!(stage_program("code.py", &[]).is_empty());
    }

    #[test]
    fn bytes_are_escaped_into_a_python_literal() {
        assert_eq!(py_bytes(b"a\nb'c\\d\x01"), "b'a\\nb\\'c\\\\d\\x01'");
        assert_eq!(py_bytes(b""), "b''");
        // A file longer than one 64-byte chunk becomes adjacent literals, not
        // one line Python would refuse to parse at the REPL.
        let long = [b'a'; 65];
        let literal = py_bytes(&long);
        assert_eq!(literal.matches("b'").count(), 2);
    }

    #[test]
    fn a_staged_name_is_escaped_into_the_bootstrap() {
        assert_eq!(py_string("lib/util.py"), "'lib/util.py'");
        assert_eq!(py_string("o'file.py"), "'o\\'file.py'");
    }
}

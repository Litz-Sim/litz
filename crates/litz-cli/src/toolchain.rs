//! `litz toolchain`: which firmware toolchains this machine has, and
//! `litz editor --vscode`: the editor configuration that points a debugger at
//! the runner's GDB stub.
//!
//! Detection is read-only. It looks for executables on `PATH` (and under
//! `$IDF_PATH`) and runs only version-style commands, each with a short
//! timeout. It makes no network calls and installs nothing.
//!
//! The editor configuration is ordinary VS Code JSON: a `cppdbg` launch
//! configuration that connects to `127.0.0.1:<port>`, and a background task
//! that runs `litz run . --gdb <port>` so the firmware boots, the GDB stub
//! starts, and the run's serial output stays in the task's terminal.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Port the generated VS Code task serves the GDB stub on.
pub const DEFAULT_GDB_PORT: u16 = 3333;

/// How long one version probe may take. Long enough for a cold interpreter,
/// short enough that a broken wrapper cannot stall `litz toolchain`.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// One row of the detection table.
#[derive(Clone, Debug, Serialize)]
pub struct Tool {
    pub name: &'static str,
    pub found: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What detection may consult: `PATH` entries and `$IDF_PATH`.
///
/// Kept explicit so a test can hand over a fake `PATH` without touching the
/// process environment.
#[derive(Clone, Debug)]
pub struct Environment {
    path: Vec<PathBuf>,
    idf_path: Option<PathBuf>,
}

impl Environment {
    /// Read `PATH` and `IDF_PATH` from this process.
    pub fn from_process() -> Self {
        let path = std::env::var_os("PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .filter(|entry| !entry.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let idf_path = std::env::var_os("IDF_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self { path, idf_path }
    }

    /// The debugger the generated launch configuration should name, when one
    /// of the common ESP32 GDB builds is installed.
    pub fn gdb(&self) -> Option<PathBuf> {
        [
            "riscv32-esp-elf-gdb",
            "xtensa-esp32s3-elf-gdb",
            "xtensa-esp32-elf-gdb",
            "gdb",
        ]
        .iter()
        .find_map(|name| self.find_on_path(name))
    }

    fn find_on_path(&self, name: &str) -> Option<PathBuf> {
        self.path.iter().find_map(|directory| {
            executable_candidates(directory, name)
                .into_iter()
                .find(|candidate| is_executable(candidate))
        })
    }

    fn probe_version(&self, program: &Path, args: &[&str]) -> Option<String> {
        let capture = self.capture(program, args)?;
        // A failed probe usually printed usage or a traceback, not a version.
        if !capture.success {
            return None;
        }
        let text = if capture.stdout.trim().is_empty() {
            &capture.stderr
        } else {
            &capture.stdout
        };
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        Some(shorten(line))
    }

    /// Run a short command and collect its output, killing it at
    /// [`PROBE_TIMEOUT`] so a wrapper that waits on input cannot hang the CLI.
    fn capture(&self, program: &Path, args: &[&str]) -> Option<Capture> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.path.is_empty() {
            let joined = std::env::join_paths(self.path.iter()).ok()?;
            command.env("PATH", joined);
        }
        let mut child = command.spawn().ok()?;
        let deadline = Instant::now() + PROBE_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => {}
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().ok()?;
        Some(Capture {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            success: output.status.success(),
        })
    }

    /// The installed version of Arduino's ESP32 core, read through
    /// `arduino-cli core list`.
    fn esp32_core(&self, arduino_cli: &Path) -> Option<String> {
        let capture = self.capture(arduino_cli, &["core", "list"])?;
        if !capture.success {
            return None;
        }
        for line in capture.stdout.lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("esp32:esp32") {
                return fields.next().map(str::to_string);
            }
        }
        None
    }

    /// Whether the wasm target is installed. `rustup` knows the installed
    /// targets directly; without it, the sysroot is the evidence.
    fn wasm_target(&self, rustc: Option<&Path>) -> Option<&'static str> {
        if let Some(rustup) = self.find_on_path("rustup")
            && let Some(capture) = self.capture(&rustup, &["target", "list", "--installed"])
            && capture.success
            && capture
                .stdout
                .lines()
                .any(|line| line.trim() == "wasm32-unknown-unknown")
        {
            return Some("installed via rustup");
        }
        let capture = self.capture(rustc?, &["--print", "sysroot"])?;
        let sysroot = PathBuf::from(capture.stdout.trim());
        let target = sysroot
            .join("lib")
            .join("rustlib")
            .join("wasm32-unknown-unknown");
        target.is_dir().then_some("found in the rustc sysroot")
    }
}

struct Capture {
    stdout: String,
    stderr: String,
    success: bool,
}

/// The fixed detection table, in the order the report prints it.
pub fn detect(environment: &Environment) -> Vec<Tool> {
    let mut tools = Vec::new();

    // Arduino first: the ESP32 core is only visible through the CLI.
    let arduino_cli = environment.find_on_path("arduino-cli");
    let arduino_version = arduino_cli
        .as_deref()
        .and_then(|program| environment.probe_version(program, &["version"]));
    tools.push(match &arduino_cli {
        Some(path) => found("arduino-cli", Some(path.clone()), arduino_version, None),
        None => missing("arduino-cli"),
    });
    let core = arduino_cli.as_deref().and_then(|program| {
        environment
            .esp32_core(program)
            .map(|version| (program.to_path_buf(), version))
    });
    tools.push(match core {
        Some((cli, version)) => found(
            "arduino core esp32",
            Some(cli),
            Some(version),
            Some("via arduino-cli core list"),
        ),
        None => missing("arduino core esp32"),
    });

    // ESP-IDF: the script may come from PATH or from $IDF_PATH/tools.
    let idf_script = environment.find_on_path("idf.py").or_else(|| {
        environment
            .idf_path
            .as_ref()
            .map(|root| root.join("tools").join("idf.py"))
            .filter(|path| is_executable(path))
    });
    tools.push(match &idf_script {
        Some(script) => found(
            "idf.py",
            Some(script.clone()),
            environment.probe_version(script, &["--version"]),
            None,
        ),
        None => match &environment.idf_path {
            Some(root) if root.is_dir() => found(
                "idf.py",
                Some(root.clone()),
                None,
                Some("$IDF_PATH is set but tools/idf.py was not found"),
            ),
            _ => missing("idf.py"),
        },
    });

    for name in [
        "riscv32-esp-elf-gcc",
        "xtensa-esp32-elf-gcc",
        "xtensa-esp32s3-elf-gcc",
        "xtensa-esp32c3-elf-gcc",
        "arm-none-eabi-gcc",
        "avr-gcc",
    ] {
        tools.push(compiler(environment, name));
    }

    tools.push(match environment.find_on_path("avrdude") {
        Some(path) => found(
            "avrdude",
            Some(path.clone()),
            environment.probe_version(&path, &["--version"]),
            None,
        ),
        None => missing("avrdude"),
    });

    // The host interpreter and the tool that reaches a board are separate
    // capabilities: one can be installed without the other.
    for (name, note) in [
        ("micropython", None),
        // CircuitPython's host CLI is separate from MicroPython's; firmware
        // images for the simulator come from tools/fetch-circuitpython.sh.
        (
            "circuitpython",
            Some("images come from tools/fetch-circuitpython.sh"),
        ),
        ("mpremote", Some("for boards over USB serial")),
    ] {
        tools.push(match environment.find_on_path(name) {
            Some(path) => found(
                name,
                Some(path.clone()),
                environment.probe_version(&path, &["--version"]),
                note,
            ),
            None => missing(name),
        });
    }

    let rustc = environment.find_on_path("rustc");
    let rustc_version = rustc
        .as_deref()
        .and_then(|program| environment.probe_version(program, &["--version"]));
    tools.push(match &rustc {
        Some(path) => found("rustc", Some(path.clone()), rustc_version, None),
        None => missing("rustc"),
    });
    tools.push(match environment.wasm_target(rustc.as_deref()) {
        Some(note) => found("rust target wasm32-unknown-unknown", None, None, Some(note)),
        None => missing("rust target wasm32-unknown-unknown"),
    });

    tools
}

/// The report as one line per tool, with columns aligned.
pub fn table(tools: &[Tool]) -> String {
    let width = tools
        .iter()
        .map(|tool| tool.name.len())
        .max()
        .unwrap_or(0)
        .max(20);
    let mut output = String::new();
    for tool in tools {
        if tool.found {
            output.push_str(&format!("{:<width$}  ", tool.name));
            match &tool.path {
                Some(path) => output.push_str(path),
                None => output.push_str("found"),
            }
            if let Some(version) = &tool.version {
                output.push_str(&format!("  ({version})"));
            }
            if let Some(note) = &tool.note {
                output.push_str(&format!("  [{note}]"));
            }
        } else {
            output.push_str(&format!("{:<width$}  not found", tool.name));
        }
        output.push('\n');
    }
    output
}

/// The report as a machine-readable object: `{"tools": [...]}`.
pub fn to_json(tools: &[Tool]) -> String {
    let document = json!({ "tools": tools });
    let mut text =
        serde_json::to_string_pretty(&document).expect("the detection table always serialises");
    text.push('\n');
    text
}

fn compiler(environment: &Environment, name: &'static str) -> Tool {
    match environment.find_on_path(name) {
        Some(path) => {
            let version = environment.probe_version(&path, &["--version"]);
            found(name, Some(path), version, None)
        }
        None => missing(name),
    }
}

fn found(
    name: &'static str,
    path: Option<PathBuf>,
    version: Option<String>,
    note: Option<&str>,
) -> Tool {
    Tool {
        name,
        found: true,
        path: path.map(|path| path.display().to_string()),
        version,
        note: note.map(str::to_string),
    }
}

fn missing(name: &'static str) -> Tool {
    Tool {
        name,
        found: false,
        path: None,
        version: None,
        note: None,
    }
}

#[cfg(windows)]
fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    vec![directory.join(format!("{name}.exe")), directory.join(name)]
}

#[cfg(not(windows))]
fn executable_candidates(directory: &Path, name: &str) -> Vec<PathBuf> {
    vec![directory.join(name)]
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn shorten(line: &str) -> String {
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 120 {
        return collapsed;
    }
    let mut short: String = collapsed.chars().take(117).collect();
    short.push_str("...");
    short
}

/// `litz editor --vscode`: write or merge the editor files for `directory`.
///
/// Without `force`, any existing `launch.json` or `tasks.json` is refused
/// rather than replaced. With `force`, the Litz entries are merged into the
/// existing documents, so unrelated configurations and tasks survive.
pub fn write_vscode(directory: &Path, force: bool, port: u16) -> Result<Vec<PathBuf>, String> {
    let environment = Environment::from_process();
    write_vscode_with(directory, force, port, environment.gdb().as_deref())
}

fn write_vscode_with(
    directory: &Path,
    force: bool,
    port: u16,
    gdb: Option<&Path>,
) -> Result<Vec<PathBuf>, String> {
    if !directory.is_dir() {
        return Err(format!("{}: not a directory", directory.display()));
    }
    let vscode = directory.join(".vscode");
    let launch_path = vscode.join("launch.json");
    let tasks_path = vscode.join("tasks.json");

    if !force {
        for path in [&launch_path, &tasks_path] {
            if path.exists() {
                return Err(format!(
                    "{} already exists; pass --force to merge the Litz configuration into it",
                    path.display()
                ));
            }
        }
    }

    let launch = merge_document(
        read_existing(&launch_path, force)?,
        "0.2.0",
        "configurations",
        "name",
        LAUNCH_NAME,
        launch_configuration(&program_for(directory), gdb, port),
    );
    let tasks = merge_document(
        read_existing(&tasks_path, force)?,
        "2.0.0",
        "tasks",
        "label",
        TASK_LABEL,
        debug_task(port),
    );

    std::fs::create_dir_all(&vscode).map_err(|error| format!("{}: {error}", vscode.display()))?;
    write_json(&launch_path, &launch)?;
    write_json(&tasks_path, &tasks)?;
    Ok(vec![launch_path, tasks_path])
}

const LAUNCH_NAME: &str = "Litz: debug firmware";
const TASK_LABEL: &str = "litz: run firmware with GDB";

#[derive(Deserialize, Default)]
struct Manifest {
    sim: Option<Sim>,
}

#[derive(Deserialize, Default)]
struct Sim {
    elf: Option<String>,
    firmware: Option<String>,
}

/// The file GDB should load symbols from. `[sim] elf` is what Litz's own
/// debugger path uses; a bare firmware image still gives GDB an address space.
fn program_for(directory: &Path) -> String {
    let file = std::fs::read_to_string(directory.join("litz.toml"))
        .ok()
        .and_then(|text| toml::from_str::<Manifest>(&text).ok())
        .and_then(|manifest| manifest.sim)
        .and_then(|sim| sim.elf.or(sim.firmware));
    match file {
        Some(file) if Path::new(&file).is_absolute() => file,
        Some(file) => format!("${{workspaceFolder}}/{}", file.replace('\\', "/")),
        None => "${workspaceFolder}".to_string(),
    }
}

fn launch_configuration(program: &str, gdb: Option<&Path>, port: u16) -> Value {
    let mut configuration = json!({
        "name": LAUNCH_NAME,
        "type": "cppdbg",
        "request": "launch",
        "program": program,
        "cwd": "${workspaceFolder}",
        "stopAtEntry": false,
        "externalConsole": false,
        "MIMode": "gdb",
        "miDebuggerServerAddress": format!("127.0.0.1:{port}"),
        "preLaunchTask": TASK_LABEL,
        "setupCommands": [{
            "description": "Enable gdb pretty-printing",
            "text": "-enable-pretty-printing",
            "ignoreFailures": true
        }]
    });
    if let Some(gdb) = gdb {
        configuration["miDebuggerPath"] = json!(gdb.display().to_string());
    }
    configuration
}

fn debug_task(port: u16) -> Value {
    json!({
        "label": TASK_LABEL,
        "type": "shell",
        "command": "litz",
        "args": ["run", ".", "--gdb", port.to_string()],
        "options": { "cwd": "${workspaceFolder}" },
        "isBackground": true,
        "problemMatcher": {
            "owner": "litz",
            "pattern": {
                "regexp": "^litz-diagnostic: ([^:]+): (.*)$",
                "file": 1,
                "message": 2
            },
            "background": {
                "activeOnStart": true,
                "beginsPattern": "litz: gdb serves board",
                "endsPattern": "litz: gdb waiting on"
            }
        },
        "presentation": { "reveal": "always", "panel": "dedicated", "clear": true }
    })
}

/// Merge one named entry into an existing document, keeping everything else.
fn merge_document(
    existing: Option<Value>,
    version: &str,
    array_key: &str,
    name_key: &str,
    name: &str,
    item: Value,
) -> Value {
    let mut root = match existing {
        Some(value) if value.is_object() => value,
        _ => {
            let mut object = Map::new();
            object.insert("version".to_string(), Value::String(version.to_string()));
            object.insert(array_key.to_string(), Value::Array(Vec::new()));
            Value::Object(object)
        }
    };
    let object = root
        .as_object_mut()
        .expect("the document was just checked to be an object");
    object
        .entry("version".to_string())
        .or_insert_with(|| json!(version));
    let array = object
        .entry(array_key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !array.is_array() {
        *array = Value::Array(Vec::new());
    }
    let array = array
        .as_array_mut()
        .expect("the entry was just made an array");
    match array
        .iter_mut()
        .find(|entry| entry.get(name_key).and_then(Value::as_str) == Some(name))
    {
        Some(slot) => *slot = item,
        None => array.push(item),
    }
    root
}

fn read_existing(path: &Path, force: bool) -> Result<Option<Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    match serde_json::from_str(&text) {
        Ok(value) => Ok(Some(value)),
        // With --force an unparseable file is replaced rather than merged.
        Err(_) if force => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    text.push('\n');
    std::fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("litz-toolchain-{}-{n}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn fake_tool(directory: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
        path
    }

    fn tool<'a>(tools: &'a [Tool], name: &str) -> &'a Tool {
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("{name:?} is in the detection table"))
    }

    #[cfg(unix)]
    #[test]
    fn the_detection_table_reads_versions_from_a_fake_path() {
        let bin = scratch("detect-bin");
        fake_tool(
            &bin,
            "arduino-cli",
            r#"if [ "$1" = "version" ]; then
  echo "arduino-cli  Version: 1.0.4 Commit: deadbeef"
elif [ "$1" = "core" ]; then
  printf 'ID          Installed Latest Name\nesp32:esp32 3.0.7     3.0.7  Arduino ESP32 Boards\n'
fi"#,
        );
        fake_tool(
            &bin,
            "riscv32-esp-elf-gcc",
            r#"echo "riscv32-esp-elf-gcc (crosstool-NG) 13.2.0""#,
        );
        fake_tool(
            &bin,
            "xtensa-esp32s3-elf-gcc",
            r#"echo "xtensa-esp32s3-elf-gcc (crosstool-NG) 12.2.0""#,
        );
        fake_tool(&bin, "rustc", r#"echo "rustc 1.98.0-nightly""#);
        fake_tool(&bin, "circuitpython", r#"echo "CircuitPython 10.3.2""#);
        fake_tool(
            &bin,
            "rustup",
            r#"if [ "$1" = "target" ]; then
  printf 'wasm32-unknown-unknown\nx86_64-apple-darwin\n'
fi"#,
        );

        let idf = scratch("detect-idf");
        let idf_tools = idf.join("tools");
        std::fs::create_dir_all(&idf_tools).unwrap();
        fake_tool(&idf_tools, "idf.py", r#"echo "ESP-IDF v5.2.1""#);

        let environment = Environment {
            path: vec![bin.clone()],
            idf_path: Some(idf.clone()),
        };
        let tools = detect(&environment);

        let arduino = tool(&tools, "arduino-cli");
        assert!(arduino.found);
        assert!(arduino.version.as_deref().unwrap().contains("1.0.4"));

        let core = tool(&tools, "arduino core esp32");
        assert!(core.found);
        assert_eq!(core.version.as_deref(), Some("3.0.7"));

        let idf_tool = tool(&tools, "idf.py");
        assert!(idf_tool.found);
        assert!(idf_tool.path.as_deref().unwrap().ends_with("tools/idf.py"));
        assert!(
            idf_tool
                .version
                .as_deref()
                .unwrap()
                .contains("ESP-IDF v5.2.1")
        );

        let riscv = tool(&tools, "riscv32-esp-elf-gcc");
        assert!(riscv.found);
        assert!(riscv.version.as_deref().unwrap().contains("13.2.0"));

        assert!(tool(&tools, "xtensa-esp32s3-elf-gcc").found);
        assert!(!tool(&tools, "xtensa-esp32-elf-gcc").found);
        assert!(!tool(&tools, "xtensa-esp32c3-elf-gcc").found);
        assert!(!tool(&tools, "arm-none-eabi-gcc").found);
        assert!(!tool(&tools, "avr-gcc").found);
        assert!(!tool(&tools, "avrdude").found);
        assert!(!tool(&tools, "micropython").found);
        assert!(!tool(&tools, "mpremote").found);

        let circuitpython = tool(&tools, "circuitpython");
        assert!(circuitpython.found);
        assert!(circuitpython.version.as_deref().unwrap().contains("10.3.2"));

        let rustc = tool(&tools, "rustc");
        assert!(rustc.found);
        assert!(rustc.version.as_deref().unwrap().contains("1.98.0"));

        let target = tool(&tools, "rust target wasm32-unknown-unknown");
        assert!(target.found);
        assert_eq!(target.note.as_deref(), Some("installed via rustup"));

        let _ = std::fs::remove_dir_all(&bin);
        let _ = std::fs::remove_dir_all(&idf);
    }

    #[test]
    fn an_empty_path_reports_every_tool_as_missing() {
        let bin = scratch("empty-bin");
        let environment = Environment {
            path: vec![bin.clone()],
            idf_path: None,
        };
        let tools = detect(&environment);
        assert!(!tools.is_empty());
        for tool in &tools {
            assert!(!tool.found, "{} should be missing", tool.name);
            assert!(tool.path.is_none(), "{} should have no path", tool.name);
        }
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn json_is_one_object_with_a_tools_array() {
        let bin = scratch("json-bin");
        let environment = Environment {
            path: vec![bin.clone()],
            idf_path: None,
        };
        let text = to_json(&detect(&environment));
        let document: Value = serde_json::from_str(&text).unwrap();
        let tools = document["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "rustc"));
        assert!(tools.iter().all(|tool| tool["found"].is_boolean()));
        let _ = std::fs::remove_dir_all(&bin);
    }

    #[test]
    fn editor_writes_valid_json_and_then_refuses_without_force() {
        let dir = scratch("editor-fresh");
        std::fs::write(
            dir.join("litz.toml"),
            "[sim]\nfirmware = \"firmware/app.bin\"\n",
        )
        .unwrap();

        let written =
            write_vscode_with(&dir, false, DEFAULT_GDB_PORT, Some(Path::new("/bin/gdb"))).unwrap();
        assert_eq!(written.len(), 2);

        let launch: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join(".vscode/launch.json")).unwrap(),
        )
        .unwrap();
        let configurations = launch["configurations"].as_array().unwrap();
        assert_eq!(configurations.len(), 1);
        let configuration = &configurations[0];
        assert_eq!(configuration["name"].as_str(), Some(LAUNCH_NAME));
        assert_eq!(configuration["type"].as_str(), Some("cppdbg"));
        assert_eq!(
            configuration["miDebuggerServerAddress"].as_str(),
            Some("127.0.0.1:3333")
        );
        assert_eq!(configuration["miDebuggerPath"].as_str(), Some("/bin/gdb"));
        assert!(
            configuration["program"]
                .as_str()
                .unwrap()
                .ends_with("firmware/app.bin")
        );

        let tasks: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(".vscode/tasks.json")).unwrap())
                .unwrap();
        let task = &tasks["tasks"][0];
        assert_eq!(task["command"].as_str(), Some("litz"));
        let args: Vec<&str> = task["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let gdb = args
            .iter()
            .position(|arg| *arg == "--gdb")
            .expect("the task serves the GDB stub");
        assert_eq!(args.get(gdb + 1), Some(&"3333"));

        let error = write_vscode_with(&dir, false, DEFAULT_GDB_PORT, None).unwrap_err();
        assert!(error.contains("--force"), "{error}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merging_keeps_unrelated_configurations_and_tasks() {
        let dir = scratch("editor-merge");
        let vscode = dir.join(".vscode");
        std::fs::create_dir_all(&vscode).unwrap();
        std::fs::write(
            vscode.join("launch.json"),
            r#"{"version":"0.2.0","configurations":[{"name":"Unrelated","type":"node"}]}"#,
        )
        .unwrap();
        std::fs::write(
            vscode.join("tasks.json"),
            r#"{"version":"2.0.0","tasks":[{"label":"Unrelated","command":"true"}]}"#,
        )
        .unwrap();

        write_vscode_with(&dir, true, 4444, None).unwrap();

        let launch: Value =
            serde_json::from_str(&std::fs::read_to_string(vscode.join("launch.json")).unwrap())
                .unwrap();
        let names: Vec<&str> = launch["configurations"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect();
        assert!(names.contains(&"Unrelated"), "{names:?}");
        assert!(names.contains(&LAUNCH_NAME), "{names:?}");

        let tasks: Value =
            serde_json::from_str(&std::fs::read_to_string(vscode.join("tasks.json")).unwrap())
                .unwrap();
        let labels: Vec<&str> = tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["label"].as_str())
            .collect();
        assert!(labels.contains(&"Unrelated"), "{labels:?}");
        assert!(labels.contains(&TASK_LABEL), "{labels:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_is_refused_before_anything_is_written() {
        let dir = scratch("editor-existing");
        let vscode = dir.join(".vscode");
        std::fs::create_dir_all(&vscode).unwrap();
        std::fs::write(vscode.join("launch.json"), "{}").unwrap();

        let error = write_vscode_with(&dir, false, DEFAULT_GDB_PORT, None).unwrap_err();
        assert!(error.contains("--force"), "{error}");
        assert!(
            !vscode.join("tasks.json").exists(),
            "nothing is written when the run is refused"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! `litz editor --vscode` and `litz toolchain`: the two host-facing commands
//! that need no firmware and no downloaded assets.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("litz-cli-editor-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn litz_in(directory: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(args)
        .current_dir(directory)
        .output()
        .expect("run the litz binary")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn editor_writes_a_debug_configuration_and_refuses_the_second_run() {
    let dir = TempDir::new("fresh");
    std::fs::write(
        dir.path().join("litz.toml"),
        "[sim]\nfirmware = \"firmware/app.bin\"\n",
    )
    .unwrap();

    let first = litz_in(dir.path(), &["editor", "--vscode", "."]);
    assert!(first.status.success(), "{}", text(&first));

    let launch: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".vscode/launch.json")).unwrap(),
    )
    .unwrap();
    let configurations = launch["configurations"].as_array().unwrap();
    assert_eq!(configurations.len(), 1);
    assert_eq!(configurations[0]["type"].as_str(), Some("cppdbg"));
    assert_eq!(
        configurations[0]["miDebuggerServerAddress"].as_str(),
        Some("127.0.0.1:3333")
    );
    assert!(
        configurations[0]["program"]
            .as_str()
            .unwrap()
            .ends_with("firmware/app.bin")
    );

    let tasks: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".vscode/tasks.json")).unwrap(),
    )
    .unwrap();
    let task = &tasks["tasks"][0];
    assert_eq!(task["command"].as_str(), Some("litz"));
    let args: Vec<&str> = task["args"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let index = args
        .iter()
        .position(|arg| *arg == "--gdb")
        .expect("the task serves the GDB stub");
    assert_eq!(args.get(index + 1), Some(&"3333"));

    let second = litz_in(dir.path(), &["editor", "--vscode", "."]);
    assert_eq!(second.status.code(), Some(1), "{}", text(&second));
    assert!(text(&second).contains("--force"), "{}", text(&second));
}

#[test]
fn toolchain_json_is_a_machine_readable_object_and_exits_zero() {
    let dir = TempDir::new("toolchain");
    for command in ["toolchain", "doctor"] {
        let output = litz_in(dir.path(), &[command, "--json"]);
        assert!(output.status.success(), "{}", text(&output));
        let document: Value = serde_json::from_slice(&output.stdout).expect("valid JSON");
        assert!(document["tools"].is_array(), "{document}");
        for tool in document["tools"].as_array().unwrap() {
            assert!(tool["name"].is_string(), "{tool}");
            assert!(tool["found"].is_boolean(), "{tool}");
        }
    }
}

#[test]
fn toolchain_help_says_it_does_not_change_the_system() {
    let dir = TempDir::new("help");
    let output = litz_in(dir.path(), &["toolchain", "--help"]);
    assert!(output.status.success(), "{}", text(&output));
    let text = text(&output);
    assert!(text.contains("does not change your system"), "{text}");
    assert!(text.contains("no network"), "{text}");
}

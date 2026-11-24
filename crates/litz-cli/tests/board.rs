//! `litz board check`: the loop a chip author runs before installing a bundle.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture() -> PathBuf {
    repo_root().join("crates/litz-mcu-wasm/tests/fixtures/board-v2")
}

fn litz(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(args)
        .current_dir(repo_root())
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
fn a_bundle_instantiates_and_reports_what_it_is() {
    let path = fixture();
    let output = litz(&["board", "check", path.to_str().unwrap()]);
    let text = text(&output);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("board-v2-fixture"), "{text}");
    assert!(text.contains("litz-mcu-2"), "{text}");
    assert!(text.contains("header pins 12"), "{text}");
}

#[test]
fn a_module_built_for_the_other_abi_is_caught_here() {
    let scratch = std::env::temp_dir().join(format!("litz-board-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    for entry in std::fs::read_dir(fixture()).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), scratch.join(entry.file_name())).unwrap();
    }
    let manifest = scratch.join("board.toml");
    let manifest_text = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, manifest_text.replace("litz-mcu-2", "litz-mcu-1")).unwrap();

    let output = litz(&["board", "check", scratch.to_str().unwrap()]);
    let text = text(&output);
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("board-v2.wasm"), "{text}");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// The whole authoring path a project experiences: a board bundle in
/// `boards/`, a diagram that wires it to an access point and a sensor, and a
/// firmware image whose program calls the 0.2 host interface.
/// `--flash-export` writes the MCU's flash as the run left it: the whole
/// array, with the firmware where it was loaded.
#[test]
fn flash_export_writes_the_array_the_firmware_ran_from() {
    let scratch = repo_root().join(format!("build/flash-export-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("firmware")).unwrap();
    // A Pico image that halts at once: stage 2 returns, the vector table
    // points at the application, and the application is one `bkpt`.
    let mut app = vec![0u8; 0x10c];
    app[0..2].copy_from_slice(&[0x70, 0x47]); // bx lr
    app[0x100..0x104].copy_from_slice(&0x2004_2000u32.to_le_bytes());
    app[0x104..0x108].copy_from_slice(&0x1000_0109u32.to_le_bytes());
    app[0x108..0x10a].copy_from_slice(&[0x00, 0xbe]); // bkpt 0
    std::fs::write(scratch.join("firmware/halt.bin"), &app).unwrap();
    std::fs::write(
        scratch.join("diagram.json"),
        r#"{"version":1,"parts":[
          {"id":"pico","type":"board-raspberry-pi-pico","attrs":{}},
          {"id":"r1","type":"litz-resistor","attrs":{"value":"330"}},
          {"id":"led1","type":"litz-led","attrs":{"color":"green"}}],
          "connections":[
          ["pico:GP15","r1:1","green",[]],
          ["r1:2","led1:A","green",[]],
          ["led1:C","pico:GND.1","black",[]]]}"#,
    )
    .unwrap();
    std::fs::write(
        scratch.join("litz.toml"),
        r#"[project]
diagram = "diagram.json"

[sim]
firmware = "firmware/halt.bin"
timeout = "2ms"
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(["run", ".", "--flash-export", "flash.bin"])
        .current_dir(&scratch)
        .output()
        .expect("run the litz binary");
    let text = text(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");

    let flash = std::fs::read(scratch.join("flash.bin")).expect("the flash was written");
    assert_eq!(flash.len(), 2 * 1024 * 1024, "the RP2040's whole array");
    assert_eq!(
        &flash[..app.len()],
        &app[..],
        "the firmware is where it was loaded"
    );
    assert!(
        flash[app.len()..].iter().all(|byte| *byte == 0xff),
        "the rest of the array is erased flash"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn a_local_v2_board_boots_with_its_own_firmware() {
    let scratch = std::env::temp_dir().join(format!("litz-board-run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let board = scratch.join("boards/board-v2-fixture");
    std::fs::create_dir_all(&board).unwrap();
    for entry in std::fs::read_dir(fixture()).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == "README.md" {
            continue;
        }
        std::fs::copy(entry.path(), board.join(entry.file_name())).unwrap();
    }

    let mut program = Vec::new();
    for byte in b"hi\n" {
        program.extend([1, *byte]);
    }
    program.extend([4, 0]); // wifi-scan
    program.extend([4, 3, 0]); // analog-read on GPIO0
    program.extend([4, 5, 3, b'l', b'a', b'b']); // wifi-join "lab"
    program.extend([4, 6, 0x00, 0x07, 3, b'p', b'i', b'n']); // tcp echo
    program.push(0); // halt
    std::fs::create_dir_all(scratch.join("firmware")).unwrap();
    std::fs::write(scratch.join("firmware/host.bin"), &program).unwrap();

    std::fs::write(
        scratch.join("diagram.json"),
        r#"{
          "version": 1,
          "parts": [
            {"id": "mcu", "type": "board-v2-fixture", "attrs": {}},
            {"id": "ap", "type": "litz-wifi-router", "attrs": {"ssid": "lab", "echo": "7"}}
          ],
          "connections": []
        }"#,
    )
    .unwrap();
    std::fs::write(
        scratch.join("litz.toml"),
        r#"[project]
diagram = "diagram.json"

[sim]
firmware = "firmware/host.bin"
timeout = "1s"
"#,
    )
    .unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(["run", "."])
        .current_dir(&scratch)
        .output()
        .expect("run the litz binary");
    let text = text(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(text.contains("wifi lab ch6"), "{text}");
    assert!(text.contains("joined lab at 10.0.2.15"), "{text}");
    assert!(text.contains("tcp 7 pin"), "{text}");
    let _ = std::fs::remove_dir_all(&scratch);
}

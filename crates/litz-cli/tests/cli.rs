//! The command line itself: spellings another tool may already use, and the
//! exit codes a CI step branches on.

use std::path::Path;
use std::process::{Command, Output};

fn litz(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("run the litz binary")
}

#[test]
fn version_prints_the_package_version() {
    let output = litz(&["--version"]);
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), format!("litz {}", env!("CARGO_PKG_VERSION")));
}

#[test]
fn lint_is_check_under_another_name() {
    let output = litz(&["lint", "examples/blinky"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("diagram is valid"));
}

#[test]
fn warnings_as_errors_is_strict() {
    // `examples/blinky` produces no warnings, so the alias must still pass;
    // the point is that the spelling exists and is accepted.
    let output = litz(&["check", "examples/blinky", "--warnings-as-errors"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn build_runs_a_recipe_and_reports_missing_config() {
    let dir = std::env::temp_dir().join(format!("litz-cli-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("litz.build.toml"),
        "[build]\nfirmware = \"out.bin\"\n\n[[step]]\nrun = \"printf built > out.bin\"\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(["build", dir.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read(dir.join("out.bin")).unwrap(), b"built");
    let empty = std::env::temp_dir().join(format!("litz-cli-no-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&empty);
    std::fs::create_dir_all(&empty).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(["build", empty.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("litz.build.toml"));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn an_unknown_option_exits_two() {
    let output = litz(&["--nonsense"]);
    assert_eq!(output.status.code(), Some(2));
}

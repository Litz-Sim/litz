//! `litz catalog`: the JSON contract CI reads, and the project-bundle
//! subcommands a user installs parts with.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn litz(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run the litz binary")
}

fn litz_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_litz"))
        .args(args)
        .current_dir(dir)
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

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("litz-cli-catalog-{tag}-{}-{n}", std::process::id()));
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

const GOOD_PART: &str = r#"
[part]
id = "vendor-good"
name = "Good"
version = "0.1.0"
[pins.P]
kind = "passive"
[simulation]
backend = "builtin"
engine = "x"
"#;

const BAD_PART: &str = r#"
[part]
id = "vendor-bad"
name = "Bad"
"#;

fn bundle(dir: &Path, id: &str, module: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("part.toml"),
        format!(
            r#"
[part]
id = "{id}"
name = "Mirror"
version = "1.0.0"
[pins.IN]
kind = "digital-in"
[pins.OUT]
kind = "digital-out"
[simulation]
backend = "wasm"
module = "{module}"
"#
        ),
    )
    .unwrap();
    std::fs::write(dir.join(module), b"\0asm").unwrap();
}

#[test]
fn catalog_json_is_a_large_array() {
    let output = litz(&["catalog", "--json"]);
    assert!(output.status.success(), "{}", text(&output));
    let parsed: Value = serde_json::from_slice(&output.stdout).expect("the output is JSON");
    let entries = parsed.as_array().expect("the catalog is an array");
    assert!(
        entries.len() > 100,
        "the builtin catalog has more than a hundred entries, found {}",
        entries.len()
    );
    for entry in entries {
        assert!(entry["id"].is_string(), "{entry}");
        assert!(matches!(entry["kind"].as_str(), Some("part" | "board")));
        assert!(entry["pins"].is_array(), "{entry}");
    }
}

#[test]
fn catalog_without_json_prints_a_human_table() {
    let output = litz(&["catalog"]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let header = stdout.lines().next().expect("a header line");
    assert!(header.starts_with("id"), "{header}");
    assert!(header.contains("kind") && header.contains("name") && header.contains("pins"));
    assert!(
        stdout.lines().any(|line| line.starts_with("litz-led ")),
        "{stdout}"
    );
    assert!(!stdout.trim_start().starts_with('['), "not JSON: {stdout}");
}

#[test]
fn check_names_the_bad_manifest_and_exits_one() {
    let project = TempDir::new("check");
    std::fs::create_dir_all(project.path().join("parts")).unwrap();
    std::fs::write(project.path().join("parts/good.toml"), GOOD_PART).unwrap();
    std::fs::write(project.path().join("parts/bad.toml"), BAD_PART).unwrap();

    let output = litz(&["catalog", "check", project.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let seen = text(&output);
    assert!(seen.contains("bad.toml"), "{seen}");
    assert!(seen.contains("version"), "{seen}");
    assert!(!seen.contains("good.toml"), "good was accepted: {seen}");

    std::fs::remove_file(project.path().join("parts/bad.toml")).unwrap();
    let output = litz(&["catalog", "check", project.path().to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 manifest"),
        "{}",
        text(&output)
    );
}

#[test]
fn add_copies_a_bundle_and_list_shows_it() {
    let scratch = TempDir::new("add-list");
    let source = scratch.path().join("bundle");
    let project = scratch.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    bundle(&source, "vendor-mirror", "mirror.wasm");

    let output = litz_in(&project, &["catalog", "add", source.to_str().unwrap()]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("vendor-mirror"),
        "{}",
        text(&output)
    );
    assert!(project.join("parts/part.toml").is_file());
    assert!(project.join("parts/mirror.wasm").is_file());

    let output = litz_in(&project, &["catalog", "list", "."]);
    assert!(output.status.success(), "{}", text(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let row = stdout
        .lines()
        .find(|line| line.starts_with("vendor-mirror "))
        .unwrap_or_else(|| panic!("vendor-mirror is listed: {stdout}"));
    assert!(row.contains("parts/"), "{row}");
}

#[test]
fn adding_the_same_id_twice_fails() {
    let scratch = TempDir::new("twice");
    let source = scratch.path().join("bundle");
    let project = scratch.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    bundle(&source, "vendor-mirror", "mirror.wasm");

    let first = litz_in(&project, &["catalog", "add", source.to_str().unwrap()]);
    assert!(first.status.success(), "{}", text(&first));

    let second = litz_in(&project, &["catalog", "add", source.to_str().unwrap()]);
    assert_eq!(second.status.code(), Some(1), "{}", text(&second));
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already installed"),
        "{}",
        text(&second)
    );
}

#[test]
fn remove_refuses_a_builtin_id() {
    let project = TempDir::new("remove-builtin");
    let output = litz_in(project.path(), &["catalog", "remove", "litz-led"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("builtin"),
        "{}",
        text(&output)
    );
}

#[test]
fn an_unknown_catalog_subcommand_exits_two() {
    let output = litz(&["catalog", "frobnicate"]);
    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("unknown catalog subcommand"),
        "{}",
        text(&output)
    );
}

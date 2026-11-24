//! `litz catalog`: the builtin catalog as JSON.
//!
//! The command exists so a front end can render a component picker without
//! linking the runner. It reads the same [`Registry::builtin`] every other
//! command starts from and prints one record per entry. Nothing here
//! simulates, loads a diagram or touches the filesystem.

use litz_catalog::{PartManifest, Registry, Simulation};
use litz_diagram::AttrDef;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

/// The builtin catalog as a JSON array, one object per part or board.
pub fn builtin_json() -> String {
    json_text(&Registry::builtin())
}

/// Serialise a registry. Every entry appears exactly once, ordered by id
/// because the registry is a `BTreeMap`.
pub fn json_text(registry: &Registry) -> String {
    serde_json::to_string_pretty(&value(registry))
        .expect("a catalog entry holds only JSON-serialisable values")
}

/// The catalog as a JSON value: `{id, kind, name, description, pins, attrs}`.
pub fn value(registry: &Registry) -> Value {
    let mut entries = Vec::new();
    for id in registry.ids() {
        let Some(manifest) = registry.get(id) else {
            continue;
        };
        let kind = if manifest.board.is_some() {
            "board"
        } else {
            "part"
        };
        entries.push(json!({
            "id": manifest.id,
            "kind": kind,
            "name": manifest.name,
            "description": manifest.description,
            "pins": manifest.pins.iter()
                .map(|pin| json!({"name": pin.name, "kind": pin.kind}))
                .collect::<Vec<_>>(),
            "attrs": manifest.attrs.iter().map(attr_value).collect::<Vec<_>>(),
        }));
    }
    Value::Array(entries)
}

fn attr_value(attr: &AttrDef) -> Value {
    json!({
        "name": attr.name,
        "type": attr.kind,
        "values": attr.values,
        "default": attr.default,
    })
}

// ------------------------------------------------------------ human listings

/// One row of a human catalog table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub kind: &'static str,
    pub name: String,
    pub pins: usize,
    /// `builtin`, `parts/` or `boards/`.
    pub source: &'static str,
}

/// The registry's entries as rows, ordered by id.
pub fn rows(registry: &Registry) -> Vec<Row> {
    let mut rows: Vec<Row> = registry
        .ids()
        .filter_map(|id| registry.get(id))
        .map(|manifest| Row {
            id: manifest.id.clone(),
            kind: if manifest.board.is_some() {
                "board"
            } else {
                "part"
            },
            name: manifest.name.clone(),
            pins: manifest.pins.len(),
            source: source_label(manifest.source.as_deref()),
        })
        .collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

/// The directory a manifest was loaded from, as the table spells it.
fn source_label(source: Option<&Path>) -> &'static str {
    match source
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
    {
        Some("parts") => "parts/",
        Some("boards") => "boards/",
        Some(_) => "project",
        None => "builtin",
    }
}

/// Render rows as a compact human table. `source` adds the source column used
/// by `catalog list`; `catalog` itself leaves it off.
pub fn table(rows: &[Row], source: bool) -> String {
    let id_width = rows
        .iter()
        .map(|row| row.id.len())
        .max()
        .unwrap_or(0)
        .max("id".len());
    let kind_width = rows
        .iter()
        .map(|row| row.kind.len())
        .max()
        .unwrap_or(0)
        .max("kind".len());
    let name_width = rows
        .iter()
        .map(|row| row.name.len())
        .max()
        .unwrap_or(0)
        .max("name".len());
    let pins_width = rows
        .iter()
        .map(|row| row.pins.to_string().len())
        .max()
        .unwrap_or(0)
        .max("pins".len());

    let mut out = String::new();
    if source {
        let source_width = rows
            .iter()
            .map(|row| row.source.len())
            .max()
            .unwrap_or(0)
            .max("source".len());
        let _ = writeln!(
            out,
            "{:<id_width$}  {:<kind_width$}  {:<name_width$}  {:>pins_width$}  {:<source_width$}",
            "id", "kind", "name", "pins", "source"
        );
        for row in rows {
            let _ = writeln!(
                out,
                "{:<id_width$}  {:<kind_width$}  {:<name_width$}  {:>pins_width$}  {:<source_width$}",
                row.id, row.kind, row.name, row.pins, row.source
            );
        }
    } else {
        let _ = writeln!(
            out,
            "{:<id_width$}  {:<kind_width$}  {:<name_width$}  {:>pins_width$}",
            "id", "kind", "name", "pins"
        );
        for row in rows {
            let _ = writeln!(
                out,
                "{:<id_width$}  {:<kind_width$}  {:<name_width$}  {:>pins_width$}",
                row.id, row.kind, row.name, row.pins
            );
        }
    }
    out
}

/// The builtin catalog as a human table.
pub fn builtin_table() -> String {
    table(&rows(&Registry::builtin()), false)
}

/// The builtin catalog plus `dir/parts` and `dir/boards`, as rows.
pub fn listing(dir: &Path) -> Result<Vec<Row>, String> {
    Ok(rows(&load_project(dir)?))
}

/// Builtin entries first, then the project's manifests, which override an
/// entry with the same id exactly as a run does.
fn load_project(dir: &Path) -> Result<Registry, String> {
    let mut registry = Registry::builtin();
    for sub in ["parts", "boards"] {
        let path = dir.join(sub);
        if path.is_dir() {
            registry.load_dir(&path).map_err(|e| e.to_string())?;
        }
    }
    Ok(registry)
}

// ------------------------------------------------------------------ checking

/// What `catalog check` found: how many manifests it read and one message per
/// manifest that did not parse.
#[derive(Debug, Default)]
pub struct CheckReport {
    pub checked: usize,
    pub errors: Vec<String>,
}

/// Parse every manifest in `dir/parts` and `dir/boards`. A missing directory
/// is not an error; it simply holds no manifests.
pub fn check_dir(dir: &Path) -> CheckReport {
    let mut report = CheckReport::default();
    for sub in ["parts", "boards"] {
        let files = match toml_files(&dir.join(sub)) {
            Ok(files) => files,
            Err(error) => {
                report.errors.push(error);
                continue;
            }
        };
        for file in files {
            report.checked += 1;
            if let Err(error) = parse_file(&file) {
                report.errors.push(error);
            }
        }
    }
    report
}

// ---------------------------------------------------------------- installing

/// A bundle copied into a project's catalog directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    /// The installed manifest, not the directory.
    pub path: PathBuf,
}

/// Install `source` into `project/dir`.
///
/// The whole bundle is read and checked before the first byte is written, so a
/// bad bundle leaves the destination untouched. A failed write rolls back the
/// files this call created.
pub fn add(source: &Path, project: &Path, dir: &str) -> Result<Installed, String> {
    if !source.is_dir() {
        return Err(format!("{}: not a directory", source.display()));
    }
    let manifest_path = bundle_manifest(source)?;
    let manifest = parse_file(&manifest_path)?;
    let id = manifest.id.clone();

    if Registry::builtin().get(&id).is_some() {
        return Err(format!(
            "{id}: the id is already a builtin entry; rename it before installing"
        ));
    }

    // A part is one file in `parts/`; a board is a directory of its own,
    // because its module, schematic and source all belong together and two
    // boards must not both land on `board.toml`.
    let base = project.join(dir);
    let destination = if manifest.board.is_some() {
        base.join(&id)
    } else {
        base.clone()
    };
    for file in toml_files(&base)? {
        let other = parse_file(&file)?;
        if other.id == id {
            return Err(format!(
                "{id}: already installed in {}",
                destination.display()
            ));
        }
    }

    let manifest_name = manifest_path
        .file_name()
        .ok_or_else(|| format!("{}: has no file name", manifest_path.display()))?;
    let target_manifest = destination.join(manifest_name);
    if target_manifest.exists() {
        return Err(format!("{}: already exists", target_manifest.display()));
    }

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for relative in referenced_files(&manifest) {
        check_relative(&id, &relative)?;
        let from = source.join(&relative);
        let bytes = std::fs::read(&from).map_err(|e| {
            format!(
                "{}: references {relative:?}, which cannot be read: {e}",
                manifest_path.display()
            )
        })?;
        if !files.iter().any(|(name, _)| name == &relative) {
            files.push((relative, bytes));
        }
    }
    let manifest_bytes =
        std::fs::read(&manifest_path).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    std::fs::create_dir_all(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;

    let mut written: Vec<PathBuf> = Vec::new();
    let result = (|| {
        std::fs::write(&target_manifest, &manifest_bytes)
            .map_err(|e| format!("{}: {e}", target_manifest.display()))?;
        written.push(target_manifest.clone());
        for (relative, bytes) in &files {
            let to = destination.join(relative);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&to, bytes).map_err(|e| format!("{}: {e}", to.display()))?;
            written.push(to);
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in written.iter().rev() {
            let _ = std::fs::remove_file(path);
        }
        return Err(error);
    }

    Ok(Installed {
        id,
        path: target_manifest,
    })
}

/// The bundle's one manifest: `part.toml` when present, otherwise the only
/// `*.toml` in the directory.
fn bundle_manifest(source: &Path) -> Result<PathBuf, String> {
    let named = source.join("part.toml");
    if named.is_file() {
        return Ok(named);
    }
    let files = toml_files(source)?;
    match files.as_slice() {
        [] => Err(format!(
            "{}: no part.toml and no *.toml manifest",
            source.display()
        )),
        [only] => Ok(only.clone()),
        many => Err(format!(
            "{}: expected one manifest, found {}: {}",
            source.display(),
            many.len(),
            many.iter()
                .filter_map(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Every file a manifest names relative to itself. A schematic a board ships
/// is checked too; it is needed for the same reason a wasm module is.
fn referenced_files(manifest: &PartManifest) -> Vec<String> {
    let mut files = Vec::new();
    if let Simulation::Wasm { module, .. } = &manifest.simulation {
        files.push(module.clone());
    }
    if let Some(board) = &manifest.board {
        if let Some(simulator) = &board.simulator {
            files.push(simulator.module.clone());
        }
        if let Some(schematic) = &board.schematic {
            files.push(schematic.clone());
        }
    }
    files
}

/// The wasm modules a manifest names. These, and only these, may be removed
/// with the manifest.
fn referenced_modules(manifest: &PartManifest) -> Vec<String> {
    let mut modules = Vec::new();
    if let Simulation::Wasm { module, .. } = &manifest.simulation {
        modules.push(module.clone());
    }
    if let Some(simulator) = manifest
        .board
        .as_ref()
        .and_then(|board| board.simulator.as_ref())
    {
        modules.push(simulator.module.clone());
    }
    modules
}

/// A referenced file must be a plain relative path: no root and no `..`.
fn check_relative(id: &str, path: &str) -> Result<(), String> {
    if path.trim().is_empty()
        || Path::new(path).is_absolute()
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(format!(
            "{id}: {path:?} must be relative to the manifest and stay inside its directory"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------- scaffolding

/// Write a starter manifest to `project/dir/<id>.toml`.
///
/// The scaffold declares two passive pins and leaves the backend commented:
/// the generated file is valid TOML, but the author chooses the engine before
/// the catalog can load it.
pub fn new_part(
    project: &Path,
    dir: &str,
    id: &str,
    name: Option<&str>,
) -> Result<PathBuf, String> {
    validate_id(id)?;

    let text = scaffold(id, name.unwrap_or(id));
    toml::from_str::<toml::Value>(&text)
        .map_err(|e| format!("generated manifest is invalid: {e}"))?;

    let destination = project.join(dir);
    std::fs::create_dir_all(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;
    let path = destination.join(format!("{id}.toml"));
    if path.exists() {
        return Err(format!("{}: already exists", path.display()));
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

fn scaffold(id: &str, name: &str) -> String {
    format!(
        r#"# Litz part manifest.
#
# Choose one simulation backend and uncomment it:
#
#   [simulation]
#   backend = "builtin"
#   engine = "led"
#
# or:
#
#   [simulation]
#   backend = "wasm"
#   module = "part.wasm"

[part]
id = "{id}"
name = "{name}"
version = "0.1.0"
description = ""
license = "MIT"

[pins."1"]
kind = "passive"
number = 1

[pins."2"]
kind = "passive"
number = 2
"#,
        name = escape(name)
    )
}

/// Quote-safe text for the scaffold's one free-form field.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// A catalog id is also a file and directory name, so it stays a plain name.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.starts_with('.')
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "{id:?}: an id may contain only letters, digits, '-', '_' and '.'"
        ));
    }
    Ok(())
}

// ------------------------------------------------------------ board scaffolding

/// The files `new_board` writes live in `templates/board`, one template per
/// file, so the repository documents the generated layout. `onboard.json` is
/// renamed to `<id>.onboard.json` when written, because the manifest resolves
/// the schematic by that name.
const BOARD_MANIFEST: &str = include_str!("../templates/board/board.toml");
const BOARD_SCHEMATIC: &str = include_str!("../templates/board/onboard.json");
const SIMULATOR_MANIFEST: &str = include_str!("../templates/board/simulator/Cargo.toml.in");
const SIMULATOR_SOURCE: &str = include_str!("../templates/board/simulator/src/lib.rs");
const SIMULATOR_BUILD: &str = include_str!("../templates/board/simulator/build.sh");
const BOARD_README: &str = include_str!("../templates/board/README.md");
/// The contract copy placed next to `simulator/`. The test module keeps it
/// byte-identical to `crates/litz-mcu-wit/wit`.
const BOARD_WIT: &str = include_str!("../templates/board/wit/litz-mcu.wit");

/// Every file of a board bundle, relative to the board directory.
fn board_files(id: &str, name: &str) -> Vec<(String, String)> {
    let krate = format!("{id}-sim");
    let name = escape(name);
    let render = |template: &str| {
        template
            .replace("{{id}}", id)
            // Longer placeholder first; `{{crate}}` is not a prefix of it.
            .replace("{{crate_snake}}", &krate.replace('-', "_"))
            .replace("{{crate}}", &krate)
            .replace("{{name}}", &name)
    };
    vec![
        ("board.toml".into(), render(BOARD_MANIFEST)),
        (format!("{id}.onboard.json"), render(BOARD_SCHEMATIC)),
        ("simulator/Cargo.toml".into(), render(SIMULATOR_MANIFEST)),
        ("simulator/src/lib.rs".into(), render(SIMULATOR_SOURCE)),
        ("simulator/build.sh".into(), render(SIMULATOR_BUILD)),
        ("wit/litz-mcu.wit".into(), render(BOARD_WIT)),
        ("README.md".into(), render(BOARD_README)),
    ]
}

/// Write a starter board bundle to `project/dir/<id>/`.
///
/// Unlike [`new_part`], the result is a directory: a board is a manifest, an
/// onboard schematic, a guest source tree and the WIT contract it builds
/// against. `catalog add` reads the directory as one bundle and installs the
/// manifest, schematic and built module.
///
/// The id is used as a catalog id, so `board-` is added when the caller did
/// not write it. An existing non-empty directory is refused rather than
/// merged.
pub fn new_board(
    project: &Path,
    dir: &str,
    id: &str,
    name: Option<&str>,
) -> Result<PathBuf, String> {
    validate_id(id)?;
    // The default name follows what the author typed, not the catalog prefix.
    let name = name.unwrap_or(id);
    let id = if id.starts_with("board-") {
        id.to_string()
    } else {
        format!("board-{id}")
    };

    let destination = project.join(dir).join(&id);
    if destination.exists() {
        let empty = std::fs::read_dir(&destination)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false);
        if !empty {
            return Err(format!("{}: already exists", destination.display()));
        }
    }

    // Render and parse before touching the disk, so a bad template cannot
    // leave a half-written board behind.
    let files = board_files(&id, name);
    let manifest = files
        .iter()
        .find(|(relative, _)| relative.as_str() == "board.toml")
        .map(|(_, text)| text)
        .expect("board_files always writes the manifest");
    toml::from_str::<toml::Value>(manifest)
        .map_err(|e| format!("generated board manifest is invalid: {e}"))?;

    std::fs::create_dir_all(&destination).map_err(|e| format!("{}: {e}", destination.display()))?;
    let result = (|| {
        for (relative, text) in &files {
            let path = destination.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&destination);
        return Err(error);
    }
    // The README runs the script directly; a plain write is not executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = destination.join("simulator/build.sh");
        if let Ok(metadata) = std::fs::metadata(&script) {
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            let _ = std::fs::set_permissions(&script, permissions);
        }
    }
    Ok(destination)
}

// ------------------------------------------------------------------ removing

/// Remove an installed manifest and the wasm module it alone names.
///
/// A module another manifest in the same directory still names is kept. A
/// builtin id is refused: the catalog ships it, and no project file owns it.
pub fn remove(project: &Path, dir: &str, id: &str) -> Result<PathBuf, String> {
    if Registry::builtin().get(id).is_some() {
        return Err(format!("{id}: builtin entries cannot be removed"));
    }

    let destination = project.join(dir);
    let mut found: Vec<(PathBuf, PartManifest)> = Vec::new();
    for file in toml_files(&destination)? {
        let manifest = parse_file(&file)?;
        if manifest.id == id {
            found.push((file, manifest));
        }
    }
    let (manifest_path, manifest) = match found.len() {
        0 => {
            return Err(format!("{id}: not installed in {}", destination.display()));
        }
        1 => found.pop().expect("one entry"),
        _ => {
            return Err(format!(
                "{id}: declared by more than one manifest in {}",
                destination.display()
            ));
        }
    };

    // Module paths are relative to the manifest, which for a board is the
    // board's own directory rather than `boards/` itself.
    let base = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| destination.clone());
    let modules: Vec<PathBuf> = referenced_modules(&manifest)
        .into_iter()
        .map(|module| base.join(module))
        .collect();
    let mut shared = BTreeSet::new();
    for file in toml_files(&destination)? {
        if file == manifest_path {
            continue;
        }
        let owner = file.parent().map(Path::to_path_buf).unwrap_or_default();
        for module in referenced_modules(&parse_file(&file)?) {
            shared.insert(owner.join(module));
        }
    }

    std::fs::remove_file(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    for path in modules {
        if shared.contains(&path) {
            continue;
        }
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    // A board's own schematic is not shared with any other manifest.
    if let Some(schematic) = manifest
        .board
        .as_ref()
        .and_then(|board| board.schematic.as_ref())
    {
        let path = base.join(schematic);
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    // A board lives in its own directory. Once its files are gone the empty
    // directory is noise; one that still holds the author's simulator source
    // is left alone.
    if base != destination && std::fs::read_dir(&base).is_ok_and(|mut d| d.next().is_none()) {
        let _ = std::fs::remove_dir(&base);
    }
    Ok(manifest_path)
}

// -------------------------------------------------------------- file helpers

/// Every `*.toml` in `dir`, sorted. A missing directory is empty, not an
/// error.
fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            // A board bundle is `boards/<id>/board.toml`; its simulator
            // crate's `Cargo.toml` is not a manifest.
            if let Ok(children) = std::fs::read_dir(&path) {
                for child in children {
                    let child = child.map_err(|e| format!("{}: {e}", path.display()))?;
                    let candidate = child.path();
                    if candidate.extension().is_some_and(|ext| ext == "toml")
                        && candidate
                            .file_name()
                            .is_none_or(|name| name != "Cargo.toml")
                    {
                        files.push(candidate);
                    }
                }
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "toml")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn parse_file(path: &Path) -> Result<PartManifest, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_text(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn parse_text(text: &str) -> Result<PartManifest, String> {
    let mut registry = Registry::new();
    let id = registry.insert_text(text)?;
    registry
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("{id}: missing after parsing"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(entry: &'a Value, name: &str) -> &'a Value {
        entry
            .get(name)
            .unwrap_or_else(|| panic!("catalog entry has no {name:?}: {entry}"))
    }

    #[test]
    fn catalog_json_names_every_builtin_entry_once() {
        let registry = Registry::builtin();
        let catalog = value(&registry);
        let entries = catalog.as_array().expect("the catalog is an array");
        assert_eq!(entries.len(), registry.ids().count());

        let mut ids: Vec<&str> = entries
            .iter()
            .map(|entry| field(entry, "id").as_str().expect("id is a string"))
            .collect();
        let sorted = ids.clone();
        ids.sort_unstable();
        assert_eq!(ids, sorted, "entries are ordered by id");
        let unique: std::collections::BTreeSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "an id appears once");
    }

    #[test]
    fn catalog_entries_have_the_front_end_shape() {
        let catalog = value(&Registry::builtin());
        let entries = catalog.as_array().unwrap();

        for entry in entries {
            let object = entry.as_object().expect("an entry is an object");
            let keys: std::collections::BTreeSet<&str> =
                object.keys().map(String::as_str).collect();
            assert_eq!(
                keys,
                ["id", "kind", "name", "description", "pins", "attrs"]
                    .into_iter()
                    .collect::<std::collections::BTreeSet<_>>(),
                "unexpected keys on {entry}"
            );
            assert!(matches!(
                field(entry, "kind").as_str(),
                Some("part" | "board")
            ));
            assert!(field(entry, "name").is_string());
            assert!(field(entry, "description").is_string());

            let pins = field(entry, "pins").as_array().expect("pins is an array");
            for pin in pins {
                let keys: std::collections::BTreeSet<&str> = pin
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(keys, ["name", "kind"].into_iter().collect());
                assert!(pin["name"].is_string());
                assert!(pin["kind"].is_string());
            }

            let attrs = field(entry, "attrs").as_array().expect("attrs is an array");
            for attr in attrs {
                let keys: std::collections::BTreeSet<&str> = attr
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                assert_eq!(
                    keys,
                    ["name", "type", "values", "default"].into_iter().collect()
                );
                assert!(attr["name"].is_string());
                assert!(attr["type"].is_string());
                assert!(attr["values"].is_array());
            }
        }
    }

    #[test]
    fn a_part_and_a_board_describe_themselves() {
        let catalog = value(&Registry::builtin());
        let entries = catalog.as_array().unwrap();

        let led = entries
            .iter()
            .find(|entry| entry["id"] == "litz-led")
            .expect("litz-led");
        assert_eq!(led["kind"], "part");
        assert_eq!(led["name"], "LED");
        let pins = led["pins"].as_array().unwrap();
        assert!(
            pins.iter()
                .any(|pin| pin["name"] == "A" && pin["kind"] == "digital-in")
        );
        assert!(
            pins.iter()
                .any(|pin| pin["name"] == "C" && pin["kind"] == "ground")
        );
        let color = led["attrs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|attr| attr["name"] == "color")
            .expect("LED has a colour");
        assert_eq!(color["type"], "enum");
        assert_eq!(color["default"], "red");
        assert!(
            color["values"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == "green")
        );

        let board = entries
            .iter()
            .find(|entry| entry["id"] == "board-esp32-c3-devkitm-1")
            .expect("the ESP32-C3 board");
        assert_eq!(board["kind"], "board");
        let pins = board["pins"].as_array().unwrap();
        assert!(
            pins.iter()
                .any(|pin| pin["name"] == "GPIO8" && pin["kind"] == "digital-io")
        );
        assert!(
            pins.iter()
                .any(|pin| pin["name"] == "GND" && pin["kind"] == "ground")
        );
    }

    #[test]
    fn the_text_form_is_valid_json() {
        let text = builtin_json();
        let parsed: Value = serde_json::from_str(&text).expect("the command prints JSON");
        assert_eq!(
            parsed.as_array().unwrap().len(),
            Registry::builtin().ids().count()
        );
    }

    // ------------------------------------------------------------ scratch dirs

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("litz-catalog-{tag}-{}-{n}", std::process::id()));
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

    fn part_text(id: &str, name: &str) -> String {
        format!(
            r#"
[part]
id = "{id}"
name = "{name}"
version = "0.1.0"
[pins.P]
kind = "passive"
[simulation]
backend = "builtin"
engine = "x"
"#
        )
    }

    fn board_text(id: &str, name: &str) -> String {
        format!(
            r#"
[board]
id = "{id}"
name = "{name}"
chip = "esp32c3"
[[pins]]
name = "GPIO0"
gpio = 0
"#
        )
    }

    fn wasm_part_text(id: &str, module: &str) -> String {
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
        )
    }

    fn row<'a>(rows: &'a [Row], id: &str) -> &'a Row {
        rows.iter()
            .find(|row| row.id == id)
            .unwrap_or_else(|| panic!("{id} is missing from the listing"))
    }

    // ------------------------------------------------------------------ tables

    #[test]
    fn the_human_table_lists_every_builtin_entry() {
        let text = builtin_table();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            Registry::builtin().ids().count() + 1,
            "one header and one row per entry"
        );
        assert!(lines[0].starts_with("id"), "{}", lines[0]);
        assert!(lines[0].contains("kind"), "{}", lines[0]);
        assert!(lines[0].contains("name"), "{}", lines[0]);
        assert!(lines[0].contains("pins"), "{}", lines[0]);
        let led = lines
            .iter()
            .find(|line| line.starts_with("litz-led "))
            .expect("litz-led is listed");
        assert!(led.contains("part"), "{led}");
        assert!(led.ends_with('2'), "the LED has two pins: {led}");
        assert!(
            !lines[0].contains("source"),
            "the plain catalog has no source column"
        );
    }

    #[test]
    fn listing_marks_builtin_parts_and_boards() {
        let dir = TempDir::new("listing");
        std::fs::create_dir_all(dir.path().join("parts")).unwrap();
        std::fs::write(
            dir.path().join("parts/vendor-thing.toml"),
            part_text("vendor-thing", "Vendor Thing"),
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("boards")).unwrap();
        std::fs::write(
            dir.path().join("boards/board-vendor.toml"),
            board_text("board-vendor", "Vendor Board"),
        )
        .unwrap();

        let rows = listing(dir.path()).expect("the project lists");
        assert_eq!(row(&rows, "litz-led").source, "builtin");
        let part = row(&rows, "vendor-thing");
        assert_eq!(part.source, "parts/");
        assert_eq!(part.kind, "part");
        assert_eq!(part.name, "Vendor Thing");
        assert_eq!(part.pins, 1);
        let board = row(&rows, "board-vendor");
        assert_eq!(board.source, "boards/");
        assert_eq!(board.kind, "board");

        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "rows are ordered by id");

        let table = table(&rows, true);
        assert!(table.lines().next().unwrap().contains("source"));
        assert!(table.contains("vendor-thing"), "{table}");
    }

    // ------------------------------------------------------------------ checks

    #[test]
    fn check_counts_manifests_and_names_the_bad_file() {
        let dir = TempDir::new("check");
        std::fs::create_dir_all(dir.path().join("parts")).unwrap();
        std::fs::write(
            dir.path().join("parts/good.toml"),
            part_text("good", "Good"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("parts/bad.toml"),
            "[part]\nid = \"bad\"\nname = \"Bad\"\n",
        )
        .unwrap();

        let report = check_dir(dir.path());
        assert_eq!(report.checked, 2);
        assert_eq!(report.errors.len(), 1, "{:?}", report.errors);
        assert!(report.errors[0].contains("bad.toml"), "{:?}", report.errors);
        assert!(
            report.errors[0].contains("version"),
            "the parse error explains itself: {:?}",
            report.errors
        );
    }

    #[test]
    fn check_of_a_missing_project_is_empty() {
        let dir = TempDir::new("check-empty");
        let report = check_dir(dir.path());
        assert_eq!(report.checked, 0);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
    }

    // -------------------------------------------------------------------- add

    fn write_bundle(dir: &Path, id: &str, module: &str, with_module: bool) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("part.toml"), wasm_part_text(id, module)).unwrap();
        if with_module {
            std::fs::write(dir.join(module), b"\0asm").unwrap();
        }
    }

    #[test]
    fn add_copies_the_manifest_and_its_module() {
        let scratch = TempDir::new("add");
        let bundle = scratch.path().join("bundle");
        let project = scratch.path().join("project");
        write_bundle(&bundle, "vendor-mirror", "mirror.wasm", true);

        let installed = add(&bundle, &project, "parts").expect("the bundle installs");
        assert_eq!(installed.id, "vendor-mirror");
        assert_eq!(installed.path, project.join("parts/part.toml"));
        assert!(installed.path.is_file());
        assert!(project.join("parts/mirror.wasm").is_file());

        let rows = listing(&project).expect("the project lists");
        let added = row(&rows, "vendor-mirror");
        assert_eq!(added.source, "parts/");
    }

    #[test]
    fn add_refuses_a_builtin_id() {
        let scratch = TempDir::new("add-builtin");
        let bundle = scratch.path().join("bundle");
        write_bundle(&bundle, "litz-led", "mirror.wasm", true);

        let error = add(&bundle, &scratch.path().join("project"), "parts").unwrap_err();
        assert!(error.contains("builtin"), "{error}");
        assert!(!scratch.path().join("project/parts").exists());
    }

    #[test]
    fn add_refuses_a_second_install_of_the_same_id() {
        let scratch = TempDir::new("add-twice");
        let bundle = scratch.path().join("bundle");
        let project = scratch.path().join("project");
        write_bundle(&bundle, "vendor-mirror", "mirror.wasm", true);

        add(&bundle, &project, "parts").expect("the first install works");
        let error = add(&bundle, &project, "parts").unwrap_err();
        assert!(error.contains("already installed"), "{error}");
    }

    #[test]
    fn add_refuses_a_missing_module_without_copying_anything() {
        let scratch = TempDir::new("add-missing");
        let bundle = scratch.path().join("bundle");
        let project = scratch.path().join("project");
        write_bundle(&bundle, "vendor-mirror", "mirror.wasm", false);

        let error = add(&bundle, &project, "parts").unwrap_err();
        assert!(error.contains("mirror.wasm"), "{error}");
        assert!(!project.join("parts").exists(), "nothing was written");
    }

    #[test]
    fn add_refuses_a_module_outside_the_bundle() {
        let scratch = TempDir::new("add-escape");
        let bundle = scratch.path().join("bundle");
        write_bundle(&bundle, "vendor-mirror", "../evil.wasm", true);

        let error = add(&bundle, &scratch.path().join("project"), "parts").unwrap_err();
        assert!(error.contains("relative"), "{error}");
        assert!(error.contains("evil.wasm"), "{error}");
    }

    // -------------------------------------------------------------------- new

    #[test]
    fn new_writes_a_parseable_manifest_and_refuses_to_overwrite() {
        let scratch = TempDir::new("new");
        let path = new_part(
            scratch.path(),
            "parts",
            "vendor-switch",
            Some("Vendor Switch"),
        )
        .expect("the scaffold is written");
        assert_eq!(path, scratch.path().join("parts/vendor-switch.toml"));

        let text = std::fs::read_to_string(&path).unwrap();
        toml::from_str::<toml::Value>(&text).expect("the scaffold is valid TOML");
        assert!(text.contains("id = \"vendor-switch\""), "{text}");
        assert!(text.contains("name = \"Vendor Switch\""), "{text}");
        assert!(text.contains("[pins.\"1\"]"), "{text}");
        assert!(text.contains("[pins.\"2\"]"), "{text}");
        assert!(
            !text
                .lines()
                .any(|line| !line.trim_start().starts_with('#') && line.contains("backend")),
            "the backend is left for the author: {text}"
        );

        let error = new_part(scratch.path(), "parts", "vendor-switch", None).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn new_refuses_an_id_that_is_not_a_plain_name() {
        let scratch = TempDir::new("new-id");
        for id in ["", ".hidden", "../escape", "two words", "a/b"] {
            let error = new_part(scratch.path(), "parts", id, None).unwrap_err();
            assert!(error.contains("may contain only"), "{id:?}: {error}");
        }
    }

    // -------------------------------------------------------------- new board

    #[test]
    fn new_board_writes_a_buildable_bundle_and_refuses_to_overwrite() {
        let scratch = TempDir::new("new-board");
        let path = new_board(scratch.path(), "boards", "vendor-mcu", Some("Vendor MCU"))
            .expect("the board scaffold is written");

        let board = scratch.path().join("boards/board-vendor-mcu");
        assert_eq!(path, board);
        for relative in [
            "board.toml",
            "board-vendor-mcu.onboard.json",
            "simulator/Cargo.toml",
            "simulator/src/lib.rs",
            "simulator/build.sh",
            "wit/litz-mcu.wit",
            "README.md",
        ] {
            assert!(board.join(relative).is_file(), "{relative} is missing");
        }

        let text = std::fs::read_to_string(board.join("board.toml")).unwrap();
        let manifest = parse_text(&text).expect("the generated manifest parses");
        assert_eq!(manifest.id, "board-vendor-mcu");
        assert_eq!(manifest.name, "Vendor MCU");
        let info = manifest.board.as_ref().expect("a board");
        assert_eq!(
            info.schematic.as_deref(),
            Some("board-vendor-mcu.onboard.json")
        );
        let simulator = info.simulator.as_ref().expect("a wasm MCU");
        assert_eq!(simulator.abi, "litz-mcu-2");
        assert!(
            !board.join("wit-v2").exists(),
            "the scaffold has one contract folder, wit/"
        );
        assert_eq!(simulator.module, "board-vendor-mcu.wasm");

        // The Cargo package and the core module derive from the same id.
        let cargo = std::fs::read_to_string(board.join("simulator/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"board-vendor-mcu-sim\""), "{cargo}");
        let build = std::fs::read_to_string(board.join("simulator/build.sh")).unwrap();
        assert!(
            build.contains("board_vendor_mcu_sim.wasm"),
            "the cargo target name is snake_case: {build}"
        );
        assert!(
            build.contains("../board-vendor-mcu.wasm"),
            "the component is written next to the manifest: {build}"
        );

        let error = new_board(scratch.path(), "boards", "vendor-mcu", None).unwrap_err();
        assert!(error.contains("already exists"), "{error}");
    }

    #[test]
    fn new_board_keeps_an_existing_prefix_and_validates_the_id() {
        let scratch = TempDir::new("new-board-id");
        let path = new_board(scratch.path(), "boards", "board-already", None)
            .expect("the scaffold is written");
        assert_eq!(path, scratch.path().join("boards/board-already"));
        assert!(path.join("board-already.onboard.json").is_file());
        assert!(
            !scratch.path().join("boards/board-board-already").exists(),
            "an id that already names a board is not prefixed twice"
        );

        for id in ["", ".hidden", "../escape", "two words", "a/b"] {
            let error = new_board(scratch.path(), "boards", id, None).unwrap_err();
            assert!(error.contains("may contain only"), "{id:?}: {error}");
        }
    }

    #[test]
    fn the_board_templates_are_the_source_of_every_generated_file() {
        // The copy of the contract in templates/board must not drift from the
        // one the host is built against.
        assert_eq!(
            BOARD_WIT,
            include_str!("../../litz-mcu-wit/wit/litz-mcu.wit"),
            "templates/board/wit must mirror crates/litz-mcu-wit/wit"
        );

        let scratch = TempDir::new("new-board-template");
        let path = new_board(scratch.path(), "boards", "example", Some("Example")).unwrap();
        let render = |template: &str| {
            template
                .replace("{{id}}", "board-example")
                .replace("{{crate_snake}}", "board_example_sim")
                .replace("{{crate}}", "board-example-sim")
                .replace("{{name}}", "Example")
        };
        for (relative, template) in [
            ("board.toml", BOARD_MANIFEST),
            ("board-example.onboard.json", BOARD_SCHEMATIC),
            ("simulator/Cargo.toml", SIMULATOR_MANIFEST),
            ("simulator/src/lib.rs", SIMULATOR_SOURCE),
            ("simulator/build.sh", SIMULATOR_BUILD),
            ("wit/litz-mcu.wit", BOARD_WIT),
            ("README.md", BOARD_README),
        ] {
            let written = std::fs::read_to_string(path.join(relative)).unwrap();
            assert_eq!(written, render(template), "{relative} is not its template");
        }
    }

    // ----------------------------------------------------------------- remove

    #[test]
    fn remove_deletes_the_manifest_and_its_own_module() {
        let scratch = TempDir::new("remove");
        let bundle = scratch.path().join("bundle");
        let project = scratch.path().join("project");
        write_bundle(&bundle, "vendor-mirror", "mirror.wasm", true);
        add(&bundle, &project, "parts").expect("install");

        let removed = remove(&project, "parts", "vendor-mirror").expect("remove");
        assert_eq!(removed, project.join("parts/part.toml"));
        assert!(!removed.exists());
        assert!(
            !project.join("parts/mirror.wasm").exists(),
            "the module went with its only manifest"
        );
    }

    #[test]
    fn remove_keeps_a_module_another_manifest_uses() {
        let scratch = TempDir::new("remove-shared");
        let project = scratch.path().join("project");
        let parts = project.join("parts");
        std::fs::create_dir_all(&parts).unwrap();
        std::fs::write(
            parts.join("a.toml"),
            wasm_part_text("vendor-a", "shared.wasm"),
        )
        .unwrap();
        std::fs::write(
            parts.join("b.toml"),
            wasm_part_text("vendor-b", "shared.wasm"),
        )
        .unwrap();
        std::fs::write(parts.join("shared.wasm"), b"\0asm").unwrap();

        remove(&project, "parts", "vendor-a").expect("remove a");
        assert!(!parts.join("a.toml").exists());
        assert!(parts.join("shared.wasm").is_file(), "b still names it");

        remove(&project, "parts", "vendor-b").expect("remove b");
        assert!(!parts.join("shared.wasm").exists());
    }

    #[test]
    fn remove_refuses_a_builtin_id() {
        let scratch = TempDir::new("remove-builtin");
        let error = remove(scratch.path(), "parts", "litz-led").unwrap_err();
        assert!(error.contains("builtin"), "{error}");
    }

    #[test]
    fn remove_refuses_an_id_the_project_does_not_have() {
        let scratch = TempDir::new("remove-missing");
        std::fs::create_dir_all(scratch.path().join("parts")).unwrap();
        let error = remove(scratch.path(), "parts", "vendor-nothing").unwrap_err();
        assert!(error.contains("not installed"), "{error}");
    }
}

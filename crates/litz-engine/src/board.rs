//! Checking a board bundle before a project depends on it.
//!
//! A board manifest is easy to get subtly wrong — a module built against the
//! other ABI, a schematic that names a pin the header does not have — and the
//! failure normally appears only when a diagram tries to boot it. This module
//! answers the same questions `litz run` would, without a project or a
//! firmware: does the manifest parse, does the module instantiate under the
//! ABI it names, and does the onboard schematic describe a `$chip`?

use litz_board::chip::Chip;
use litz_catalog::Registry;
use std::path::{Path, PathBuf};

/// What a board bundle contains, once it has been checked.
#[derive(Clone, Debug)]
pub struct BoardReport {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The chip backend, when the board names a builtin one.
    pub chip: Option<String>,
    /// The ABI and module, when the board carries a component.
    pub simulator: Option<(String, String)>,
    /// What the component says it is, when it could be instantiated here.
    pub module_name: Option<String>,
    /// The rate the guest counts `run-result.cycles` at.
    pub cpu_hz: Option<u64>,
    pub pins: usize,
    pub wires: usize,
}

impl BoardReport {
    pub fn lines(&self) -> String {
        let mut text = format!("{} {} ({})\n", self.id, self.version, self.name);
        match (&self.chip, &self.simulator) {
            (Some(chip), None) => text.push_str(&format!("  chip {chip}: builtin\n")),
            (None, Some((abi, module))) => {
                text.push_str(&format!("  module {module} ({abi})\n"));
                if let Some(name) = &self.module_name {
                    text.push_str(&format!("  component reports {name}\n"));
                }
                if let Some(hz) = self.cpu_hz {
                    text.push_str(&format!("  cpu {hz} Hz\n"));
                }
            }
            _ => {}
        }
        text.push_str(&format!("  header pins {}\n", self.pins));
        text.push_str(&format!("  schematic wires {}\n", self.wires));
        text
    }
}

/// Check one board directory or manifest.
///
/// The path may be a `board.toml` or the directory that holds it. Everything
/// named by the manifest is resolved relative to the manifest, the same rule
/// the catalog and the runner use.
pub fn check(path: &Path) -> Result<BoardReport, String> {
    let manifest_path = if path.is_dir() {
        let candidate = path.join("board.toml");
        if candidate.is_file() {
            candidate
        } else {
            // A distributed bundle may keep its own file name; accept the one
            // manifest in the directory, but never a simulator's Cargo.toml.
            let mut found: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|file| {
                    file.extension()
                        .is_some_and(|extension| extension == "toml")
                        && file.file_name().is_none_or(|name| name != "Cargo.toml")
                })
                .collect();
            found.sort();
            match found.as_slice() {
                [one] => one.clone(),
                _ => {
                    return Err(format!(
                        "{}: a board directory holds one manifest, `board.toml`",
                        path.display()
                    ));
                }
            }
        }
    } else {
        path.to_path_buf()
    };
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let mut registry = Registry::builtin();
    let id = registry
        .insert_text(&text)
        .map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    let manifest = registry
        .get(&id)
        .ok_or_else(|| format!("{}: the manifest did not load", manifest_path.display()))?
        .clone();
    let info = manifest.board.as_ref().ok_or_else(|| {
        format!(
            "{}: this is a part manifest, not a board",
            manifest_path.display()
        )
    })?;
    let dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    if let Some(chip) = &info.chip
        && !crate::runner::is_builtin_chip(chip)
    {
        return Err(format!(
            "{}: chip {chip:?} is not built in; name a wasm component under \
             [board.simulator] instead",
            manifest_path.display()
        ));
    }

    let mut wires = 0;
    if let Some(schematic) = &info.schematic {
        let path = dir.join(schematic);
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let circuit = litz_diagram::Circuit::from_json(&text)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if !circuit.parts.iter().any(|part| part.type_key == "$chip") {
            return Err(format!(
                "{}: the onboard schematic has no `$chip` part",
                path.display()
            ));
        }
        wires = circuit.connections.len();
    }

    let mut module_name = None;
    let mut cpu_hz = None;
    if let Some(simulator) = &info.simulator {
        let module = dir.join(&simulator.module);
        let bytes = std::fs::read(&module).map_err(|e| format!("{}: {e}", module.display()))?;
        let chip =
            litz_mcu_wasm::WasmChip::from_bytes_with_abi(&bytes, &simulator.module, &simulator.abi)
                .map_err(|e| {
                    format!(
                        "{}: does not instantiate as {}: {e}\n  the guest must be built against the unified WIT (crates/litz-mcu-wit/wit); `litz catalog new board` copies it as wit/",
                        module.display(),
                        simulator.abi
                    )
                })?;
        module_name = Some(chip.name().to_string());
        cpu_hz = Some(chip.info().cpu_hz);
    }

    Ok(BoardReport {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        chip: info.chip.clone(),
        simulator: info
            .simulator
            .as_ref()
            .map(|simulator| (simulator.abi.clone(), simulator.module.clone())),
        module_name,
        cpu_hz,
        pins: manifest.pins.len(),
        wires,
    })
}

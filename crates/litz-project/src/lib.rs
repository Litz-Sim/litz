//! One portable Litz project.
//!
//! A project is the circuit, the source files that belong to it, and the few
//! settings a runner needs. It is a single JSON document with a format tag and
//! a version, so it can be written by one frontend, read by another, and kept
//! in version control without a database.
//!
//! The circuit itself stays `diagram.json`-shaped: this crate does not invent
//! a second schematic format. What it adds is the parts a project needs around
//! it — files, file groups for multi-board canvases, and options — plus the
//! validation that keeps a hand-edited file from walking out of its own
//! directory.
//!
//! ```
//! use litz_project::Project;
//!
//! let mut project = Project::new("blink");
//! project.add_file("main.c", "int main(void) { return 0; }");
//! let text = project.to_json().unwrap();
//! let reopened = Project::from_json(&text).unwrap();
//! assert_eq!(reopened.name, "blink");
//! assert_eq!(reopened.files[0].name, "main.c");
//! ```

#![forbid(unsafe_code)]

use litz_diagram::Circuit;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The tag every project file carries.
pub const FORMAT: &str = "litz-project";
/// The format revision this crate writes. Older files load unchanged.
pub const VERSION: u32 = 1;

fn empty_circuit() -> Circuit {
    Circuit {
        version: 1,
        author: None,
        editor: None,
        parts: Vec::new(),
        connections: Vec::new(),
        extra: BTreeMap::new(),
    }
}

fn format_tag() -> String {
    FORMAT.to_string()
}

fn format_version() -> u32 {
    VERSION
}

/// One source file in the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    pub name: String,
    #[serde(default)]
    pub content: String,
    /// Which board's file group this file belongs to, for a multi-board
    /// project. `None` is the project's own group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

impl SourceFile {
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            content: content.into(),
            group: None,
        }
    }

    pub fn in_group(self, group: impl Into<String>) -> Self {
        Self {
            group: Some(group.into()),
            ..self
        }
    }
}

/// Runner settings stored with the project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Options {
    /// `micropython`, `firmware`, or a board-specific runtime name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// The image the project boots, relative to the project file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    /// Treat warnings as failures, matching `litz --strict`.
    #[serde(default)]
    pub strict: bool,
}

/// A complete project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    #[serde(default = "format_tag")]
    pub format: String,
    #[serde(default = "format_version")]
    pub version: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default = "empty_circuit")]
    pub circuit: Circuit,
    #[serde(default)]
    pub files: Vec<SourceFile>,
    #[serde(default)]
    pub options: Options,
    /// Unknown keys survive a save, as in the circuit format.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Why a project file was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    #[error("not a Litz project (format {0:?})")]
    WrongFormat(String),
    #[error("project version {0} is newer than this build understands ({VERSION})")]
    NewerVersion(u32),
    #[error("file name {0:?} is empty")]
    EmptyFileName(String),
    #[error("file name {0:?} must be a plain name, not a path")]
    UnsafeFileName(String),
    #[error("file name {0:?} is used twice")]
    DuplicateFileName(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Json(String),
}

impl Project {
    /// A new, empty project.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            format: FORMAT.to_string(),
            version: VERSION,
            name: name.into(),
            circuit: empty_circuit(),
            files: Vec::new(),
            options: Options::default(),
            extra: BTreeMap::new(),
        }
    }

    /// Add or replace a file in the project's own group.
    pub fn add_file(
        &mut self,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> &mut SourceFile {
        let file = SourceFile::new(name, content);
        self.files.retain(|f| f.name != file.name);
        self.files.push(file);
        self.files.last_mut().expect("just pushed")
    }

    pub fn file(&self, name: &str) -> Option<&SourceFile> {
        self.files.iter().find(|f| f.name == name)
    }

    /// Files belonging to one board's group.
    pub fn group(&self, group: Option<&str>) -> impl Iterator<Item = &SourceFile> {
        self.files
            .iter()
            .filter(move |f| f.group.as_deref() == group)
    }

    /// Structural checks a hand-edited file can fail.
    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.format != FORMAT {
            return Err(ProjectError::WrongFormat(self.format.clone()));
        }
        if self.version > VERSION {
            return Err(ProjectError::NewerVersion(self.version));
        }
        let mut seen = BTreeSet::new();
        for file in &self.files {
            if file.name.trim().is_empty() {
                return Err(ProjectError::EmptyFileName(file.name.clone()));
            }
            let safe = Path::new(&file.name)
                .components()
                .all(|c| matches!(c, std::path::Component::Normal(_)));
            if !safe || file.name.contains('/') || file.name.contains('\\') {
                return Err(ProjectError::UnsafeFileName(file.name.clone()));
            }
            if !seen.insert(file.name.clone()) {
                return Err(ProjectError::DuplicateFileName(file.name.clone()));
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, ProjectError> {
        serde_json::to_string_pretty(self).map_err(|e| ProjectError::Json(e.to_string()))
    }

    pub fn from_json(text: &str) -> Result<Self, ProjectError> {
        let project: Self =
            serde_json::from_str(text).map_err(|e| ProjectError::Json(e.to_string()))?;
        project.validate()?;
        Ok(project)
    }

    pub fn save(&self, path: &Path) -> Result<(), ProjectError> {
        self.validate()?;
        let text = self.to_json()?;
        std::fs::write(path, text).map_err(|e| ProjectError::Io(format!("{}: {e}", path.display())))
    }

    pub fn load(path: &Path) -> Result<Self, ProjectError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ProjectError::Io(format!("{}: {e}", path.display())))?;
        Self::from_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use litz_diagram::PartInstance;

    #[test]
    fn a_project_round_trips_with_its_files_and_circuit() {
        let mut project = Project::new("blink");
        project
            .circuit
            .parts
            .push(PartInstance::new("led1", "litz-led"));
        project.add_file("main.c", "int main(void) { return 0; }");
        project.options.runtime = Some("firmware".into());
        let text = project.to_json().unwrap();
        let again = Project::from_json(&text).unwrap();
        assert_eq!(again.name, "blink");
        assert_eq!(again.files.len(), 1);
        assert_eq!(again.circuit.parts.len(), 1);
        assert_eq!(again.options.runtime.as_deref(), Some("firmware"));
        assert_eq!(again.format, FORMAT);
    }

    #[test]
    fn unknown_keys_survive_a_save() {
        let text = r#"{"format":"litz-project","version":1,"name":"x","thumbnail":"a.png"}"#;
        let project = Project::from_json(text).unwrap();
        let written = project.to_json().unwrap();
        assert!(written.contains("thumbnail"));
    }

    #[test]
    fn a_path_in_a_file_name_is_refused() {
        let mut project = Project::new("x");
        project.add_file("../outside.c", "");
        assert!(matches!(
            project.validate(),
            Err(ProjectError::UnsafeFileName(_))
        ));
    }

    #[test]
    fn two_files_cannot_share_a_name() {
        let mut project = Project::new("x");
        project.files.push(SourceFile::new("main.c", "a"));
        project.files.push(SourceFile::new("main.c", "b"));
        assert!(matches!(
            project.validate(),
            Err(ProjectError::DuplicateFileName(_))
        ));
    }

    #[test]
    fn a_foreign_document_is_refused() {
        let text = r#"{"format":"other","version":1}"#;
        assert!(matches!(
            Project::from_json(text),
            Err(ProjectError::WrongFormat(_))
        ));
    }

    #[test]
    fn a_newer_version_is_refused_rather_than_guessed() {
        let text = format!(r#"{{"format":"litz-project","version":{}}}"#, VERSION + 1);
        assert!(matches!(
            Project::from_json(&text),
            Err(ProjectError::NewerVersion(_))
        ));
    }

    #[test]
    fn file_groups_are_separate() {
        let mut project = Project::new("multi");
        project
            .files
            .push(SourceFile::new("a.c", "").in_group("left"));
        project
            .files
            .push(SourceFile::new("b.c", "").in_group("right"));
        assert_eq!(project.group(Some("left")).count(), 1);
        assert_eq!(project.group(None).count(), 0);
    }
}

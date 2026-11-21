//! Keeping every path a tool touches inside the project the caller named.
//!
//! MCP clients send paths as text, often produced by a model. A project root
//! is the sandbox: a tool may read and write below it and nowhere else. Parent
//! traversal and absolute paths are refused before the filesystem is touched,
//! then the result is canonicalised so a symlink cannot widen the sandbox
//! either.

use std::path::{Component, Path, PathBuf};

/// A canonical project directory that tool paths are resolved against.
pub struct ProjectRoot {
    root: PathBuf,
}

impl ProjectRoot {
    pub fn open(path: &str) -> Result<Self, String> {
        if path.trim().is_empty() {
            return Err("the project root is empty".into());
        }
        let root = Path::new(path)
            .canonicalize()
            .map_err(|e| format!("project root {path:?}: {e}"))?;
        if !root.is_dir() {
            return Err(format!("project root {path:?} is not a directory"));
        }
        Ok(Self { root })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// A project-relative path with no `..` and no absolute prefix.
    fn relative(&self, rel: &str) -> Result<PathBuf, String> {
        if rel.trim().is_empty() {
            return Err("a project-relative path is empty".into());
        }
        let path = Path::new(rel);
        if path.is_absolute() {
            return Err(format!(
                "{rel:?} is absolute; paths are relative to the project root"
            ));
        }
        let mut out = self.root.clone();
        for component in path.components() {
            match component {
                Component::Normal(part) => out.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    return Err(format!("{rel:?} escapes the project root"));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(format!("{rel:?} is not a relative path"));
                }
            }
        }
        Ok(out)
    }

    /// An existing file or directory, canonicalised and checked.
    pub fn existing(&self, rel: &str) -> Result<PathBuf, String> {
        let candidate = self.relative(rel)?;
        let canonical = candidate
            .canonicalize()
            .map_err(|e| format!("{rel:?}: {e}"))?;
        if !canonical.starts_with(&self.root) {
            return Err(format!("{rel:?} escapes the project root"));
        }
        Ok(canonical)
    }

    /// A path to write, whose parent must already exist inside the root.
    ///
    /// An existing target is canonicalised too: writing through a symlink that
    /// points outside the project is the same escape as `..`.
    pub fn for_write(&self, rel: &str) -> Result<PathBuf, String> {
        let candidate = self.relative(rel)?;
        let parent = candidate
            .parent()
            .ok_or_else(|| format!("{rel:?} has no parent directory"))?;
        let canonical_parent = parent.canonicalize().map_err(|e| format!("{rel:?}: {e}"))?;
        if !canonical_parent.starts_with(&self.root) {
            return Err(format!("{rel:?} escapes the project root"));
        }
        let name = candidate
            .file_name()
            .ok_or_else(|| format!("{rel:?} names no file"))?;
        let target = canonical_parent.join(name);
        if target.exists() {
            let canonical = target.canonicalize().map_err(|e| format!("{rel:?}: {e}"))?;
            if !canonical.starts_with(&self.root) {
                return Err(format!("{rel:?} escapes the project root"));
            }
            return Ok(canonical);
        }
        Ok(target)
    }

    /// The path as written relative to the root, for a report.
    pub fn display_relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_and_absolute_paths_are_refused_before_io() {
        let dir = std::env::temp_dir();
        let root = ProjectRoot::open(&dir.to_string_lossy()).expect("the temp dir exists");
        assert!(root.existing("../Cargo.toml").is_err());
        assert!(root.existing("/etc/passwd").is_err());
        assert!(root.for_write("../escape.txt").is_err());
    }
}

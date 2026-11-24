//! Putting files on a card or a flash chip.
//!
//! A storage part can name a directory of host files; this module formats a
//! FAT volume on the device and copies the tree in, so a firmware that mounts
//! the card finds `main.py`, or the CSVs the diagram promised, on its first
//! read. `fatfs` does the on-disk work; the only decisions here are the
//! volume's fixed identity and which files go where.
//!
//! The formatter is deterministic: the volume id, the label and every
//! timestamp are constants, so two builds of the same diagram produce the
//! same bytes. `fatfs` is configured without `chrono`, which leaves the
//! default time provider on the FAT epoch rather than the wall clock.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use fatfs::{FileSystem, FormatVolumeOptions, FsOptions};

use crate::block::ImageIo;

/// A file to place on the volume: a `/`-separated path and its bytes.
pub struct File<'a> {
    pub path: &'a str,
    pub data: &'a [u8],
}

/// The volume id every formatted image carries (`LITZ`).
const VOLUME_ID: u32 = 0x4c49_545a;
/// The FAT label, space padded to eleven bytes.
const LABEL: &[u8; 11] = b"LITZ       ";

/// Format `image` as a FAT volume and copy `files` onto it.
///
/// The image must be sector aligned and at least one FAT12 volume's worth of
/// space; a smaller request is a diagram mistake, not something to round.
pub fn format(image: &mut crate::block::Image, files: &[File<'_>]) -> Result<(), String> {
    let capacity = image.capacity();
    let sectors = capacity / crate::block::SECTOR;
    if !capacity.is_multiple_of(crate::block::SECTOR) {
        return Err(format!(
            "a formatted volume needs a whole number of 512-byte sectors, not {capacity} bytes"
        ));
    }
    if sectors < 64 {
        return Err(format!(
            "a FAT volume needs at least 32 KiB, not {capacity} bytes"
        ));
    }
    let options = FormatVolumeOptions::new()
        .bytes_per_sector(crate::block::SECTOR as u16)
        .total_sectors(sectors as u32)
        .volume_id(VOLUME_ID)
        .volume_label(*LABEL);
    {
        let mut io = ImageIo::new(image);
        fatfs::format_volume(&mut io, options)
            .map_err(|error| format!("cannot format the volume: {error}"))?;
    }
    let mut io = ImageIo::new(image);
    let fs = FileSystem::new(&mut io, FsOptions::new())
        .map_err(|error| format!("cannot mount the new volume: {error}"))?;
    let root = fs.root_dir();
    for file in files {
        let path = normalize(file.path)?;
        // `create_file` walks the path but will not create its parents.
        let mut prefix = PathBuf::new();
        for component in path.components() {
            prefix.push(component.as_os_str());
            if prefix != path {
                root.create_dir(&path_text(&prefix))
                    .map_err(|error| format!("{}: {error}", prefix.display()))?;
            }
        }
        let mut entry = root
            .create_file(&path_text(&path))
            .map_err(|error| format!("{}: {error}", path.display()))?;
        entry
            .truncate()
            .and_then(|()| entry.write_all(file.data))
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    drop(root);
    fs.unmount()
        .map_err(|error| format!("cannot finish the volume: {error}"))?;
    Ok(())
}

/// The project files a `files` attribute names, as `(path, bytes)` pairs.
///
/// A directory is walked in sorted order so the image does not depend on the
/// host's directory order, and its contents land at the volume root (a
/// `files = "sdcard"` attribute puts `sdcard/main.py` at `/main.py`); a plain
/// file lands at the root under its own name. The named path is relative to
/// the project directory and may not climb out of it.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_tree(relative: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let base = std::env::current_dir().map_err(|error| format!("no current directory: {error}"))?;
    read_tree_in(relative, &base)
}

/// [`read_tree`] against an explicit project directory, so a test can use a
/// scratch tree without changing the process's working directory.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_tree_in(relative: &Path, base: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    let base = base
        .canonicalize()
        .map_err(|error| format!("the project directory is not readable: {error}"))?;
    let root = base.join(relative);
    let root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", root.display()))?;
    if !root.starts_with(&base) {
        return Err("the files resolve outside the project directory".into());
    }
    let mut out = Vec::new();
    if root.is_file() {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("{} has no UTF-8 name", root.display()))?;
        out.push((name.to_string(), read_file(&root)?));
    } else {
        walk(&root, &root, &mut out)?;
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(target_arch = "wasm32")]
pub fn read_tree(_relative: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    Err(
        "`files` reads a project directory, which a browser build does not have; \
         provide the image instead"
            .into(),
    )
}

#[cfg(not(target_arch = "wasm32"))]
fn walk(root: &Path, directory: &Path, out: &mut Vec<(String, Vec<u8>)>) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("cannot read {}: {error}", directory.display()))
        })
        .collect::<Result<_, _>>()?;
    entries.sort();
    for path in entries {
        // A symlink could point anywhere; the resolved path was already
        // checked for the root, and following links below it would reopen the
        // question for every entry.
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            out.push((volume_path(root, &path)?, read_file(&path)?));
        }
    }
    Ok(())
}

/// The path a file takes on the volume: relative to the directory the `files`
/// attribute named.
#[cfg(not(target_arch = "wasm32"))]
fn volume_path(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| format!("{} is outside {}", path.display(), root.display()))?;
    let text = relative
        .to_str()
        .ok_or_else(|| format!("{} has no UTF-8 name", relative.display()))?;
    Ok(text.replace('\\', "/"))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))
}

/// A path that is safe to hand to the formatter: relative, no `..`, no empty
/// component and no FAT metacharacter.
fn normalize(text: &str) -> Result<PathBuf, String> {
    let text = text.replace('\\', "/");
    let path = Path::new(&text);
    if text.starts_with('/') {
        return Err(format!("{text:?}: paths on a volume are relative"));
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name.is_empty() {
                    return Err(format!("{text:?}: an empty path component"));
                }
                if name.len() > 255 {
                    return Err(format!("{text:?}: a name longer than 255 bytes"));
                }
                if name.chars().any(|c| ":*?\"<>|".contains(c)) {
                    return Err(format!("{text:?}: {name:?} is not a FAT file name"));
                }
                out.push(part);
            }
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "{text:?}: the path may not climb out of the volume"
                ));
            }
        }
    }
    if out.as_os_str().is_empty() {
        return Err(format!("{text:?}: an empty path"));
    }
    Ok(out)
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Read every file on a formatted image back, for tests and for showing a
/// host what the guest wrote.
#[allow(dead_code)]
pub fn read_files(image: &crate::block::Image) -> Result<Vec<(String, Vec<u8>)>, String> {
    // Mounting updates access counters, so read from a copy and leave the
    // device's bytes untouched.
    let mut copy = crate::block::Image::from_bytes(image.to_vec());
    let mut io = ImageIo::new(&mut copy);
    let fs = FileSystem::new(&mut io, FsOptions::new())
        .map_err(|error| format!("cannot mount the volume: {error}"))?;
    let mut out = Vec::new();
    read_dir(&fs.root_dir(), "", &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[allow(dead_code)]
fn read_dir<T: fatfs::ReadWriteSeek>(
    dir: &fatfs::Dir<'_, T>,
    prefix: &str,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    for entry in dir.iter() {
        let entry = entry.map_err(|error| format!("cannot read a directory entry: {error}"))?;
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.is_dir() {
            let child = dir
                .open_dir(&name)
                .map_err(|error| format!("{path}: {error}"))?;
            read_dir(&child, &path, out)?;
        } else if entry.is_file() {
            let mut file = entry.to_file();
            let mut data = Vec::new();
            file.read_to_end(&mut data)
                .map_err(|error| format!("{path}: {error}"))?;
            out.push((path, data));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::Image;

    fn files() -> Vec<File<'static>> {
        vec![
            File {
                path: "README.TXT",
                data: b"hello from the card\n",
            },
            File {
                path: "data/log.csv",
                data: b"a,b\n1,2\n",
            },
            File {
                path: "data/2026-11-24.log",
                data: b"long names work\n",
            },
        ]
    }

    #[test]
    fn a_formatted_card_reads_its_files_back() {
        let mut image = Image::blank(1 << 20, 0);
        format(&mut image, &files()).unwrap();
        let read = read_files(&image).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].0, "README.TXT");
        assert_eq!(read[0].1, b"hello from the card\n");
        assert_eq!(read[1].0, "data/2026-11-24.log");
        assert_eq!(read[2].0, "data/log.csv");
        // Sector zero is a boot sector, not the blank fill.
        assert_eq!(&image.read_at(0, 3).unwrap(), &[0xeb, 0x3c, 0x90]);
        assert_eq!(&image.read_at(510, 2).unwrap(), &[0x55, 0xaa]);
    }

    #[test]
    fn two_formats_are_byte_identical() {
        let mut one = Image::blank(8 << 20, 0);
        let mut two = Image::blank(8 << 20, 0);
        format(&mut one, &files()).unwrap();
        format(&mut two, &files()).unwrap();
        assert_eq!(one.to_vec(), two.to_vec());
    }

    #[test]
    fn a_big_volume_chooses_fat32_and_still_round_trips() {
        // 512 MiB is where fatfs switches to FAT32; the file table is written
        // far past the first megabytes, so this also proves the sparse image
        // and the formatter agree on the laid-out geometry.
        let mut image = Image::blank(512 << 20, 0);
        format(&mut image, &files()).unwrap();
        let read = read_files(&image).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[2].1, b"a,b\n1,2\n");
    }

    #[test]
    fn a_path_outside_the_volume_is_refused() {
        let mut image = Image::blank(1 << 20, 0);
        let files = [File {
            path: "../escape.txt",
            data: b"no",
        }];
        assert!(format(&mut image, &files).is_err());
    }

    #[test]
    fn a_volume_that_is_too_small_is_refused() {
        let mut image = Image::blank(4096, 0);
        assert!(format(&mut image, &files()).is_err());
    }

    #[test]
    fn a_directory_tree_is_read_in_sorted_order() {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build");
        let name = format!("fat-tree-{}", std::process::id());
        let dir = base.join(&name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.txt"), b"bee").unwrap();
        std::fs::write(dir.join("sub/a.txt"), b"ay").unwrap();
        let files = read_tree_in(Path::new(&name), &base).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "b.txt");
        assert_eq!(
            files[1].0, "sub/a.txt",
            "the named directory is the volume root"
        );
        assert_eq!(files[1].1, b"ay");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

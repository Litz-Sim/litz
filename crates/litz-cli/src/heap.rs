//! Static memory and heap facts read from a firmware ELF.
//!
//! The chip model has no allocator: firmware `malloc` runs inside the guest
//! and the host never sees an allocation. What the image itself declares does
//! not need a running guest, though: the section table says how much RAM the
//! static data occupies, and the linked heap symbols say where the heap starts
//! and ends. This module reads those directly, without the `object` crate, so
//! `litz heap` answers offline and cannot drift from the image it describes.
//!
//! Runtime free/used heap needs a sample from the running firmware; that is a
//! separate, backend-specific facility and is not guessed here.

use std::path::Path;

const SHT_PROGBITS: u32 = 1;
const SHT_SYMTAB: u32 = 2;
const SHT_NOBITS: u32 = 8;
const SHF_ALLOC: u64 = 0x2;

/// Heap-region symbols an ESP-IDF linker script may leave in the image.
const HEAP_SYMBOLS: [&str; 6] = [
    "_heap_start",
    "__heap_start",
    "heap_start",
    "_heap_end",
    "__heap_end",
    "heap_end",
];

struct Section {
    name: String,
    kind: u32,
    addr: u64,
    size: u64,
    alloc: bool,
}

/// Explain a firmware's static RAM and heap bounds, or why it cannot.
pub fn report(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let elf = Elf::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;

    let mut out = String::new();
    out.push_str(&format!(
        "{}: ELF{} {}-endian\n",
        path.display(),
        elf.bits,
        if elf.little { "little" } else { "big" }
    ));

    let mut ram = 0u64;
    out.push_str("  section                 size        addr\n");
    for section in &elf.sections {
        if !section.alloc || !is_ram(section) {
            continue;
        }
        if section.kind == SHT_PROGBITS || section.kind == SHT_NOBITS {
            ram += section.size;
        }
        out.push_str(&format!(
            "  {:<20} {:>8}  0x{:08x}\n",
            section.name, section.size, section.addr
        ));
    }
    if ram == 0 {
        out.push_str("  (no allocated RAM sections)\n");
    } else {
        out.push_str(&format!("  static RAM: {ram} bytes\n"));
    }

    let mut bounds = Vec::new();
    for name in HEAP_SYMBOLS {
        if let Some(address) = elf.symbol(name) {
            bounds.push((name, address));
        }
    }
    if bounds.is_empty() {
        out.push_str(
            "  heap bounds: no _heap_start/_heap_end symbol in the image; the heap is \
             allocated at runtime or the ELF is stripped\n",
        );
    } else {
        for (name, address) in &bounds {
            out.push_str(&format!("  {name} = 0x{address:08x}\n"));
        }
        let start = bounds
            .iter()
            .find(|(name, _)| name.contains("start"))
            .map(|(_, address)| *address);
        let end = bounds
            .iter()
            .find(|(name, _)| name.contains("end"))
            .map(|(_, address)| *address);
        if let (Some(start), Some(end)) = (start, end)
            && end > start
        {
            out.push_str(&format!("  heap region: {} bytes\n", end - start));
        }
    }
    Ok(out)
}

/// Whether a section is loaded into RAM. The ESP linker names its RAM
/// sections `.data`, `.bss`, `.dram*`, `.iram*`, `.rtc*`; flash-resident
/// `.rodata`/`.text` are allocated but not RAM, and `.flash*` is explicitly
/// excluded.
fn is_ram(section: &Section) -> bool {
    let name = section.name.as_str();
    if name.starts_with(".rodata") || name.starts_with(".text") || name.starts_with(".flash") {
        return false;
    }
    matches!(section.kind, SHT_PROGBITS | SHT_NOBITS)
        && (name.starts_with(".data")
            || name.starts_with(".bss")
            || name.starts_with(".dram")
            || name.starts_with(".iram")
            || name.starts_with(".rtc")
            || name == ".noinit")
}

struct Elf {
    bits: u32,
    little: bool,
    sections: Vec<Section>,
    /// (name, address) in symbol-table order.
    symbols: Vec<(String, u64)>,
}

impl Elf {
    fn parse(bytes: &[u8]) -> Result<Elf, String> {
        if bytes.len() < 16 || !bytes.starts_with(b"\x7fELF") {
            return Err("not an ELF image".into());
        }
        let bits = match bytes[4] {
            1 => 32,
            2 => 64,
            other => return Err(format!("unknown ELF class {other}")),
        };
        let little = match bytes[5] {
            1 => true,
            2 => false,
            other => return Err(format!("unknown ELF data encoding {other}")),
        };
        let read = |offset: usize, len: usize| -> Result<u64, String> {
            let slice = bytes
                .get(offset..offset + len)
                .ok_or_else(|| "truncated ELF header".to_string())?;
            let mut value = 0u64;
            for (index, byte) in slice.iter().enumerate() {
                let shift = if little { index } else { len - 1 - index } * 8;
                value |= u64::from(*byte) << shift;
            }
            Ok(value)
        };
        let (shoff, shentsize, shnum, shstrndx) = if bits == 32 {
            (read(32, 4)?, read(46, 2)?, read(48, 2)?, read(50, 2)?)
        } else {
            (read(40, 8)?, read(58, 2)?, read(60, 2)?, read(62, 2)?)
        };
        if shoff == 0 || shnum == 0 {
            return Err("the image has no section table".into());
        }
        let shoff = shoff as usize;
        let entry = shentsize.max(1) as usize;
        let index = |n: u64| shoff.checked_add(n as usize * entry);

        let mut raw = Vec::with_capacity(shnum as usize);
        for n in 0..shnum {
            let at = index(n).ok_or_else(|| "section table out of range".to_string())?;
            let (name, kind, flags, addr, offset, size, link) = if bits == 32 {
                (
                    read(at, 4)?,
                    read(at + 4, 4)?,
                    read(at + 8, 4)?,
                    read(at + 12, 4)?,
                    read(at + 16, 4)?,
                    read(at + 20, 4)?,
                    read(at + 24, 4)?,
                )
            } else {
                (
                    read(at, 4)?,
                    read(at + 4, 4)?,
                    read(at + 8, 8)?,
                    read(at + 16, 8)?,
                    read(at + 24, 8)?,
                    read(at + 32, 8)?,
                    read(at + 40, 4)?,
                )
            };
            raw.push((name, kind, flags, addr, offset, size, link));
        }

        let shstr = raw
            .get(shstrndx as usize)
            .ok_or_else(|| "the section-name table is missing".to_string())?;
        let strtab = section_bytes(bytes, shstr.4, shstr.5)?;
        let name_at = |offset: u64| -> String {
            let start = offset as usize;
            let end = strtab[start..]
                .iter()
                .position(|b| *b == 0)
                .map(|n| start + n)
                .unwrap_or(strtab.len());
            String::from_utf8_lossy(&strtab[start..end]).into_owned()
        };

        let mut sections = Vec::with_capacity(raw.len());
        let mut symbols = Vec::new();
        for (name, kind, flags, addr, offset, size, link) in &raw {
            sections.push(Section {
                name: name_at(*name),
                kind: *kind as u32,
                addr: *addr,
                size: *size,
                alloc: flags & SHF_ALLOC != 0,
            });
            if *kind as u32 == SHT_SYMTAB {
                let table = section_bytes(bytes, *offset, *size)?;
                let strings = raw
                    .get(*link as usize)
                    .map(|s| section_bytes(bytes, s.4, s.5))
                    .transpose()?
                    .unwrap_or(&[][..]);
                let stride = if bits == 32 { 16 } else { 24 };
                for entry in table.chunks_exact(stride) {
                    let name_offset = u32::from_le_bytes(entry[0..4].try_into().unwrap()) as usize;
                    let value = if bits == 32 {
                        u32::from_le_bytes(entry[4..8].try_into().unwrap()) as u64
                    } else {
                        u64::from_le_bytes(entry[8..16].try_into().unwrap())
                    };
                    let name = symbol_name(strings, name_offset);
                    if !name.is_empty() {
                        symbols.push((name, value));
                    }
                }
            }
        }
        Ok(Elf {
            bits,
            little,
            sections,
            symbols,
        })
    }

    fn symbol(&self, name: &str) -> Option<u64> {
        self.symbols
            .iter()
            .find(|(symbol, _)| symbol == name)
            .map(|(_, address)| *address)
    }
}

fn section_bytes(bytes: &[u8], offset: u64, size: u64) -> Result<&[u8], String> {
    let start = offset as usize;
    let end = start
        .checked_add(size as usize)
        .ok_or_else(|| "section out of range".to_string())?;
    bytes
        .get(start..end)
        .ok_or_else(|| "section out of range".to_string())
}

fn symbol_name(strings: &[u8], offset: usize) -> String {
    let Some(rest) = strings.get(offset..) else {
        return String::new();
    };
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hand-built ELF32 little-endian image with `.data`, `.bss`, `.text`
    /// and a symbol table carrying `_heap_start`/`_heap_end`.
    fn sample_elf() -> Vec<u8> {
        // Layout: header (52), section table at 52 (7 * 40), names at 332,
        // symtab at 400 (3 * 16), strtab at 448.
        let mut data = vec![0u8; 512];
        data[0..4].copy_from_slice(b"\x7fELF");
        data[4] = 1; // 32-bit
        data[5] = 1; // little
        data[6] = 1;
        let put16 =
            |data: &mut [u8], at: usize, v: u16| data[at..at + 2].copy_from_slice(&v.to_le_bytes());
        let put32 =
            |data: &mut [u8], at: usize, v: u32| data[at..at + 4].copy_from_slice(&v.to_le_bytes());
        put32(&mut data, 32, 52); // e_shoff
        put16(&mut data, 46, 40); // e_shentsize
        put16(&mut data, 48, 7); // e_shnum
        put16(&mut data, 50, 4); // e_shstrndx

        let names = b"\0.text\0.data\0.bss\0.shstrtab\0.symtab\0.strtab\0";
        data[332..332 + names.len()].copy_from_slice(names);

        let mut section = |n: usize,
                           name: u32,
                           kind: u32,
                           flags: u32,
                           addr: u32,
                           off: u32,
                           size: u32,
                           link: u32| {
            let at = 52 + n * 40;
            put32(&mut data, at, name);
            put32(&mut data, at + 4, kind);
            put32(&mut data, at + 8, flags);
            put32(&mut data, at + 12, addr);
            put32(&mut data, at + 16, off);
            put32(&mut data, at + 20, size);
            put32(&mut data, at + 24, link);
        };
        // name offsets in `names`
        section(0, 0, 0, 0, 0, 0, 0, 0);
        section(1, 1, SHT_PROGBITS, 0x6, 0x4000_0000, 0, 100, 0); // .text
        section(2, 7, SHT_PROGBITS, 0x3, 0x3ffb_0000, 0, 800, 0); // .data
        section(3, 13, SHT_NOBITS, 0x3, 0x3ffb_0320, 0, 1600, 0); // .bss
        section(4, 18, 3, 0, 0, 332, names.len() as u32, 0); // .shstrtab
        section(5, 28, SHT_SYMTAB, 0, 0, 400, 48, 6); // .symtab
        section(6, 36, 3, 0, 0, 448, 32, 0); // .strtab

        let strings = b"\0_heap_start\0_heap_end\0";
        data[448..448 + strings.len()].copy_from_slice(strings);
        let mut symbol = |n: usize, name: u32, value: u32| {
            let at = 400 + n * 16;
            put32(&mut data, at, name);
            put32(&mut data, at + 4, value);
            put32(&mut data, at + 8, 0);
        };
        symbol(0, 0, 0);
        symbol(1, 1, 0x3ffb_0a00);
        symbol(2, 13, 0x3ffc_0000);
        data
    }

    #[test]
    fn a_report_sums_static_ram_and_finds_the_heap_region() {
        let dir = build_dir().join(format!("heap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("firmware.elf");
        std::fs::write(&path, sample_elf()).unwrap();

        let text = report(&path).unwrap();
        assert!(text.contains(".data"), "{text}");
        assert!(text.contains(".bss"), "{text}");
        assert!(!text.contains(".text"), "code is not RAM: {text}");
        assert!(text.contains("static RAM: 2400 bytes"), "{text}");
        assert!(text.contains("_heap_start = 0x3ffb0a00"), "{text}");
        assert!(text.contains("heap region: 62976 bytes"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_non_elf_file_is_refused() {
        let dir = build_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("heap-not-elf-{}.bin", std::process::id()));
        std::fs::write(&path, b"not an elf").unwrap();
        assert!(report(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    fn build_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build")
    }
}

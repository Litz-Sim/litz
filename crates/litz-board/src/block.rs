//! A sparse block image: the bytes a card or a flash chip holds.
//!
//! A microSD card in a diagram can be sized like the real one in the drawer,
//! so the image cannot be a `Vec` of the whole capacity. [`Image`] keeps only
//! the sectors that were written and answers the rest from a fill byte: a
//! 32 GiB card costs one B-tree entry per sector the firmware touches, and an
//! untouched card costs nothing.
//!
//! The fill byte is what distinguishes a blank card (0xFF, what erased flash
//! and an unwritten card return) from a formatted volume (0x00, what the FAT
//! format leaves outside its metadata).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// The sector size every device here addresses in.
pub const SECTOR: usize = 512;

/// A sparse byte image of a fixed capacity.
#[derive(Debug)]
pub struct Image {
    capacity: usize,
    fill: u8,
    sectors: BTreeMap<usize, Box<[u8; SECTOR]>>,
}

impl Image {
    /// An image of `capacity` bytes whose unwritten sectors read back as
    /// `fill`.
    pub fn blank(capacity: usize, fill: u8) -> Self {
        Self {
            capacity,
            fill,
            sectors: BTreeMap::new(),
        }
    }

    /// An image holding exactly `bytes`; the sectors it does not cover read
    /// back as zero, the same bytes a file shorter than the medium would hold.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        let capacity = bytes.len();
        let mut image = Self::blank(capacity, 0);
        for (index, chunk) in bytes.chunks(SECTOR).enumerate() {
            let mut sector = Box::new([0u8; SECTOR]);
            sector[..chunk.len()].copy_from_slice(chunk);
            image.sectors.insert(index, sector);
        }
        image
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn is_empty(&self) -> bool {
        self.capacity == 0
    }

    pub fn fill(&self) -> u8 {
        self.fill
    }

    /// The bytes in `offset..offset+len`, or `None` when the range is not
    /// wholly inside the image.
    pub fn read_at(&self, offset: usize, len: usize) -> Option<Vec<u8>> {
        let end = offset.checked_add(len)?;
        if end > self.capacity {
            return None;
        }
        let mut out = Vec::with_capacity(len);
        let mut at = offset;
        while out.len() < len {
            let index = at / SECTOR;
            let within = at % SECTOR;
            let take = (SECTOR - within).min(len - out.len());
            match self.sectors.get(&index) {
                Some(sector) => out.extend_from_slice(&sector[within..within + take]),
                None => out.extend(std::iter::repeat_n(self.fill, take)),
            }
            at += take;
        }
        Some(out)
    }

    /// Store `data` at `offset`. Returns `false` when the write would run
    /// past the end of the image.
    pub fn write_at(&mut self, offset: usize, data: &[u8]) -> bool {
        let Some(end) = offset.checked_add(data.len()) else {
            return false;
        };
        if end > self.capacity {
            return false;
        }
        let mut at = offset;
        let mut written = 0;
        while written < data.len() {
            let index = at / SECTOR;
            let within = at % SECTOR;
            let take = (SECTOR - within).min(data.len() - written);
            let sector = self
                .sectors
                .entry(index)
                .or_insert_with(|| Box::new([self.fill; SECTOR]));
            sector[within..within + take].copy_from_slice(&data[written..written + take]);
            // A sector that fell back to the fill byte is no information.
            if sector.iter().all(|&byte| byte == self.fill) {
                self.sectors.remove(&index);
            }
            at += take;
            written += take;
        }
        true
    }

    /// Every byte of the image, unwanted sectors included.
    pub fn to_vec(&self) -> Vec<u8> {
        self.read_at(0, self.capacity).unwrap_or_default()
    }

    /// Write the image to `path`.
    ///
    /// An image whose fill is zero is written as a sparse file: only the
    /// sectors that hold data are written and the file is extended to the
    /// capacity, which every filesystem that can hold a card image represents
    /// as a hole. An image with a non-zero fill (an erased card or flash)
    /// cannot be sparse without losing its fill, so it is written out whole.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        if self.fill == 0 {
            file.set_len(self.capacity as u64)?;
            for (&index, sector) in &self.sectors {
                let offset = index * SECTOR;
                if offset >= self.capacity {
                    continue;
                }
                let take = (self.capacity - offset).min(SECTOR);
                file.seek(SeekFrom::Start(offset as u64))?;
                file.write_all(&sector[..take])?;
            }
        } else {
            file.write_all(&self.to_vec())?;
        }
        Ok(())
    }
}

impl Default for Image {
    fn default() -> Self {
        Self::blank(0, 0)
    }
}

/// `Read`/`Write`/`Seek` over an [`Image`], the shape a filesystem builder
/// wants.
pub struct ImageIo<'a> {
    image: &'a mut Image,
    position: u64,
}

impl<'a> ImageIo<'a> {
    pub fn new(image: &'a mut Image) -> Self {
        Self { image, position: 0 }
    }

    pub fn image(&self) -> &Image {
        self.image
    }
}

impl Read for ImageIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let position = self.position as usize;
        if position >= self.image.capacity {
            return Ok(0);
        }
        let len = buf.len().min(self.image.capacity - position);
        let Some(bytes) = self.image.read_at(position, len) else {
            return Ok(0);
        };
        buf[..len].copy_from_slice(&bytes);
        self.position += len as u64;
        Ok(len)
    }
}

impl Write for ImageIo<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let position = self.position as usize;
        if position >= self.image.capacity {
            return Ok(0);
        }
        let len = buf.len().min(self.image.capacity - position);
        if !self.image.write_at(position, &buf[..len]) {
            return Ok(0);
        }
        self.position += len as u64;
        Ok(len)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for ImageIo<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::End(offset) => self.image.capacity as i128 + i128::from(offset),
            SeekFrom::Current(offset) => self.position as i128 + i128::from(offset),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the image",
            ));
        }
        self.position = target as u64;
        Ok(self.position)
    }
}

/// `64KB`, `4MB`, `8GB`, or a bare byte count.
///
/// Binary units, the way every capacity in the manifest reads.
pub fn parse_size(text: &str) -> Result<usize, String> {
    let text = text.trim();
    let upper = text.to_ascii_uppercase();
    let (number, scale) = if let Some(v) = upper.strip_suffix("GB") {
        (v, 1024 * 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix("G") {
        (v, 1024 * 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix("MB") {
        (v, 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix("M") {
        (v, 1024 * 1024)
    } else if let Some(v) = upper.strip_suffix("KB") {
        (v, 1024)
    } else if let Some(v) = upper.strip_suffix('K') {
        (v, 1024)
    } else {
        (upper.as_str(), 1)
    };
    let value: usize = number
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a size"))?;
    value
        .checked_mul(scale)
        .ok_or_else(|| format!("{text:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_the_way_a_card_is_sold() {
        assert_eq!(parse_size("64KB").unwrap(), 64 * 1024);
        assert_eq!(parse_size("8MB").unwrap(), 8 * 1024 * 1024);
        assert_eq!(parse_size("32GB").unwrap(), 32 * 1024 * 1024 * 1024);
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert!(parse_size("a lot").is_err());
    }

    #[test]
    fn unwritten_sectors_read_back_as_the_fill() {
        let image = Image::blank(4096, 0xff);
        assert_eq!(image.read_at(0, 8).unwrap(), [0xff; 8]);
        assert!(image.read_at(4090, 8).is_none(), "past the end");
        assert!(
            Image::blank(4096, 0)
                .read_at(0, 4096)
                .unwrap()
                .iter()
                .all(|&b| b == 0)
        );
    }

    #[test]
    fn a_write_spans_sectors_and_reads_back() {
        let mut image = Image::blank(4096, 0);
        assert!(image.write_at(500, &[1, 2, 3, 4]));
        assert_eq!(image.read_at(500, 4).unwrap(), [1, 2, 3, 4]);
        assert_eq!(image.read_at(496, 8).unwrap(), [0, 0, 0, 0, 1, 2, 3, 4]);
        assert!(
            !image.write_at(4094, &[1, 2, 3, 4]),
            "a write may not run past the end"
        );
    }

    #[test]
    fn writing_the_fill_back_drops_the_sector() {
        let mut image = Image::blank(4096, 0xff);
        image.write_at(0, &[0x00; 512]);
        assert!(!image.sectors.is_empty());
        image.write_at(0, &[0xff; 512]);
        assert!(image.sectors.is_empty(), "a sector of fill carries nothing");
    }

    #[test]
    fn seek_end_is_the_capacity() {
        let mut image = Image::blank(2048, 0);
        let mut io = ImageIo::new(&mut image);
        assert_eq!(io.seek(SeekFrom::End(0)).unwrap(), 2048);
        assert_eq!(io.seek(SeekFrom::Start(512)).unwrap(), 512);
        assert_eq!(io.stream_position().unwrap(), 512);
        assert!(io.seek(SeekFrom::Start(0)).is_ok());
    }

    #[test]
    fn a_sparse_save_keeps_the_data() {
        let mut image = Image::blank(1 << 20, 0);
        image.write_at(700_000, b"hello");
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("block-sparse-{}.img", std::process::id()));
        image.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 1 << 20);
        assert_eq!(&bytes[700_000..700_005], b"hello");
        assert!(bytes[..700_000].iter().all(|&b| b == 0));
        let _ = std::fs::remove_file(&path);
    }
}

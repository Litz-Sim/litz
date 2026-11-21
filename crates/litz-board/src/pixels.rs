//! Whole-part screenshots and pixel-exact comparison.
//!
//! A [`Framebuffer`] is the board-level picture of one display: a size, a
//! colour format and the packed pixels. It is deliberately independent of the
//! device models, so a caller can encode a PNG or diff a run against a golden
//! image without knowing whether the pixels came from an SPI TFT or an I2C
//! OLED. [`Board::part_framebuffer`](crate::Board::part_framebuffer) is the
//! bridge from a diagram id to that value.
//!
//! The PNG writer is small on purpose: this crate has no image dependency and
//! the files a simulated panel needs (a few hundred kilobytes at most) are
//! fine with zlib's stored, uncompressed deflate blocks. A real decoder reads
//! them exactly like a compressed stream; it just spends more disk than it
//! would on a photo.

/// How the channels of one [`Framebuffer`] pixel are packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Three bytes per pixel, red, green, blue. PNG colour type 2.
    Rgb8,
    /// Four bytes per pixel, red, green, blue, alpha. PNG colour type 6.
    Rgba8,
}

impl PixelFormat {
    /// Bytes one pixel occupies in the packed buffer.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb8 => 3,
            Self::Rgba8 => 4,
        }
    }

    /// The PNG colour type number for this format.
    fn png_color_type(self) -> u8 {
        match self {
            Self::Rgb8 => 2,
            Self::Rgba8 => 6,
        }
    }
}

/// A display's current pixels, ready to encode or compare.
///
/// The buffer is always exactly `width * height * bytes_per_pixel` bytes: a
/// constructor pads a short input with zeroes and truncates a long one, because
/// a PNG scanline cannot be ragged and a comparison needs a pixel for every
/// coordinate. Accessors and comparisons work in RGBA, using an opaque alpha
/// for [`PixelFormat::Rgb8`], so mixed formats compare channel by channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl Framebuffer {
    /// A framebuffer in `format`. Pixels are row-major, top row first.
    pub fn new(width: u32, height: u32, format: PixelFormat, mut pixels: Vec<u8>) -> Self {
        let expected = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(format.bytes_per_pixel());
        // A short or long buffer would make `to_png` emit a malformed stream or
        // leave half a pixel dangling, so normalise it here once.
        pixels.resize(expected, 0);
        Self {
            width,
            height,
            format,
            pixels,
        }
    }

    /// A row-major RGB8 image.
    pub fn rgb8(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self::new(width, height, PixelFormat::Rgb8, pixels)
    }

    /// A row-major RGBA8 image.
    pub fn rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self::new(width, height, PixelFormat::Rgba8, pixels)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    /// The packed bytes exactly as the format lays them out.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// One pixel normalised to RGBA, or `None` outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let bytes = self.format.bytes_per_pixel();
        let index = (y as usize * self.width as usize + x as usize) * bytes;
        let pixel = self.pixels.get(index..index + bytes)?;
        Some(match self.format {
            PixelFormat::Rgb8 => [pixel[0], pixel[1], pixel[2], 255],
            PixelFormat::Rgba8 => [pixel[0], pixel[1], pixel[2], pixel[3]],
        })
    }

    /// Convert an RGB565 panel buffer (ILI9341 and similar) to RGB8.
    ///
    /// The scaling matches the ESP32 backend's PNG capture: a 5- or 6-bit
    /// channel is stretched across the full 0..=255 range rather than shifted,
    /// so the brightest code is exactly white.
    pub fn from_rgb565(pixels: &[u16], width: u32, height: u32) -> Self {
        let count = width as usize * height as usize;
        let mut rgb = Vec::with_capacity(count * 3);
        for &color in pixels.iter().take(count) {
            let red = (color >> 11) & 0x1f;
            let green = (color >> 5) & 0x3f;
            let blue = color & 0x1f;
            rgb.push((u32::from(red) * 255 / 31) as u8);
            rgb.push((u32::from(green) * 255 / 63) as u8);
            rgb.push((u32::from(blue) * 255 / 31) as u8);
        }
        Self::rgb8(width, height, rgb)
    }

    /// Convert an SSD1306 GDDRAM page-major buffer to RGB8.
    ///
    /// In that layout `page * width + column` selects a byte, bit 0 is the top
    /// row of the page and bit 7 the bottom, so the rows run down each byte.
    /// A set bit is a lit (white) pixel; monochrome panels have no colour.
    pub fn from_page_bits_lsb_first(bytes: &[u8], width: u32, height: u32) -> Self {
        Self::monochrome(width, height, |x, y| {
            let index = (y / 8) * width as usize + x;
            bytes
                .get(index)
                .is_some_and(|byte| byte & (1 << (y % 8)) != 0)
        })
    }

    /// Convert a row-major 1bpp buffer whose most significant bit is leftmost.
    ///
    /// E-paper controllers use this layout: `y * ceil(width / 8) + x / 8`, with
    /// bit 7 of the byte lighting the leftmost pixel of the eight.
    pub fn from_row_bits_msb_first(bytes: &[u8], width: u32, height: u32) -> Self {
        let stride = (width as usize).div_ceil(8);
        Self::monochrome(width, height, |x, y| {
            bytes
                .get(y * stride + x / 8)
                .is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0)
        })
    }

    fn monochrome(width: u32, height: u32, lit: impl Fn(usize, usize) -> bool) -> Self {
        let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let value = if lit(x, y) { 255 } else { 0 };
                rgb.extend_from_slice(&[value, value, value]);
            }
        }
        Self::rgb8(width, height, rgb)
    }

    /// Encode the image as an 8-bit PNG.
    ///
    /// A zero-sized framebuffer cannot exist in PNG, so it is emitted as a
    /// single transparent black pixel instead of a file a decoder rejects.
    pub fn to_png(&self) -> Vec<u8> {
        let width = self.width.max(1) as usize;
        let height = self.height.max(1) as usize;
        let bytes = self.format.bytes_per_pixel();
        let mut raw = Vec::with_capacity((width * bytes + 1) * height);
        for y in 0..height {
            // Filter type 0: the writer trades compression for code size.
            raw.push(0);
            for x in 0..width {
                let pixel = self.pixel(x as u32, y as u32).unwrap_or([
                    0,
                    0,
                    0,
                    if bytes == 3 { 255 } else { 0 },
                ]);
                raw.extend_from_slice(&pixel[..bytes]);
            }
        }

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&(width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, self.format.png_color_type(), 0, 0, 0]);

        let mut png = PNG_SIGNATURE.to_vec();
        chunk(&mut png, b"IHDR", &ihdr);
        chunk(&mut png, b"IDAT", &zlib_stored(&raw));
        chunk(&mut png, b"IEND", &[]);
        png
    }
}

/// The first difference between two framebuffers.
///
/// When the sizes differ, `x` and `y` name the first coordinate that exists in
/// only one image: the smaller width at row zero, or column zero at the smaller
/// height. `mismatched` then counts every pixel one image has and the other
/// does not. With equal sizes it is the number of pixels outside the per-channel
/// tolerance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Diff {
    pub x: u32,
    pub y: u32,
    /// The actual pixel at the first difference, as RGBA.
    pub actual: [u8; 4],
    /// The expected pixel at the same coordinate, as RGBA.
    pub expected: [u8; 4],
    pub mismatched: usize,
}

/// Compare two images pixel by pixel.
///
/// Sizes are checked first, so a missing row or column is reported as such
/// instead of hiding behind whatever happens to share the overlap. With equal
/// sizes, every channel of a pixel must be within `tolerance` of the expected
/// value; `0` is an exact comparison.
pub fn compare_framebuffers(
    actual: &Framebuffer,
    expected: &Framebuffer,
    tolerance: u8,
) -> Result<(), Diff> {
    if (actual.width, actual.height) != (expected.width, expected.height) {
        let overlap_width = actual.width.min(expected.width);
        let overlap_height = actual.height.min(expected.height);
        let (x, y) = if actual.width != expected.width {
            (overlap_width, 0)
        } else {
            (0, overlap_height)
        };
        let overlap = overlap_width as usize * overlap_height as usize;
        let extra = actual.width as usize * actual.height as usize
            + expected.width as usize * expected.height as usize
            - 2 * overlap;
        return Err(Diff {
            x,
            y,
            actual: actual.pixel(x, y).unwrap_or([0; 4]),
            expected: expected.pixel(x, y).unwrap_or([0; 4]),
            mismatched: extra,
        });
    }

    let mut mismatched = 0;
    let mut first = None;
    for y in 0..actual.height {
        for x in 0..actual.width {
            let a = actual.pixel(x, y).unwrap_or([0; 4]);
            let e = expected.pixel(x, y).unwrap_or([0; 4]);
            let differs = (0..4).any(|channel| a[channel].abs_diff(e[channel]) > tolerance);
            if differs {
                mismatched += 1;
                if first.is_none() {
                    first = Some((x, y, a, e));
                }
            }
        }
    }
    match first {
        None => Ok(()),
        Some((x, y, actual, expected)) => Err(Diff {
            x,
            y,
            actual,
            expected,
            mismatched,
        }),
    }
}

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// One PNG chunk: length, type, payload and the CRC over type plus payload.
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// A zlib stream made only of stored deflate blocks.
///
/// No compressor means the output is larger than the raw image, but it removes
/// both a dependency and the state a real deflate encoder would need. PNG
/// decoders do not care which deflate block types produced the bytes.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    // CMF/FLG: deflate with a 32 KiB window, no preset dictionary, fastest.
    let mut out = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        // Even an empty stream needs one final block, or it is unterminated.
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        while let Some(block) = blocks.next() {
            let last = blocks.peek().is_none();
            // BFINAL in bit 0, BTYPE 00 for stored, then LEN and its complement.
            out.push(u8::from(last));
            out.extend_from_slice(&(block.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            out.extend_from_slice(block);
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                0xedb8_8320 ^ (crc >> 1)
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(png: &[u8]) -> (u32, u32, u8, u8) {
        assert_eq!(&png[..8], &PNG_SIGNATURE, "the PNG signature");
        assert_eq!(&png[12..16], b"IHDR");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        (width, height, png[24], png[25])
    }

    #[test]
    fn a_png_carries_the_framebuffer_dimensions() {
        let png = Framebuffer::rgb8(7, 3, vec![0; 7 * 3 * 3]).to_png();
        assert_eq!(header(&png), (7, 3, 8, 2));
        let rgba = Framebuffer::rgba8(2, 5, vec![0; 2 * 5 * 4]).to_png();
        assert_eq!(header(&rgba), (2, 5, 8, 6));

        // Every chunk is length, type, payload, CRC; the file ends at IEND.
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn an_ili9341_rgb565_colour_becomes_rgb8() {
        let panel = [0xf800u16, 0x07e0, 0x001f, 0xffff];
        let image = Framebuffer::from_rgb565(&panel, 2, 2);
        assert_eq!(image.pixel(0, 0), Some([255, 0, 0, 255]), "red");
        assert_eq!(image.pixel(1, 0), Some([0, 255, 0, 255]), "green");
        assert_eq!(image.pixel(0, 1), Some([0, 0, 255, 255]), "blue");
        assert_eq!(image.pixel(1, 1), Some([255, 255, 255, 255]), "white");
    }

    #[test]
    fn an_ssd1306_page_byte_runs_from_bit_zero_downwards() {
        // Page 0 columns 0 and 1: 0xaa is bits 1,3,5,7; 0x0f is bits 0..3.
        let image = Framebuffer::from_page_bits_lsb_first(&[0xaa, 0x0f], 2, 8);
        assert_eq!(image.pixel(0, 0), Some([0, 0, 0, 255]), "bit 0 clear");
        assert_eq!(image.pixel(0, 1), Some([255, 255, 255, 255]), "bit 1 set");
        assert_eq!(image.pixel(0, 7), Some([255, 255, 255, 255]), "bit 7");
        assert_eq!(image.pixel(1, 0), Some([255, 255, 255, 255]), "bit 0");
        assert_eq!(image.pixel(1, 4), Some([0, 0, 0, 255]), "bit 4 clear");
    }

    #[test]
    fn an_epaper_row_lights_the_msb_first() {
        let image = Framebuffer::from_row_bits_msb_first(&[0x80], 8, 1);
        assert_eq!(image.pixel(0, 0), Some([255, 255, 255, 255]));
        assert_eq!(image.pixel(1, 0), Some([0, 0, 0, 255]));
    }

    #[test]
    fn the_first_mismatch_is_the_first_changed_pixel() {
        let actual = Framebuffer::rgb8(3, 2, vec![0; 3 * 2 * 3]);
        let mut pixels = vec![0; 3 * 2 * 3];
        pixels[(3 + 2) * 3] = 10; // pixel (2, 1), red channel
        let expected = Framebuffer::rgb8(3, 2, pixels);

        let diff = compare_framebuffers(&actual, &expected, 0).expect_err("they differ");
        assert_eq!((diff.x, diff.y), (2, 1));
        assert_eq!(diff.actual, [0, 0, 0, 255]);
        assert_eq!(diff.expected, [10, 0, 0, 255]);
        assert_eq!(diff.mismatched, 1);
    }

    #[test]
    fn tolerance_is_the_largest_accepted_channel_difference() {
        let actual = Framebuffer::rgb8(1, 1, vec![10, 20, 30]);
        let expected = Framebuffer::rgb8(1, 1, vec![15, 20, 30]);
        assert!(compare_framebuffers(&actual, &expected, 5).is_ok());
        let diff = compare_framebuffers(&actual, &expected, 4).expect_err("just outside");
        assert_eq!(diff.actual, [10, 20, 30, 255]);
        assert_eq!(diff.expected, [15, 20, 30, 255]);
    }

    #[test]
    fn a_size_mismatch_is_reported_before_the_overlap() {
        let actual = Framebuffer::rgb8(2, 2, vec![0; 2 * 2 * 3]);
        let expected = Framebuffer::rgb8(3, 2, vec![0; 3 * 2 * 3]);
        let diff = compare_framebuffers(&actual, &expected, 0).expect_err("widths differ");
        assert_eq!((diff.x, diff.y), (2, 0), "the first missing column");
        assert_eq!(diff.mismatched, 2, "one missing pixel per row");
    }
}

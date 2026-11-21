//! A tiny RGBA8888 framebuffer, testable on the host.
//!
//! The 8x8 panel exists to make the display half of the part contract real:
//! `framebuffer()` advertises the geometry, `draw()` copies bytes at a byte
//! offset, and `damage()` reports the bounding box of what changed since the
//! last call. The pixel format is four bytes per pixel, red/green/blue/alpha.

/// Pixels per side.
pub const SIDE: u32 = 8;
/// Bytes per pixel.
pub const BPP: usize = 4;

pub struct Panel {
    pixels: Vec<u8>,
    damage: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            pixels: vec![0; (SIDE * SIDE) as usize * BPP],
            damage: None,
        }
    }

    pub fn width(&self) -> u32 {
        SIDE
    }

    pub fn height(&self) -> u32 {
        SIDE
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Copy `bytes` into the framebuffer at `offset`, clamped to the end, and
    /// fold the touched pixels into the pending damage rectangle.
    pub fn draw(&mut self, offset: u32, bytes: &[u8]) {
        let start = (offset as usize).min(self.pixels.len());
        let end = start.saturating_add(bytes.len()).min(self.pixels.len());
        if start == end {
            return;
        }
        self.pixels[start..end].copy_from_slice(&bytes[..end - start]);

        let first = start / BPP;
        let last = (end - 1) / BPP;
        let x0 = first as u32 % SIDE;
        let x1 = last as u32 % SIDE;
        let y0 = first as u32 / SIDE;
        let y1 = last as u32 / SIDE;
        // A run that crosses a row boundary spans the full width of the rows
        // in between; the two ends are not enough.
        let rect = if y0 == y1 {
            Rect {
                x: x0,
                y: y0,
                width: x1 - x0 + 1,
                height: 1,
            }
        } else {
            Rect {
                x: 0,
                y: y0,
                width: SIDE,
                height: y1 - y0 + 1,
            }
        };
        self.damage = Some(match self.damage {
            None => rect,
            Some(current) => union(current, rect),
        });
    }

    /// The pending damage rectangle, cleared by reading it.
    pub fn take_damage(&mut self) -> Option<Rect> {
        self.damage.take()
    }
}

impl Default for Panel {
    fn default() -> Self {
        Self::new()
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pixel_upload_marks_one_pixel_damaged() {
        let mut panel = Panel::new();
        panel.draw(0, &[0xff, 0, 0, 0xff]);
        assert_eq!(
            panel.take_damage(),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            })
        );
        assert_eq!(&panel.pixels()[..4], &[0xff, 0, 0, 0xff]);
        assert_eq!(panel.take_damage(), None, "damage is consumed");
    }

    #[test]
    fn a_row_crossing_upload_spans_the_width() {
        let mut panel = Panel::new();
        // The last pixel of row 0 and the first of row 1.
        panel.draw(7 * 4, &[0; 8]);
        assert_eq!(
            panel.take_damage(),
            Some(Rect {
                x: 0,
                y: 0,
                width: SIDE,
                height: 2
            })
        );
    }

    #[test]
    fn damage_unions_successive_uploads() {
        let mut panel = Panel::new();
        panel.draw(0, &[1, 2, 3, 4]);
        panel.draw(8 * 4 * 3, &[5, 6, 7, 8]);
        assert_eq!(
            panel.take_damage(),
            Some(Rect {
                x: 0,
                y: 0,
                width: 1,
                height: 4
            })
        );
    }
}

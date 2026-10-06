//! Framebuffer console sink.
//!
//! Renders text into a linear framebuffer provided by the
//! bootloader (Multiboot2 framebuffer tag, type 8). Each character
//! is drawn using the bitmap font parsed by
//! [`crate::console::font`].
//!
//! # Pixel format
//!
//! The framebuffer is a contiguous array of pixels, with each row
//! separated by a `pitch` of bytes. The colour layout depends on
//! the Multiboot2 tag. This sink supports RGB modes with 16, 24,
//! or 32 bits per pixel. Indexed modes are not supported.

use core::fmt;

use super::font::Font;

/// Maximum number of bytes in a single glyph's bitmap.
///
/// 8x16 fonts use 16 bytes, one per row. Fonts up to 8x32 use 32
/// bytes. The buffer in `draw_glyph` is sized to this maximum so
/// that any font the sink accepts can be copied without bounds
/// checks.
const MAX_GLYPH_BYTES: usize = 32;

/// A framebuffer console.
pub struct Framebuffer {
    /// Base virtual address of the framebuffer.
    base: *mut u8,

    /// Bytes per row of pixels.
    pitch: usize,

    /// Width in pixels.
    width: usize,

    /// Height in pixels.
    height: usize,

    /// Bytes per pixel (2, 3, or 4).
    bytes_per_pixel: usize,

    /// Foreground colour packed into the framebuffer's layout.
    fg: u32,

    /// Background colour packed into the framebuffer's layout.
    bg: u32,

    /// The loaded font.
    font: Font,

    /// Current cursor column, in character cells.
    column: usize,

    /// Current cursor row, in character cells.
    row: usize,
}

/// The framebuffer pointer is a fixed memory-mapped region. Any
/// thread can safely write to it, and the pointer never changes.
unsafe impl Send for Framebuffer {}
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    /// Creates a framebuffer sink.
    ///
    /// Returns `None` if the font cannot be loaded, the font is
    /// wider than 8 pixels, or the framebuffer dimensions are too
    /// small for at least one cell.
    ///
    /// # Safety
    ///
    /// `base` must point at a valid framebuffer of at least
    /// `pitch * height` bytes, and the framebuffer must be mapped
    /// as writable memory. This is the contract the Multiboot2
    /// tag provides.
    pub unsafe fn new(
        base: *mut u8,
        pitch: usize,
        width: usize,
        height: usize,
        bytes_per_pixel: usize,
        red_shift: u8,
        red_size: u8,
        green_shift: u8,
        green_size: u8,
        blue_shift: u8,
        blue_size: u8,
    ) -> Option<Self> {
        if bytes_per_pixel != 2 && bytes_per_pixel != 3 && bytes_per_pixel != 4 {
            return None;
        }

        let font = Font::load()?;

        // This sink draws one bit per pixel and assumes at most
        // 8 pixels per glyph row. Fonts with wider glyphs are not
        // supported.
        if font.width() > 8 {
            return None;
        }

        if font.height() > MAX_GLYPH_BYTES {
            return None;
        }

        if width < font.width() || height < font.height() {
            return None;
        }

        // Foreground: white; background: black.
        let fg = pack_color(
            0xFF,
            0xFF,
            0xFF,
            red_shift,
            red_size,
            green_shift,
            green_size,
            blue_shift,
            blue_size,
        );
        let bg = pack_color(
            0x00,
            0x00,
            0x00,
            red_shift,
            red_size,
            green_shift,
            green_size,
            blue_shift,
            blue_size,
        );

        Some(Self {
            base,
            pitch,
            width,
            height,
            bytes_per_pixel,
            fg,
            bg,
            font,
            column: 0,
            row: 0,
        })
    }

    /// Number of character columns.
    fn columns(&self) -> usize {
        self.width / self.font.width()
    }

    /// Number of character rows.
    fn rows(&self) -> usize {
        self.height / self.font.height()
    }

    /// Writes one pixel at (x, y).
    fn put_pixel(&mut self, x: usize, y: usize, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }

        let offset = y * self.pitch + x * self.bytes_per_pixel;

        unsafe {
            let ptr = self.base.add(offset);

            match self.bytes_per_pixel {
                2 => core::ptr::write_volatile(ptr as *mut u16, color as u16),
                3 => {
                    core::ptr::write_volatile(ptr, (color & 0xFF) as u8);
                    core::ptr::write_volatile(ptr.add(1), ((color >> 8) & 0xFF) as u8);
                    core::ptr::write_volatile(ptr.add(2), ((color >> 16) & 0xFF) as u8);
                }
                4 => core::ptr::write_volatile(ptr as *mut u32, color),
                _ => unreachable!(),
            }
        }
    }

    /// Draws one glyph at the current cursor position.
    ///
    /// The glyph's bitmap is copied into a local buffer before any
    /// pixels are drawn. This releases the immutable borrow of the
    /// font so that `put_pixel` (which takes `&mut self`) can be
    /// called in the drawing loop.
    fn draw_glyph(&mut self, byte: u8) {
        let glyph_width = self.font.width();
        let glyph_height = self.font.height();

        let base_x = self.column * glyph_width;
        let base_y = self.row * glyph_height;

        // Copy the glyph's bitmap into a local buffer.
        let mut bitmap = [0u8; MAX_GLYPH_BYTES];
        let copy_len = {
            let glyph = self.font.glyph(byte);
            let n = glyph.len().min(MAX_GLYPH_BYTES);
            bitmap[..n].copy_from_slice(&glyph[..n]);
            n
        };

        // For each row of the glyph.
        for row in 0..glyph_height {
            // Rows beyond the copied data are blank. This can
            // happen if the font is shorter than the framebuffer
            // sink expects, which should not occur in practice.
            let bits = if row < copy_len { bitmap[row] } else { 0 };

            for col in 0..glyph_width {
                // Bit 7 is the leftmost pixel. For fonts narrower
                // than 8 pixels, the unused high bits are shifted
                // out by using the same bit positions.
                let bit = (bits >> (7 - col)) & 1;
                let color = if bit == 1 { self.fg } else { self.bg };
                self.put_pixel(base_x + col, base_y + row, color);
            }
        }
    }

    /// Advances to the next row, scrolling if necessary.
    fn new_line(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows() {
            self.row += 1;
        } else {
            self.scroll();
        }
    }

    /// Scrolls the framebuffer up by one character row.
    fn scroll(&mut self) {
        let glyph_height = self.font.height();
        let bytes_per_row = self.pitch * glyph_height;
        let total = self.pitch * self.rows() * glyph_height;

        unsafe {
            // Move every glyph row up by `bytes_per_row` bytes.
            let src = self.base.add(bytes_per_row);
            let dst = self.base;
            core::ptr::copy(src, dst, total - bytes_per_row);

            // Blank the last glyph row.
            core::ptr::write_bytes(
                self.base.add(total - bytes_per_row),
                0,
                bytes_per_row,
            );
        }

        self.row = self.rows() - 1;
    }

    /// Writes one byte to the framebuffer.
    fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            b'\r' => self.column = 0,
            b'\t' => {
                // Advance to the next multiple of 4 columns.
                self.column = (self.column + 4) & !3;
                if self.column >= self.columns() {
                    self.new_line();
                }
            }
            byte => {
                self.draw_glyph(byte);
                self.column += 1;
                if self.column >= self.columns() {
                    self.new_line();
                }
            }
        }
    }
}

impl fmt::Write for Framebuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

/// Packs an RGB triplet into the framebuffer's pixel layout.
fn pack_color(
    r: u8,
    g: u8,
    b: u8,
    red_shift: u8,
    red_size: u8,
    green_shift: u8,
    green_size: u8,
    blue_shift: u8,
    blue_size: u8,
) -> u32 {
    let r = scale_channel(r, red_size) << red_shift;
    let g = scale_channel(g, green_size) << green_shift;
    let b = scale_channel(b, blue_size) << blue_shift;
    r | g | b
}

/// Scales an 8-bit channel value to a smaller bit width.
fn scale_channel(value: u8, size: u8) -> u32 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return value as u32;
    }
    (value as u32) >> (8 - size)
}
//! Font parser for the framebuffer console.
//!
//! Supports both PSF1 and PSF2 formats. PSF1 is the older format
//! and is what Debian's console fonts ship as. PSF2 is the newer
//! format and has an explicit header.
//!
//! # PSF1 header
//!
//! ```text
//!   offset  size  field
//!     0       2   magic      (0x36 0x04)
//!     2       1   mode       (bit 0: 512 glyphs if set; bit 1: unicode table)
//!     3       1   charsize   (bytes per glyph)
//!     4      ...  glyphs     (256 or 512 glyphs, each charsize bytes)
//! ```
//!
//! PSF1 does not store the glyph width or height. For console
//! fonts, the glyph is always 8 pixels wide. The height is
//! `charsize`, since each row uses one byte.
//!
//! # PSF2 header
//!
//! ```text
//!   offset  size  field
//!     0       4   magic      (0x72 0xB5 0x4A 0x86)
//!     4       4   version    (0)
//!     8       4   header size
//!    12       4   flags
//!    16       4   glyph count
//!    20       4   bytes per glyph
//!    24       4   glyph height
//!    28       4   glyph width
//! ```

/// The embedded font file.
static FONT_DATA: &[u8] = include_bytes!("default.psf");

/// A parsed font. Handles both PSF1 and PSF2.
pub struct Font {
    /// Offset in `FONT_DATA` where the first glyph begins.
    glyphs_start: usize,

    /// Number of glyphs in the font.
    glyph_count: usize,

    /// Bytes per glyph.
    glyph_size: usize,

    /// Glyph width in pixels.
    width: usize,

    /// Glyph height in pixels.
    height: usize,
}

impl Font {
    /// Parses the embedded font.
    ///
    /// Returns `None` if the font is neither PSF1 nor PSF2, or if
    /// the header is malformed.
    pub fn load() -> Option<Self> {
        let data = FONT_DATA;

        if data.len() < 4 {
            return None;
        }

        // PSF1: magic 0x36 0x04.
        if data[0] == 0x36 && data[1] == 0x04 {
            return Self::parse_psf1(data);
        }

        // PSF2: magic 0x72 0xB5 0x4A 0x86.
        if data.len() >= 32
            && data[0] == 0x72
            && data[1] == 0xB5
            && data[2] == 0x4A
            && data[3] == 0x86
        {
            return Self::parse_psf2(data);
        }

        None
    }

    /// Parses a PSF1 font.
    fn parse_psf1(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }

        let mode = data[2];
        let charsize = data[3] as usize;

        if charsize == 0 {
            return None;
        }

        // Bit 0 of mode selects 256 or 512 glyphs.
        let glyph_count = if mode & 0x01 != 0 { 512 } else { 256 };

        // Bit 1 of mode indicates a trailing Unicode table; we
        // ignore it.
        let glyphs_start = 4;

        let glyphs_end = glyphs_start + glyph_count * charsize;
        if glyphs_end > data.len() {
            return None;
        }

        // PSF1 console fonts are always 8 pixels wide. Height is
        // charsize, since each row is one byte.
        Some(Self {
            glyphs_start,
            glyph_count,
            glyph_size: charsize,
            width: 8,
            height: charsize,
        })
    }

    /// Parses a PSF2 font.
    fn parse_psf2(data: &[u8]) -> Option<Self> {
        let header_size = read_u32(data, 8) as usize;
        let glyph_count = read_u32(data, 16) as usize;
        let glyph_size = read_u32(data, 20) as usize;
        let height = read_u32(data, 24) as usize;
        let width = read_u32(data, 28) as usize;

        if header_size < 32 || header_size > data.len() {
            return None;
        }
        if glyph_count == 0 || glyph_count > 65536 {
            return None;
        }
        if glyph_size == 0 {
            return None;
        }
        if header_size + glyph_count * glyph_size > data.len() {
            return None;
        }

        Some(Self {
            glyphs_start: header_size,
            glyph_count,
            glyph_size,
            width,
            height,
        })
    }

    /// Returns the pixel width of one glyph.
    pub const fn width(&self) -> usize {
        self.width
    }

    /// Returns the pixel height of one glyph.
    pub const fn height(&self) -> usize {
        self.height
    }

    /// Returns the bitmap for a byte value.
    ///
    /// The returned slice is `glyph_size` bytes long. Each byte is
    /// one row of the glyph, with bit 7 the leftmost pixel.
    ///
    /// If the byte value is beyond the font's glyph count, an empty
    /// slice is returned.
    pub fn glyph(&self, byte: u8) -> &[u8] {
        let index = byte as usize;

        if index >= self.glyph_count {
            return &[];
        }

        let offset = self.glyphs_start + index * self.glyph_size;
        &FONT_DATA[offset..offset + self.glyph_size]
    }
}

/// Reads a little-endian `u32` from a byte slice.
fn read_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        data[offset],
        data[offset + 1],
        data[offset + 2],
        data[offset + 3],
    ])
}
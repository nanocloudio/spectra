//! BCM2712 HEVC output surface — column-tiled NV12 → linear (nv12 / p010).
//!
//! The decode block writes pictures (and reads references) in a column-tiled
//! layout the spec calls "NV12 col128" [r01 §8]: the image is cut into vertical
//! columns **128 bytes** wide, and within a column the rows are stored
//! contiguously top-to-bottom, so one whole column is `128 × padded_height`
//! bytes and the next column follows it. A display or encoder wants linear,
//! row-major planes, so this module detiles.
//!
//! Two sample packings share the one column geometry [r01 §8]:
//! - **8-bit (NC12):** one byte per luma sample, so a 128-byte column is 128
//!   pixels wide. Detiling is a pure byte rearrangement → NV12.
//! - **10-bit (NC30):** three samples packed into the low 30 bits of each
//!   32-bit word, so a 128-byte column (32 words) is 96 pixels wide. Detiling
//!   unpacks to 16-bit samples → P010 (each sample in the high 10 bits of a
//!   little-endian u16).
//!
//! ## Provenance
//!
//! **Clean-room Team B.** The tiling geometry derives solely from the released
//! spec `.context/clean_room/spec/hevc_block_r01_released.md` §8 (confirmed
//! unchanged by r02 §C — "output-format geometry from r01 is unchanged"). The
//! P010 sample packing is the public Microsoft/industry format, not from the
//! driver. No GPL source was read.
//!
//! Like the rest of this crate the module is `no_std` and **allocation-free**:
//! detiling writes into a caller-provided output slice rather than returning a
//! `Vec`, so the caller — which owns the arena — decides where the linear
//! plane lands. Each detile returns the number of samples written.

/// Bytes per tiled column [r01 §8]. Always 128, for both sample depths — it is
/// a *byte* width, which is why 8-bit columns are 128 px and 10-bit are 96 px.
pub const COLUMN_BYTES: u32 = 128;

/// Vertical alignment of the tiled surface [r01 §8].
const HEIGHT_ALIGN: u32 = 8;

#[inline]
const fn align_up(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

/// Geometry of one tiled surface: the padded dimensions, column count, and the
/// per-column byte sizes that also feed the phase-2 stride registers
/// (`P2_OUT_Y_STRIDE` / `P2_OUT_C_STRIDE`, both ÷64) [r01 §8, §10].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Coded luma width/height in pixels (pre-crop; the block decodes the full
    /// coded picture).
    pub width: u32,
    pub height: u32,
    /// 8 or 10.
    pub bit_depth: u8,
}

impl Surface {
    #[must_use]
    pub const fn new(width: u32, height: u32, bit_depth: u8) -> Self {
        Self {
            width,
            height,
            bit_depth,
        }
    }

    /// Luma pixels per 128-byte column: 128 at 8-bit, 96 at 10-bit [r01 §8].
    #[must_use]
    pub const fn pixels_per_column(self) -> u32 {
        if self.bit_depth >= 10 {
            96
        } else {
            128
        }
    }

    /// Width padded up to a whole number of columns [r01 §8].
    #[must_use]
    pub fn padded_width(self) -> u32 {
        align_up(self.width, self.pixels_per_column())
    }

    /// Height padded to a multiple of 8 [r01 §8].
    #[must_use]
    pub fn padded_height(self) -> u32 {
        align_up(self.height, HEIGHT_ALIGN)
    }

    /// Number of tiled columns across the picture.
    #[must_use]
    pub fn num_columns(self) -> u32 {
        self.padded_width() / self.pixels_per_column()
    }

    /// Bytes in one whole luma column = `128 × padded_height` [r01 §8].
    #[must_use]
    pub fn luma_column_bytes(self) -> u32 {
        COLUMN_BYTES * self.padded_height()
    }

    /// Total luma plane size in bytes.
    #[must_use]
    pub fn luma_plane_bytes(self) -> u32 {
        self.num_columns() * self.luma_column_bytes()
    }

    /// Chroma column bytes — the chroma plane has half the rows, so half the
    /// luma column size [r01 §8].
    #[must_use]
    pub fn chroma_column_bytes(self) -> u32 {
        COLUMN_BYTES * (self.padded_height() / 2)
    }

    /// Total (interleaved CbCr) chroma plane size in bytes.
    #[must_use]
    pub fn chroma_plane_bytes(self) -> u32 {
        self.num_columns() * self.chroma_column_bytes()
    }

    /// `P2_OUT_Y_STRIDE` register value: the whole-column byte size ÷64
    /// [r01 §8, §10]. Equals `2 × padded_height`.
    #[must_use]
    pub fn luma_stride_reg(self) -> u32 {
        self.luma_column_bytes() / 64
    }

    /// `P2_OUT_C_STRIDE` register value — half the luma one [r01 §8, §10].
    #[must_use]
    pub fn chroma_stride_reg(self) -> u32 {
        self.chroma_column_bytes() / 64
    }
}

/// Byte offset of tiled luma sample `(x, y)` within the luma plane, for the
/// 8-bit layout [r01 §8]: `col × column_bytes_total + y × 128 + (x mod 128)`.
///
/// Exposed (and unit-tested directly) because it is the one formula the whole
/// detile depends on; a round-trip alone could hide a matching error in tiler
/// and detiler.
#[must_use]
pub fn luma_offset_8bit(s: Surface, x: u32, y: u32) -> usize {
    let col = x / COLUMN_BYTES;
    let x_in_col = x % COLUMN_BYTES;
    (col * s.luma_column_bytes() + y * COLUMN_BYTES + x_in_col) as usize
}

/// Detile an 8-bit column-tiled plane into a linear, row-major byte plane of
/// `out_w × out_h` (`out_w` bytes per row) written to `out`. Works for the luma
/// plane and, since the chroma plane shares the column structure, for the
/// interleaved-CbCr chroma plane too (pass the chroma byte width/height and the
/// chroma column size).
///
/// `col_total_bytes` is the whole-column size for the plane being detiled.
/// Returns the number of bytes written, or **0 if `out` is too small** to hold
/// the plane — the caller sizes it from [`Surface`], so that is a caller bug,
/// but this reports it rather than panicking: the module compiles into a
/// bare-metal PIC driver where a panic is both unrecoverable and drags
/// `core::fmt` machinery into the image (which broke module load outright on
/// 2026-07-28). A short *tiled input* is different and benign — a decode that
/// overflowed its buffer leaves the affected samples zero.
pub fn detile_bytes(
    tiled: &[u8],
    out: &mut [u8],
    out_w: u32,
    out_h: u32,
    col_total_bytes: u32,
) -> usize {
    let n = (out_w as usize) * (out_h as usize);
    if out.len() < n {
        return 0;
    }
    for y in 0..out_h {
        for x in 0..out_w {
            let col = x / COLUMN_BYTES;
            let x_in_col = x % COLUMN_BYTES;
            let src = (col * col_total_bytes + y * COLUMN_BYTES + x_in_col) as usize;
            let dst = (y * out_w + x) as usize;
            out[dst] = tiled.get(src).copied().unwrap_or(0);
        }
    }
    n
}

/// Detile the 8-bit luma plane into `out`, cropped to `width × height`.
pub fn detile_luma_8bit(s: Surface, tiled: &[u8], out: &mut [u8]) -> usize {
    detile_bytes(tiled, out, s.width, s.height, s.luma_column_bytes())
}

/// Detile the 10-bit luma plane into P010 samples (one `u16` per luma sample,
/// value in the high 10 bits) in `out`, cropped to `width × height` [r01 §8].
///
/// The tiled column is 128 bytes = 32 little-endian words, each holding three
/// 10-bit samples in its low 30 bits. Sample `x` within a column is word
/// `(x mod 96) / 3`, lane `(x mod 96) mod 3`.
pub fn detile_luma_10bit(s: Surface, tiled: &[u8], out: &mut [u16]) -> usize {
    let n = (s.width as usize) * (s.height as usize);
    if out.len() < n {
        return 0;
    }
    let ppc = s.pixels_per_column(); // 96
    let col_total = s.luma_column_bytes();
    for y in 0..s.height {
        for x in 0..s.width {
            let col = x / ppc;
            let x_in_col = x % ppc;
            let word_idx = x_in_col / 3;
            let lane = x_in_col % 3;
            let word_off = (col * col_total + y * COLUMN_BYTES + word_idx * 4) as usize;
            let sample = match tiled.get(word_off..word_off + 4) {
                Some(b) => {
                    let w = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    ((w >> (lane * 10)) & 0x3FF) as u16
                }
                None => 0,
            };
            // P010: 10-bit value in the most-significant 10 bits of the u16.
            out[(y * s.width + x) as usize] = sample << 6;
        }
    }
    n
}

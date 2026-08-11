//! BCM2712 HEVC phase-2 launch programming (spec §10, §11).
//!
//! Assembles the ordered sequence of register writes that launches the
//! reconstruction engine for one picture, tying together the register offsets
//! and encoders of [`crate::hevc_bcm2712`] and the surface geometry of
//! [`crate::hevc_detile`]. It does **not** touch hardware: it drives a
//! caller-supplied writer, called once per `(offset, value)` in the order the
//! block requires. The platform driver passes a writer that does the MMIO
//! store; a test passes one that records the sequence.
//!
//! The one ordering rule this exists to guarantee: **the launching write
//! (`P2_ROWS`) is issued last, after every other register** [r01 §10, §11].
//! Every earlier write may be posted/relaxed; the launch must be ordered after
//! them, or the engine starts against a half-programmed register file. Encoding
//! that as the single place the sequence is built keeps the hazard in one
//! audited spot rather than scattered across a driver.
//!
//! **Clean-room Team B.** Register offsets, field layouts and the launch-last
//! ordering all cite released spec r01 §9–§11; no GPL source was read.

use crate::hevc_bcm2712 as hw;
use crate::hevc_detile::Surface;

/// One reference picture's address quad, as programmed into a reference slot
/// [r01 §9].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RefPic {
    /// Luma base, already encoded as byte-address >> 6 (see [`hw::dma_addr`]).
    pub luma_base: u32,
    pub luma_stride: u32,
    pub chroma_base: u32,
    pub chroma_stride: u32,
}

/// Everything the phase-2 launch needs for one picture [r01 §10]. Addresses
/// are the already-encoded (>>6) register forms; the caller encodes them with
/// [`hw::dma_addr`] and fills the geometry-derived strides from [`Surface`].
#[derive(Clone, Copy, Debug)]
pub struct Phase2Launch {
    pub surface: Surface,
    /// Intermediate streams written by phase 1, read here (same base+stride).
    pub pu_read_base: u32,
    pub pu_read_stride: u32,
    pub coeff_read_base: u32,
    pub coeff_read_stride: u32,
    /// From [`hw::p2_pic_cfg`].
    pub pic_cfg: u32,
    /// Output surface bases (>>6); strides come from `surface`.
    pub out_luma_base: u32,
    pub out_chroma_base: u32,
    /// Collocated-MV write base for this picture, or 0 if this picture's motion
    /// is not written [r01 §10 `P2_MV_WBASE`].
    pub mv_write_base: u32,
    /// Collocated-MV read base = the MV buffer of the collocated reference, or
    /// 0 if temporal MVP is unused [r01 §10 `P2_MV_RBASE`].
    pub mv_read_base: u32,
    /// MV record stride (/64) for both read and write [r01 §10].
    pub mv_stride: u32,
    /// Picture order count, signed [r01 §10 `P2_POC`].
    pub poc: i32,
    /// Picture height in CTB rows — the value written to `P2_ROWS` that
    /// **launches** the engine [r01 §10].
    pub ctb_rows: u32,
    /// All 16 reference slots. Unused slots should still hold a valid picture
    /// address so a corrupt stream cannot make the block fetch from an
    /// unprogrammed slot [r01 §9]; the caller is responsible for that fill.
    pub refs: [RefPic; 16],
}

/// Drive `write` with the full phase-2 launch sequence for one picture, in the
/// required order, ending with the launching `P2_ROWS` write [r01 §10, §11].
///
/// `write(offset, value)` is called once per register. Offsets are relative to
/// the main register block. The function performs no I/O and no ordering itself
/// beyond the call order; the caller's writer is responsible for the actual
/// store semantics (and for ensuring the final `P2_ROWS` store is ordered after
/// the rest, which §11 requires — this function guarantees it is issued last).
pub fn program_phase2(p: &Phase2Launch, write: &mut impl FnMut(u32, u32)) {
    // Intermediate stream read side (matches what phase 1 wrote) [r01 §10].
    write(hw::P2_PU_RBASE, p.pu_read_base);
    write(hw::P2_PU_RSTRIDE, p.pu_read_stride);
    write(hw::P2_COEFF_RBASE, p.coeff_read_base);
    write(hw::P2_COEFF_RSTRIDE, p.coeff_read_stride);

    // Picture configuration and geometry.
    write(hw::P2_PIC_CFG, p.pic_cfg);
    write(
        hw::P2_PIC_SIZE,
        hw::p2_pic_size(p.surface.width as u16, p.surface.height as u16),
    );
    write(hw::P2_POC, p.poc as u32);

    // Output surface: bases from the caller, strides from the geometry [r01 §8].
    write(hw::P2_OUT_Y_BASE, p.out_luma_base);
    write(hw::P2_OUT_Y_STRIDE, p.surface.luma_stride_reg());
    write(hw::P2_OUT_C_BASE, p.out_chroma_base);
    write(hw::P2_OUT_C_STRIDE, p.surface.chroma_stride_reg());

    // Collocated motion vectors: write side (0 when not written), read side
    // (0 when temporal MVP unused) [r01 §10].
    write(hw::P2_MV_WBASE, p.mv_write_base);
    write(hw::P2_MV_WSTRIDE, p.mv_stride);
    write(hw::P2_MV_RBASE, p.mv_read_base);
    write(hw::P2_MV_RSTRIDE, p.mv_stride);

    // All 16 reference slots [r01 §9].
    for (n, r) in p.refs.iter().enumerate() {
        let base = hw::ref_set_offset(n as u8);
        write(base, r.luma_base);
        write(base + 4, r.luma_stride);
        write(base + 8, r.chroma_base);
        write(base + 12, r.chroma_stride);
    }

    // LAUNCH — must be the last write [r01 §10, §11].
    write(hw::P2_ROWS, p.ctb_rows);
}

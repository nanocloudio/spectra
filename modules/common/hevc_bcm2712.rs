//! BCM2712 HEVC decode block — register-value encoding.
//!
//! Pure functions that turn parsed HEVC syntax into the exact 32-bit register
//! values the BCM2712 HEVC accelerator expects, plus the block's addressing
//! and buffer-sizing arithmetic. No MMIO, no allocation, no platform calls —
//! the actual register *writes*, interrupt handling and DMA-buffer management
//! are the platform driver's job (the "transport"). This is the layer above
//! it, and it is the bare-metal counterpart of [`crate::hevc_v4l2`]: identical
//! parsed inputs, but encoded as packed register words rather than ioctl
//! control structs.
//!
//! ## Provenance
//!
//! **Clean-room Team B.** Every value in this module derives solely from the
//! released functional specification
//! `.context/clean_room/spec/hevc_block_r01_released.md`, which was produced by
//! a separate team and passed gatekeeper review; no GPL driver source was read
//! to write it. Each register constant and bit layout cites its spec section
//! as `[r01 §x.y]`. That citation trail is an audit requirement, not a
//! courtesy: a value that cannot name the section that authorised it is a
//! provenance defect (see `rfc_hevc_clean_room.md` §7).
//!
//! Names here (`P1_SEQ_A`, …) are the spec's author-invented functional names,
//! themselves invented for description — they are not the hardware's or any
//! driver's identifiers.

#![allow(
    dead_code,
    reason = "the full register surface is encoded; the platform driver uses \
              the subset each decode phase needs"
)]

use crate::hevc::{Pps, SliceType, Sps};

// ─── Platform constants [r01 §2] ────────────────────────────────────────────

/// Main register block, bus address and size [r01 §2].
pub const MAIN_BASE: u64 = 0x10_0080_0000;
pub const MAIN_SIZE: u64 = 0x1_0000;
/// Interrupt-control block [r01 §2].
pub const IRQ_BASE: u64 = 0x10_0084_0000;
pub const IRQ_SIZE: u64 = 0x1000;
/// GIC SPI number, level-high [r01 §2].
pub const GIC_SPI: u32 = 98;
/// Firmware (mailbox) clock id; query max rate and set before use [r01 §2].
pub const CLOCK_ID: u32 = 11;
/// Value read from the version register on supported silicon [r01 §2, §6.1].
pub const HW_VERSION: u32 = 0x202;

// ─── DMA addressing [r01 §2] ────────────────────────────────────────────────

/// All DMA buffers are 64-byte aligned; addresses and strides are in 64-byte
/// units [r01 §2].
pub const DMA_ALIGN: u64 = 64;

/// Encode a byte address as the block programs it: the byte address shifted
/// right by 6 [r01 §2]. Returns `None` if the address is not 64-byte aligned
/// (which the hardware cannot express) or does not fit the 2^38-byte window.
#[must_use]
pub fn dma_addr(byte_addr: u64) -> Option<u32> {
    // Alignment/stride arithmetic is written as mask and shift, never `%`/`/`,
    // because DMA_ALIGN is a power of two AND because a u64 `%`/`/` emits
    // `__aeabi_uldivmod` on the 32-bit PIC target in a debug build — which
    // does not link (caught by `tools/ci/pic_link_check.sh`, ci phase 3.5).
    if byte_addr & (DMA_ALIGN - 1) != 0 {
        return None;
    }
    let shifted = byte_addr >> 6;
    u32::try_from(shifted).ok()
}

/// A stride/length in 64-byte units, `ceil(bytes / 64)` [r01 §2]. Used for the
/// stride registers (but not `P1_BS_LEN`, which is in bytes — see §6.2).
#[must_use]
pub fn stride_units(bytes: u64) -> u32 {
    // ceil(bytes / 64) as a shift; see dma_addr on why not `/`.
    u32::try_from((bytes + (DMA_ALIGN - 1)) >> 6).unwrap_or(u32::MAX)
}

// ─── Intermediate-stream sizing [r01 §4.1] ──────────────────────────────────

/// The size-rounding function S [r02 §R.1, superseding r01 §4.1].
///
/// `S(x) = 3·2^n` where `n = max(8, floor(log2 x))` — round `x` up to the next
/// member of `{3·2^k : k ≥ 8}`, with a hard minimum of `3·2^8 = 768`. Every
/// value is three times a power of two; the set contains **no** powers of two.
///
/// This corrects an earlier reading. r01 stated S two contradictory ways — a
/// compact formula whose `else` branch is unreachable, and an explicit
/// progression `256, 384, 512, 768, 1024, …`. Team-B question Q1 flagged the
/// contradiction; the r02 reconciliation settled it from source: the driver's
/// helper *always* returns `3·2^n` (the unreachable branch is dead code), so
/// **neither** r01 reading was right — not the progression this function once
/// implemented, and not a 256-byte floor (the exponent is floored at 8, so the
/// real minimum output is 768, and r01's "256" was a stale source comment).
///
/// Because grow (`S(old+1)`) always steps `n` up by one, each overflow retry
/// **doubles** the buffer: 768 → 1536 → 3072 → 6144 → … The runtime cost of an
/// undersized seed is therefore one bounded re-run, never a misdecode.
#[must_use]
pub fn size_round(x: u64) -> u64 {
    let log2_floor = if x == 0 {
        0
    } else {
        63 - x.leading_zeros() as u64
    };
    let n = log2_floor.max(8);
    3u64 << n
}

/// Next size after an overflow: `S(old + 1)` [r02 §R.1, r01 §6.4]. Because
/// `old` is always some `3·2^n`, this doubles it.
#[must_use]
pub fn size_grow(old: u64) -> u64 {
    size_round(old + 1)
}

/// Initial PU-stream size heuristic, `S(w*h/4)` [r01 §4 item 3].
#[must_use]
pub fn pu_stream_size(width: u64, height: u64) -> u64 {
    // `>> 2` not `/ 4`: constant power-of-two, and keeps the 32-bit target
    // clear of `__aeabi_uldivmod`.
    size_round((width * height) >> 2)
}

/// Initial coefficient-stream size heuristic, `S(w*h)` [r01 §4 item 4].
#[must_use]
pub fn coeff_stream_size(width: u64, height: u64) -> u64 {
    size_round(width * height)
}

// ─── Interrupt-control block [r01 §3] ───────────────────────────────────────

/// Engine-1 (entropy) interrupt-latched bit, write-1-to-clear [r01 §3].
pub const IRQ_ENG1_LATCHED: u32 = 1 << 0;
/// Engine-1 interrupt enable [r01 §3].
pub const IRQ_ENG1_ENABLE: u32 = 1 << 2;
/// Engine-2 (reconstruction) interrupt-latched bit, W1C [r01 §3].
pub const IRQ_ENG2_LATCHED: u32 = 1 << 4;
/// Engine-2 interrupt enable [r01 §3].
pub const IRQ_ENG2_ENABLE: u32 = 1 << 6;

/// The value to write to the interrupt-control register during
/// initialisation: only the two enable bits set, edge selects left at their
/// reset value 0 (latch on the falling edge of the active line = job
/// completion) and reserved bits 0 [r01 §3]. After this, read back and write
/// the read value (reserved bits forced 0) to clear stale latches.
pub const IRQ_INIT: u32 = IRQ_ENG1_ENABLE | IRQ_ENG2_ENABLE;

/// Reserved bits that must be written as zero [r01 §3]. Applied when writing
/// back a read value to clear latches, so an undefined read bit is never
/// written back as one.
pub const IRQ_RESERVED_MASK: u32 = (1 << 11) | (0xFF << 12);

/// Mask a value read from the interrupt register before writing it back, per
/// the "reserved bits forced to 0" rule [r01 §3].
#[must_use]
pub const fn irq_writeback(read_value: u32) -> u32 {
    read_value & !IRQ_RESERVED_MASK
}

// ─── Phase-1 register offsets [r01 §6.1] ────────────────────────────────────

pub const P1_SEQ_A: u32 = 0x00;
pub const P1_SEQ_B: u32 = 0x04;
pub const P1_PIC: u32 = 0x08;
pub const P1_SEG_CFG: u32 = 0x0C;
pub const P1_RGN_FIRST: u32 = 0x10;
pub const P1_RGN_LAST: u32 = 0x14;
pub const P1_SEG_FIRST: u32 = 0x18;
pub const P1_RUN_MODE: u32 = 0x1C;
pub const P1_QP: u32 = 0x30;
pub const P1_RUN_FROM: u32 = 0x34;
pub const P1_CHECKPOINT: u32 = 0x38;
pub const P1_VERSION: u32 = 0x3C;
pub const P1_BS_BASE: u32 = 0x40;
pub const P1_BS_LEN: u32 = 0x44;
pub const P1_BS_CTRL: u32 = 0x48;
pub const P1_PU_WBASE: u32 = 0x50;
pub const P1_PU_WSTRIDE: u32 = 0x54;
pub const P1_COEFF_WBASE: u32 = 0x58;
pub const P1_COEFF_WSTRIDE: u32 = 0x5C;
pub const P1_MSG_CTRL: u32 = 0x60;
pub const P1_RGN_LAST_INDEP: u32 = 0x64;
pub const P1_CTX_XFER: u32 = 0x68;
pub const P1_LIST_BASE: u32 = 0x6C;
pub const P1_LIST_COUNT: u32 = 0x70;
pub const P1_LIST_DONE: u32 = 0x74;

/// `P1_CHECKPOINT` read-back bit: coefficient stream overflowed [r01 §5, §6.1].
pub const P1_CHECKPOINT_COEFF_OVERFLOW: u32 = 1 << 3;
/// `P1_CHECKPOINT` read-back bit: PU stream overflowed [r01 §5, §6.1].
pub const P1_CHECKPOINT_PU_OVERFLOW: u32 = 1 << 4;

/// CABAC context-bank transfer commands [r01 §6.3]. Treated as opaque
/// save/restore constants (field semantics beyond these are open point O-4).
pub const P1_CTX_SAVE: u32 = 0x14500;
pub const P1_CTX_RESTORE: u32 = 0x14014;

/// Phase-1 command-list data windows [r01 §5].
pub const P1_WIN_CABAC_INIT: u32 = 0x1000;
pub const P1_WIN_DEQUANT: u32 = 0x2000;
pub const P1_WIN_SLICE_MSG: u32 = 0x4000;

// ─── Phase-1 sequence/picture registers [r01 §6.5] ──────────────────────────

/// `P1_SEQ_A` — sequence geometry and sample depths [r01 §6.5].
///
/// A pure function of the SPS. Every field is a raw log2 size or a depth; the
/// transform-block and hierarchy fields are why [`Sps`] had to start retaining
/// values it previously parsed and discarded.
#[must_use]
pub fn p1_seq_a(sps: &Sps) -> u32 {
    let log2_max_tb = sps.log2_min_tb_size + sps.log2_diff_max_min_tb;
    field(sps.log2_min_cb_size, 0)
        | field(sps.log2_ctb_size, 4)
        | field(sps.log2_min_tb_size, 8)
        | field(log2_max_tb, 12)
        | field(sps.bit_depth_luma, 16)
        | field(sps.bit_depth_chroma, 20)
        | field(sps.max_transform_hierarchy_depth_intra, 24)
        | field(sps.max_transform_hierarchy_depth_inter, 28)
}

/// `P1_SEQ_B` — PCM, chroma format and sequence tool flags [r01 §6.5].
#[must_use]
pub fn p1_seq_b(sps: &Sps) -> u32 {
    let log2_max_pcm = sps.log2_min_pcm_cb_size + sps.log2_diff_max_min_pcm_cb_size;
    // "write 0 if separate-colour-plane coding" [r01 §6.5].
    let chroma_fmt = if sps.separate_colour_plane {
        0
    } else {
        sps.chroma_format_idc
    };
    field(sps.pcm_bit_depth_luma, 0)
        | field(sps.pcm_bit_depth_chroma, 4)
        | field(sps.log2_min_pcm_cb_size, 8)
        | field(log2_max_pcm, 12)
        | field(chroma_fmt, 16)
        | bit(sps.amp_enabled, 18)
        | bit(sps.pcm_enabled, 19)
        | bit(sps.scaling_list_enabled, 20)
        | bit(sps.strong_intra_smoothing_enabled, 21)
}

/// `P1_PIC` — picture-level QP and tool parameters [r01 §6.5].
///
/// Written once per slice, because the chroma QP offsets fold the slice-level
/// offsets into bits 15:8 / 23:16. Two things are passed in rather than read
/// from the parsed structs:
/// - `log2_ctb_size` — bits 3:0 are `log2(CTB size) − diff_cu_qp_delta_depth`,
///   and the CTB size lives in the SPS this PPS references, which the caller
///   has when it programs a slice. Passing it keeps this a pure function
///   without re-threading the whole SPS.
/// - the slice chroma QP offsets — [`crate::hevc::SliceHeader`] does not yet
///   carry them; a slice with no `slice_chroma_qp_offsets_present` override
///   passes 0.
#[must_use]
pub fn p1_pic(pps: &Pps, log2_ctb_size: u8, slice_cb_qp_offset: i8, slice_cr_qp_offset: i8) -> u32 {
    let cb = (i16::from(pps.cb_qp_offset) + i16::from(slice_cb_qp_offset)) as u8;
    let cr = (i16::from(pps.cr_qp_offset) + i16::from(slice_cr_qp_offset)) as u8;
    let low = log2_ctb_size.saturating_sub(pps.diff_cu_qp_delta_depth);
    field(low, 0)
        | bit(pps.cu_qp_delta_enabled, 4)
        | bit(pps.transquant_bypass_enabled, 5)
        | bit(pps.transform_skip_enabled, 6)
        | bit(pps.sign_data_hiding_enabled, 7)
        | (u32::from(cb) << 8)
        | (u32::from(cr) << 16)
        | bit(pps.constrained_intra_pred, 24)
}

/// Initial luma QP for CABAC context init: `slice QP + 6×(luma_depth − 8)`
/// [r01 §6.5 `P1_QP`, §6.6 step 5].
#[must_use]
pub fn p1_qp(slice_qp: i32, luma_bit_depth: u8) -> i32 {
    slice_qp + 6 * (i32::from(luma_bit_depth) - 8)
}

// ─── Phase-1 per-entry-point slice config [r01 §6.6] ────────────────────────

/// Inputs to [`p1_seg_cfg`], the per-entry-point slice configuration [r01 §6.6].
///
/// The last-CTB width/height are the luma-sample size of the region's last CTB:
/// the full CTB size, or the picture-edge remainder when the region ends at a
/// picture edge whose dimension is not CTB-aligned. The caller computes those
/// (it knows the region geometry).
#[derive(Clone, Copy, Debug)]
pub struct SegConfig {
    pub slice_type: SliceType,
    pub max_merge_cand: u8,
    pub active_refs_l0: u8,
    pub active_refs_l1: u8,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub mvd_l1_zero: bool,
    pub last_ctb_width: u8,
    pub last_ctb_height: u8,
}

/// `P1_SEG_CFG` — the per-entry-point slice configuration word [r01 §6.6].
#[must_use]
pub fn p1_seg_cfg(cfg: &SegConfig) -> u32 {
    field(cfg.max_merge_cand, 0)
        | field(cfg.active_refs_l0, 4)
        | field(cfg.active_refs_l1, 8)
        | (u32::from(seg_slice_type(cfg.slice_type)) << 12)
        | bit(cfg.sao_luma, 14)
        | bit(cfg.sao_chroma, 15)
        | bit(cfg.mvd_l1_zero, 16)
        | (u32::from(cfg.last_ctb_width & 0x7F) << 17)
        | (u32::from(cfg.last_ctb_height & 0x7F) << 24)
}

/// Slice-type code in `P1_SEG_CFG` bits 13:12: 0 = B, 1 = P, 2 = I [r01 §6.6].
#[must_use]
const fn seg_slice_type(t: SliceType) -> u8 {
    match t {
        SliceType::B => 0,
        SliceType::P => 1,
        SliceType::I => 2,
    }
}

/// Pack a (column, row) CTB address as the region/segment registers expect:
/// column in bits 15:0, row in bits 31:16 [r01 §6.6, e.g. `P1_RGN_FIRST`].
#[must_use]
pub fn ctb_addr(col: u16, row: u16) -> u32 {
    u32::from(col) | (u32::from(row) << 16)
}

/// Pack a checkpoint word: `reason | (col << 5) | (row << 18)` with reason in
/// bits 4:0, column 17:5, row 31:18 [r01 §6.6].
#[must_use]
pub fn checkpoint(reason: u8, col: u32, row: u32) -> u32 {
    (u32::from(reason) & 0x1F) | ((col & 0x1FFF) << 5) | ((row & 0x3FFF) << 18)
}

/// Checkpoint reasons [r01 §6.6].
pub const CHECKPOINT_SLICE_END: u8 = 1;
pub const CHECKPOINT_REGION_END: u8 = 2;
pub const CHECKPOINT_WPP_ROW_PAUSE: u8 = 5;

// ─── Command list [r01 §5] ──────────────────────────────────────────────────

/// One command-list entry: a 64-bit little-endian word whose low 16 bits are
/// the target offset within the phase-1 command window, bits 31:16 zero, and
/// bits 63:32 the value to write [r01 §5].
#[must_use]
pub fn command(target_offset: u16, value: u32) -> u64 {
    u64::from(target_offset) | (u64::from(value) << 32)
}

// ─── Phase-2 register offsets and config [r01 §10] ──────────────────────────

pub const P2_PU_RBASE: u32 = 0x8000;
pub const P2_PU_RSTRIDE: u32 = 0x8004;
pub const P2_COEFF_RBASE: u32 = 0x8008;
pub const P2_COEFF_RSTRIDE: u32 = 0x800C;
pub const P2_ROWS: u32 = 0x8010;
pub const P2_PIC_CFG: u32 = 0x8014;
pub const P2_OUT_Y_BASE: u32 = 0x8018;
pub const P2_OUT_Y_STRIDE: u32 = 0x801C;
pub const P2_OUT_C_BASE: u32 = 0x8020;
pub const P2_OUT_C_STRIDE: u32 = 0x8024;
pub const P2_PIC_SIZE: u32 = 0x802C;
pub const P2_MV_WBASE: u32 = 0x8030;
pub const P2_MV_WSTRIDE: u32 = 0x8034;
pub const P2_MV_RBASE: u32 = 0x8038;
pub const P2_MV_RSTRIDE: u32 = 0x803C;
pub const P2_POC: u32 = 0x8040;

/// Reference-picture register sets: 16 sets at M+0x9000, 16 bytes apart
/// [r01 §9].
pub const P2_REF_SET_BASE: u32 = 0x9000;
pub const P2_REF_SET_STRIDE: u32 = 16;

/// Offset of reference set `n` [r01 §9]. The hardware has exactly 16 slots, so
/// `n` is masked rather than asserted: this compiles into a bare-metal PIC
/// driver where a panic is unrecoverable — and a *formatted* panic drags
/// `core::fmt` into the image, which on 2026-07-28 stopped the decode module
/// loading at all (zero telemetry, indistinguishable from a dead board).
/// Wrapping to a valid slot is safer than addressing past the register file;
/// callers iterate 0..16, so the mask is unreachable in practice.
#[must_use]
pub fn ref_set_offset(n: u8) -> u32 {
    P2_REF_SET_BASE + P2_REF_SET_STRIDE * u32::from(n & 0x0F)
}

/// `P2_PIC_SIZE`: luma width in 15:0, height in 31:16 [r01 §10].
#[must_use]
pub fn p2_pic_size(width: u16, height: u16) -> u32 {
    u32::from(width) | (u32::from(height) << 16)
}

/// `P2_PIC_CFG` — reconstruction-engine picture configuration [r01 §10].
///
/// Two inputs are picture-runtime decisions the SPS/PPS alone do not settle,
/// so they are parameters: `write_collocated_mv` (bit 15 — set when this
/// picture's motion may be referenced later: temporal MVP enabled and the
/// picture is a reference within sub-layer limits) and `temporal_mvp_for_pic`
/// (bit 19 — temporal MVP enabled for this picture's slices).
#[must_use]
pub fn p2_pic_cfg(
    sps: &Sps,
    pps: &Pps,
    write_collocated_mv: bool,
    temporal_mvp_for_pic: bool,
) -> u32 {
    field(sps.bit_depth_luma, 0)
        | field(sps.bit_depth_chroma, 4)
        | bit(sps.bit_depth_luma > 8, 8)
        | bit(sps.bit_depth_chroma > 8, 9)
        | (u32::from(sps.log2_ctb_size & 0x7) << 10)
        | bit(pps.constrained_intra_pred, 13)
        | bit(sps.strong_intra_smoothing_enabled, 14)
        | bit(write_collocated_mv, 15)
        | (u32::from(pps.log2_parallel_merge_level & 0x7) << 16)
        | bit(temporal_mvp_for_pic, 19)
        | bit(sps.pcm_loop_filter_disabled, 20)
        | (u32::from((pps.cb_qp_offset as u8) & 0x1F) << 21)
        | (u32::from((pps.cr_qp_offset as u8) & 0x1F) << 26)
}

// ─── bit-packing helpers ────────────────────────────────────────────────────

/// Place a small unsigned value at a bit offset. The value is masked to a
/// nibble (4 bits) because every field packed with this helper is a 4-bit
/// log2/depth field; a value that does not fit is a bug in the caller's units,
/// not something to silently widen.
#[inline]
fn field(value: u8, shift: u32) -> u32 {
    (u32::from(value) & 0xF) << shift
}

#[inline]
fn bit(set: bool, shift: u32) -> u32 {
    if set {
        1 << shift
    } else {
        0
    }
}

//! HEVC phase-1 slice-parameter message words (spec §7.3).
//!
//! Before decoding a slice's data, phase 1 is given a short sequence of 16-bit
//! messages describing the slice and its references, committed through the
//! slice-message FIFO window [r01 §7.3]. This module encodes those message
//! words. Each is a pure bit-packing of parsed slice state plus DPB slot
//! numbers (from [`crate::hevc_dpb`]), so — like the register encoders — the
//! words are checked against the spec's bit layouts directly.
//!
//! **A three-way slice-type gotcha.** This block encodes the slice type three
//! different ways: the §7.3 messages use `I=1, P=2, B=3`; the §6.6 `P1_SEG_CFG`
//! register uses `B=0, P=1, I=2`; and the V4L2 stateless path uses yet another.
//! They are independent and must not be interchanged — [`msg_slice_type`] is
//! deliberately separate from `hevc_bcm2712`'s seg-config encoding, and the
//! tests pin both so a copy-paste between them is caught.
//!
//! **Clean-room Team B.** All bit layouts from released spec r01 §7.3. No GPL
//! source read.

use crate::hevc::SliceType;

/// Command-window base for the slice-message FIFO [r01 §5, §7.3].
pub const SLICE_MSG_WINDOW: u32 = 0x4000;

/// Slice-type code in the §7.3 **message** word: `I=1, P=2, B=3` [r01 §7.3].
/// Note this differs from every other slice-type encoding in the block.
#[must_use]
pub const fn msg_slice_type(t: SliceType) -> u16 {
    match t {
        SliceType::I => 1,
        SliceType::P => 2,
        SliceType::B => 3,
    }
}

/// The slice command word (message 1) [r01 §7.3].
///
/// `no_backward_prediction` is the H.265 §8.3.5 predicate — true when every
/// referenced picture in both lists has POC ≤ the current POC — which
/// [`no_backward_prediction`] computes. `collocated_from_l0` is true when
/// temporal MVP is off, the slice is not B, or the slice signals its
/// collocated reference in L0.
#[must_use]
pub fn slice_command_word(
    slice_type: SliceType,
    active_refs_l0: u8,
    active_refs_l1: u8,
    no_backward_pred: bool,
    max_merge_cand: u8,
    collocated_from_l0: bool,
) -> u16 {
    (msg_slice_type(slice_type) & 0x3)
        | ((u16::from(active_refs_l0) & 0xF) << 2)
        | ((u16::from(active_refs_l1) & 0xF) << 6)
        | (u16::from(no_backward_pred) << 10)
        | ((u16::from(max_merge_cand) & 0x7) << 11)
        | (u16::from(collocated_from_l0) << 14)
}

/// One reference's descriptor word (part of message 2) [r01 §7.3]: DPB slot in
/// bits 3:0, long-term in bit 4, and bits 6:5 = 3 when weighted prediction
/// applies to this reference (else 0).
#[must_use]
pub fn ref_descriptor(dpb_slot: u8, long_term: bool, weighted: bool) -> u16 {
    (u16::from(dpb_slot) & 0xF) | (u16::from(long_term) << 4) | (if weighted { 3 << 5 } else { 0 })
}

/// The deblocking message word (message 3) [r01 §7.3].
#[must_use]
pub fn deblocking_word(
    beta_offset_div2: i8,
    tc_offset_div2: i8,
    disabled: bool,
    loop_filter_across_slices: bool,
    loop_filter_across_tiles: bool,
) -> u16 {
    (u16::from((beta_offset_div2 as u8) & 0xF))
        | (u16::from((tc_offset_div2 as u8) & 0xF) << 4)
        | (u16::from(disabled) << 8)
        | (u16::from(loop_filter_across_slices) << 9)
        | (u16::from(loop_filter_across_tiles) << 10)
}

/// The chroma-QP message word (message 4) [r01 §7.3]: slice Cb offset in bits
/// 4:0, Cr offset in bits 9:5.
#[must_use]
pub fn chroma_qp_word(slice_cb_qp_offset: i8, slice_cr_qp_offset: i8) -> u16 {
    (u16::from((slice_cb_qp_offset as u8) & 0x1F))
        | (u16::from((slice_cr_qp_offset as u8) & 0x1F) << 5)
}

/// `P1_MSG_CTRL` value that commits a batch of messages: count in bits 7:0, a
/// per-picture slice sequence number in bits 15:8 [r01 §6.1, §7.3].
#[must_use]
pub fn msg_commit(count: u8, slice_seq: u8) -> u32 {
    u32::from(count) | (u32::from(slice_seq) << 8)
}

/// The H.265 §8.3.5 no-backward-prediction predicate: true when **every**
/// referenced picture in both lists has POC ≤ the current picture's POC
/// [r01 §7.3 msg 1 bit 10]. An empty reference set (an I slice) is vacuously
/// true.
#[must_use]
pub fn no_backward_prediction(current_poc: i32, ref_pocs: &[i32]) -> bool {
    ref_pocs.iter().all(|&p| p <= current_poc)
}

/// The six weighted-prediction messages that follow a reference's descriptor
/// and POC when weighted prediction applies [r01 §7.3].
///
/// Three (weight, offset) pairs — luma, then Cb, then Cr — each encoded as:
///
/// * word 0: `log2 weight denominator` in bits 2:0, reconstructed weight
///   (`delta + (1 << denom)`, 9 bits) in bits 11:3;
/// * word 1: `offset & 0xFF`.
///
/// The weight is the RECONSTRUCTED value, not the coded delta, and the chroma
/// offset is the derived `ChromaOffsetLX` rather than its coded delta — both
/// are already reconstructed by [`crate::hevc::PredWeightTable`], which is why
/// this takes plain numbers.
///
/// Returns the six words in the order the FIFO expects.
#[must_use]
pub fn weight_words(
    luma_denom: u8,
    luma_weight: i16,
    luma_offset: i16,
    chroma_denom: u8,
    chroma_weight: [i16; 2],
    chroma_offset: [i16; 2],
) -> [u16; 6] {
    [
        weight_word(luma_denom, luma_weight),
        offset_word(luma_offset),
        weight_word(chroma_denom, chroma_weight[0]),
        offset_word(chroma_offset[0]),
        weight_word(chroma_denom, chroma_weight[1]),
        offset_word(chroma_offset[1]),
    ]
}

/// One weight word: denominator in bits 2:0, 9-bit reconstructed weight in
/// bits 11:3 [r01 §7.3].
#[must_use]
pub fn weight_word(denom: u8, weight: i16) -> u16 {
    (u16::from(denom) & 0x7) | (((weight as u16) & 0x1FF) << 3)
}

/// One offset word: the offset in bits 7:0 [r01 §7.3].
#[must_use]
pub fn offset_word(offset: i16) -> u16 {
    (offset as u16) & 0xFF
}

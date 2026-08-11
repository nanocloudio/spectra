//! HEVC parsed syntax → stateless-decode control structures.
//!
//! The Linux V4L2 *stateless* HEVC decoder (`V4L2_CID_STATELESS_HEVC_SPS` /
//! `_PPS` / `_SLICE_PARAMS` / `_DECODE_PARAMS`) takes the bitstream *already
//! parsed* — the host does the syntax work and hands the hardware filled
//! structures plus the raw slice data. This module fills those structures from
//! the [`crate::hevc`] front end.
//!
//! ## Why this lives in the core, not in a Linux provider
//!
//! These structs are the V4L2 uAPI, but the *mapping* — which parsed field
//! feeds which hardware input, how the biases (`_minus1`, `_minus8`) and the
//! flag words are formed — is not Linux-specific. It is the programming model
//! of the decode block, and the bare-metal BCM2712 driver performs the exact
//! same mapping, differing only in that it writes the values to registers
//! instead of passing the struct to an ioctl. Building it here, against the
//! Linux decoder as a correctness oracle, means the register driver inherits a
//! validated mapping rather than re-deriving one.
//!
//! ## What this module does and does not do
//!
//! It covers the parts that are a **pure function of the bitstream**: the SPS
//! and PPS, and the syntax-derived fields of the slice parameters. It does
//! **not** fill the fields that depend on decoded-picture-buffer *runtime
//! state* — the DPB entries, the `ref_idx_*` arrays, `collocated_ref_idx`, and
//! the `poc_st_curr_*` index lists — because those are a mapping from POC to
//! whichever capture buffer currently holds that reference, which only the
//! component managing the buffer pool knows. Those are filled by the provider,
//! from the POC values this module and the [`crate::hevc`] reference-list
//! derivation produce. The split is deliberate and is where the pure/impure
//! boundary genuinely falls.
//!
//! Layouts and field semantics: the kernel's
//! `Documentation/userspace-api/media/v4l/ext-ctrls-codec-stateless.rst`, the
//! published userspace API. No driver source is involved.

#![allow(
    dead_code,
    reason = "the full uAPI surface is defined; providers use the subset their \
              hardware path needs"
)]

use crate::hevc::{Pps, SliceType, Sps};

// ─── SPS ────────────────────────────────────────────────────────────────────

pub const V4L2_HEVC_SPS_FLAG_SEPARATE_COLOUR_PLANE: u64 = 0x0000_0001;
pub const V4L2_HEVC_SPS_FLAG_SCALING_LIST_ENABLED: u64 = 0x0000_0002;
pub const V4L2_HEVC_SPS_FLAG_AMP_ENABLED: u64 = 0x0000_0004;
pub const V4L2_HEVC_SPS_FLAG_SAMPLE_ADAPTIVE_OFFSET: u64 = 0x0000_0008;
pub const V4L2_HEVC_SPS_FLAG_PCM_ENABLED: u64 = 0x0000_0010;
pub const V4L2_HEVC_SPS_FLAG_PCM_LOOP_FILTER_DISABLED: u64 = 0x0000_0020;
pub const V4L2_HEVC_SPS_FLAG_LONG_TERM_REF_PICS_PRESENT: u64 = 0x0000_0040;
pub const V4L2_HEVC_SPS_FLAG_SPS_TEMPORAL_MVP_ENABLED: u64 = 0x0000_0080;
pub const V4L2_HEVC_SPS_FLAG_STRONG_INTRA_SMOOTHING_ENABLED: u64 = 0x0000_0100;

/// `struct v4l2_ctrl_hevc_sps`. Field order and types mirror the uAPI so the
/// struct is ABI-compatible when passed to an ioctl on Linux; on any other
/// target it is simply the neutral description of the sequence the hardware
/// needs.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V4l2CtrlHevcSps {
    pub video_parameter_set_id: u8,
    pub seq_parameter_set_id: u8,
    pub pic_width_in_luma_samples: u16,
    pub pic_height_in_luma_samples: u16,
    pub bit_depth_luma_minus8: u8,
    pub bit_depth_chroma_minus8: u8,
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    pub sps_max_dec_pic_buffering_minus1: u8,
    pub sps_max_num_reorder_pics: u8,
    pub sps_max_latency_increase_plus1: u8,
    pub log2_min_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_luma_coding_block_size: u8,
    pub log2_min_luma_transform_block_size_minus2: u8,
    pub log2_diff_max_min_luma_transform_block_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub pcm_sample_bit_depth_luma_minus1: u8,
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    pub num_short_term_ref_pic_sets: u8,
    pub num_long_term_ref_pics_sps: u8,
    pub chroma_format_idc: u8,
    pub sps_max_sub_layers_minus1: u8,
    pub flags: u64,
}

/// Fill the SPS control from a parsed [`Sps`].
///
/// Every value here is a pure function of the sequence header. The biases are
/// the uAPI's, not the bitstream's: the kernel struct stores several fields in
/// the same `_minusN` form the bitstream codes them in, so where [`Sps`] has
/// already removed a bias (it stores `bit_depth_luma`, not the coded
/// `_minus8`) this re-applies it. Getting a bias wrong yields a plausible but
/// wrong decode — a picture at the wrong size or bit depth — so each is
/// checked in the tests against a known stream.
#[must_use]
pub fn map_sps(sps: &Sps) -> V4l2CtrlHevcSps {
    let mut flags = 0u64;
    set_if(
        &mut flags,
        sps.separate_colour_plane,
        V4L2_HEVC_SPS_FLAG_SEPARATE_COLOUR_PLANE,
    );
    set_if(
        &mut flags,
        sps.scaling_list_enabled,
        V4L2_HEVC_SPS_FLAG_SCALING_LIST_ENABLED,
    );
    set_if(&mut flags, sps.amp_enabled, V4L2_HEVC_SPS_FLAG_AMP_ENABLED);
    set_if(
        &mut flags,
        sps.sao_enabled,
        V4L2_HEVC_SPS_FLAG_SAMPLE_ADAPTIVE_OFFSET,
    );
    set_if(&mut flags, sps.pcm_enabled, V4L2_HEVC_SPS_FLAG_PCM_ENABLED);
    set_if(
        &mut flags,
        sps.pcm_loop_filter_disabled,
        V4L2_HEVC_SPS_FLAG_PCM_LOOP_FILTER_DISABLED,
    );
    set_if(
        &mut flags,
        sps.long_term_ref_pics_present,
        V4L2_HEVC_SPS_FLAG_LONG_TERM_REF_PICS_PRESENT,
    );
    set_if(
        &mut flags,
        sps.temporal_mvp_enabled,
        V4L2_HEVC_SPS_FLAG_SPS_TEMPORAL_MVP_ENABLED,
    );
    set_if(
        &mut flags,
        sps.strong_intra_smoothing_enabled,
        V4L2_HEVC_SPS_FLAG_STRONG_INTRA_SMOOTHING_ENABLED,
    );

    V4l2CtrlHevcSps {
        video_parameter_set_id: sps.vps_id,
        seq_parameter_set_id: sps.sps_id,
        // Coded, pre-crop dimensions: the hardware decodes the full coded
        // picture and cropping is applied downstream.
        pic_width_in_luma_samples: sps.width as u16,
        pic_height_in_luma_samples: sps.height as u16,
        bit_depth_luma_minus8: sps.bit_depth_luma.saturating_sub(8),
        bit_depth_chroma_minus8: sps.bit_depth_chroma.saturating_sub(8),
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_poc_lsb.saturating_sub(4),
        sps_max_dec_pic_buffering_minus1: sps.max_dec_pic_buffering.saturating_sub(1),
        sps_max_num_reorder_pics: sps.max_num_reorder_pics,
        // The uAPI truncates the latency-increase to 8 bits; a value that does
        // not fit is not a real stream (it would exceed the DPB many times
        // over), so saturating is safe and never triggers in practice.
        sps_max_latency_increase_plus1: sps.max_latency_increase_plus1.min(255) as u8,
        log2_min_luma_coding_block_size_minus3: sps.log2_min_cb_size.saturating_sub(3),
        log2_diff_max_min_luma_coding_block_size: sps
            .log2_ctb_size
            .saturating_sub(sps.log2_min_cb_size),
        log2_min_luma_transform_block_size_minus2: sps.log2_min_tb_size.saturating_sub(2),
        log2_diff_max_min_luma_transform_block_size: sps.log2_diff_max_min_tb,
        max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
        pcm_sample_bit_depth_luma_minus1: sps.pcm_bit_depth_luma.saturating_sub(1),
        pcm_sample_bit_depth_chroma_minus1: sps.pcm_bit_depth_chroma.saturating_sub(1),
        log2_min_pcm_luma_coding_block_size_minus3: sps.log2_min_pcm_cb_size.saturating_sub(3),
        log2_diff_max_min_pcm_luma_coding_block_size: sps.log2_diff_max_min_pcm_cb_size,
        num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets,
        num_long_term_ref_pics_sps: sps.num_long_term_ref_pics_sps,
        chroma_format_idc: sps.chroma_format_idc,
        sps_max_sub_layers_minus1: sps.max_sub_layers_minus1,
        flags,
    }
}

// ─── PPS ────────────────────────────────────────────────────────────────────

pub const V4L2_HEVC_PPS_FLAG_DEPENDENT_SLICE_SEGMENT_ENABLED: u64 = 0x0000_0001;
pub const V4L2_HEVC_PPS_FLAG_OUTPUT_FLAG_PRESENT: u64 = 0x0000_0002;
pub const V4L2_HEVC_PPS_FLAG_SIGN_DATA_HIDING_ENABLED: u64 = 0x0000_0004;
pub const V4L2_HEVC_PPS_FLAG_CABAC_INIT_PRESENT: u64 = 0x0000_0008;
pub const V4L2_HEVC_PPS_FLAG_CONSTRAINED_INTRA_PRED: u64 = 0x0000_0010;
pub const V4L2_HEVC_PPS_FLAG_TRANSFORM_SKIP_ENABLED: u64 = 0x0000_0020;
pub const V4L2_HEVC_PPS_FLAG_CU_QP_DELTA_ENABLED: u64 = 0x0000_0040;
pub const V4L2_HEVC_PPS_FLAG_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT: u64 = 0x0000_0080;
pub const V4L2_HEVC_PPS_FLAG_WEIGHTED_PRED: u64 = 0x0000_0100;
pub const V4L2_HEVC_PPS_FLAG_WEIGHTED_BIPRED: u64 = 0x0000_0200;
pub const V4L2_HEVC_PPS_FLAG_TRANSQUANT_BYPASS_ENABLED: u64 = 0x0000_0400;
pub const V4L2_HEVC_PPS_FLAG_TILES_ENABLED: u64 = 0x0000_0800;
pub const V4L2_HEVC_PPS_FLAG_ENTROPY_CODING_SYNC_ENABLED: u64 = 0x0000_1000;
pub const V4L2_HEVC_PPS_FLAG_LOOP_FILTER_ACROSS_TILES_ENABLED: u64 = 0x0000_2000;
pub const V4L2_HEVC_PPS_FLAG_PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 0x0000_4000;
pub const V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_OVERRIDE_ENABLED: u64 = 0x0000_8000;
pub const V4L2_HEVC_PPS_FLAG_PPS_DISABLE_DEBLOCKING_FILTER: u64 = 0x0001_0000;
pub const V4L2_HEVC_PPS_FLAG_LISTS_MODIFICATION_PRESENT: u64 = 0x0002_0000;
pub const V4L2_HEVC_PPS_FLAG_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT: u64 = 0x0004_0000;
pub const V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_CONTROL_PRESENT: u64 = 0x0008_0000;
pub const V4L2_HEVC_PPS_FLAG_UNIFORM_SPACING: u64 = 0x0010_0000;

/// `struct v4l2_ctrl_hevc_pps`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct V4l2CtrlHevcPps {
    pub pic_parameter_set_id: u8,
    pub num_extra_slice_header_bits: u8,
    pub num_ref_idx_l0_default_active_minus1: u8,
    pub num_ref_idx_l1_default_active_minus1: u8,
    pub init_qp_minus26: i8,
    pub diff_cu_qp_delta_depth: u8,
    pub pps_cb_qp_offset: i8,
    pub pps_cr_qp_offset: i8,
    pub num_tile_columns_minus1: u8,
    pub num_tile_rows_minus1: u8,
    pub column_width_minus1: [u8; 20],
    pub row_height_minus1: [u8; 22],
    pub pps_beta_offset_div2: i8,
    pub pps_tc_offset_div2: i8,
    pub log2_parallel_merge_level_minus2: u8,
    pub padding: [u8; 4],
    pub flags: u64,
}

/// Fill the PPS control from a parsed [`Pps`].
///
/// The `column_width_minus1` / `row_height_minus1` arrays are left zero: they
/// carry explicit tile sizes only for *non-uniform* spacing, which [`Pps`]
/// does not currently retain the per-tile widths for. Uniform spacing — the
/// overwhelmingly common case, and the only one x265 emits — needs no explicit
/// widths, so the hardware derives them from the tile counts. A non-uniform,
/// explicitly-sized tiling would need those arrays filled; that is called out
/// as a limitation rather than silently mis-decoded, because a zero-width tile
/// is not a benign default.
#[must_use]
pub fn map_pps(pps: &Pps) -> V4l2CtrlHevcPps {
    let mut flags = 0u64;
    set_if(
        &mut flags,
        pps.dependent_slice_segments_enabled,
        V4L2_HEVC_PPS_FLAG_DEPENDENT_SLICE_SEGMENT_ENABLED,
    );
    set_if(
        &mut flags,
        pps.output_flag_present,
        V4L2_HEVC_PPS_FLAG_OUTPUT_FLAG_PRESENT,
    );
    set_if(
        &mut flags,
        pps.sign_data_hiding_enabled,
        V4L2_HEVC_PPS_FLAG_SIGN_DATA_HIDING_ENABLED,
    );
    set_if(
        &mut flags,
        pps.cabac_init_present,
        V4L2_HEVC_PPS_FLAG_CABAC_INIT_PRESENT,
    );
    set_if(
        &mut flags,
        pps.constrained_intra_pred,
        V4L2_HEVC_PPS_FLAG_CONSTRAINED_INTRA_PRED,
    );
    set_if(
        &mut flags,
        pps.transform_skip_enabled,
        V4L2_HEVC_PPS_FLAG_TRANSFORM_SKIP_ENABLED,
    );
    set_if(
        &mut flags,
        pps.cu_qp_delta_enabled,
        V4L2_HEVC_PPS_FLAG_CU_QP_DELTA_ENABLED,
    );
    set_if(
        &mut flags,
        pps.slice_chroma_qp_offsets_present,
        V4L2_HEVC_PPS_FLAG_PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT,
    );
    set_if(
        &mut flags,
        pps.weighted_pred,
        V4L2_HEVC_PPS_FLAG_WEIGHTED_PRED,
    );
    set_if(
        &mut flags,
        pps.weighted_bipred,
        V4L2_HEVC_PPS_FLAG_WEIGHTED_BIPRED,
    );
    set_if(
        &mut flags,
        pps.transquant_bypass_enabled,
        V4L2_HEVC_PPS_FLAG_TRANSQUANT_BYPASS_ENABLED,
    );
    set_if(
        &mut flags,
        pps.tiles_enabled,
        V4L2_HEVC_PPS_FLAG_TILES_ENABLED,
    );
    set_if(
        &mut flags,
        pps.entropy_coding_sync_enabled,
        V4L2_HEVC_PPS_FLAG_ENTROPY_CODING_SYNC_ENABLED,
    );
    set_if(
        &mut flags,
        pps.loop_filter_across_tiles_enabled,
        V4L2_HEVC_PPS_FLAG_LOOP_FILTER_ACROSS_TILES_ENABLED,
    );
    set_if(
        &mut flags,
        pps.loop_filter_across_slices_enabled,
        V4L2_HEVC_PPS_FLAG_PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED,
    );
    set_if(
        &mut flags,
        pps.deblocking_filter_override_enabled,
        V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_OVERRIDE_ENABLED,
    );
    set_if(
        &mut flags,
        pps.deblocking_filter_disabled,
        V4L2_HEVC_PPS_FLAG_PPS_DISABLE_DEBLOCKING_FILTER,
    );
    set_if(
        &mut flags,
        pps.lists_modification_present,
        V4L2_HEVC_PPS_FLAG_LISTS_MODIFICATION_PRESENT,
    );
    set_if(
        &mut flags,
        pps.slice_segment_header_extension_present,
        V4L2_HEVC_PPS_FLAG_SLICE_SEGMENT_HEADER_EXTENSION_PRESENT,
    );
    set_if(
        &mut flags,
        pps.deblocking_filter_control_present,
        V4L2_HEVC_PPS_FLAG_DEBLOCKING_FILTER_CONTROL_PRESENT,
    );
    set_if(
        &mut flags,
        pps.uniform_spacing,
        V4L2_HEVC_PPS_FLAG_UNIFORM_SPACING,
    );

    V4l2CtrlHevcPps {
        pic_parameter_set_id: pps.pps_id,
        num_extra_slice_header_bits: pps.num_extra_slice_header_bits,
        // [`Pps`] stores the effective active counts (post `+1`); the uAPI
        // wants the coded `_minus1` form.
        num_ref_idx_l0_default_active_minus1: pps.num_ref_idx_l0_default_active.saturating_sub(1),
        num_ref_idx_l1_default_active_minus1: pps.num_ref_idx_l1_default_active.saturating_sub(1),
        // [`Pps::init_qp`] already has the `+26` applied; the uAPI wants it
        // removed again.
        init_qp_minus26: pps.init_qp - 26,
        diff_cu_qp_delta_depth: pps.diff_cu_qp_delta_depth,
        pps_cb_qp_offset: pps.cb_qp_offset,
        pps_cr_qp_offset: pps.cr_qp_offset,
        num_tile_columns_minus1: (pps.num_tile_columns.saturating_sub(1)) as u8,
        num_tile_rows_minus1: (pps.num_tile_rows.saturating_sub(1)) as u8,
        column_width_minus1: [0; 20],
        row_height_minus1: [0; 22],
        pps_beta_offset_div2: pps.beta_offset_div2,
        pps_tc_offset_div2: pps.tc_offset_div2,
        log2_parallel_merge_level_minus2: pps.log2_parallel_merge_level.saturating_sub(2),
        padding: [0; 4],
        flags,
    }
}

// ─── Slice params (syntax-derived portion) ──────────────────────────────────

pub const V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_SAO_LUMA: u64 = 0x0000_0001;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_SAO_CHROMA: u64 = 0x0000_0002;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_TEMPORAL_MVP_ENABLED: u64 = 0x0000_0004;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_MVD_L1_ZERO: u64 = 0x0000_0008;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_CABAC_INIT: u64 = 0x0000_0010;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_COLLOCATED_FROM_L0: u64 = 0x0000_0020;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_USE_INTEGER_MV: u64 = 0x0000_0040;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_DEBLOCKING_FILTER_DISABLED: u64 = 0x0000_0080;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED: u64 = 0x0000_0100;
pub const V4L2_HEVC_SLICE_PARAMS_FLAG_DEPENDENT_SLICE_SEGMENT: u64 = 0x0000_0200;

/// uAPI `slice_type` values.
pub const V4L2_HEVC_SLICE_TYPE_B: u8 = 0;
pub const V4L2_HEVC_SLICE_TYPE_P: u8 = 1;
pub const V4L2_HEVC_SLICE_TYPE_I: u8 = 2;

#[must_use]
pub const fn slice_type_value(t: SliceType) -> u8 {
    match t {
        SliceType::B => V4L2_HEVC_SLICE_TYPE_B,
        SliceType::P => V4L2_HEVC_SLICE_TYPE_P,
        SliceType::I => V4L2_HEVC_SLICE_TYPE_I,
    }
}

// ─── helpers ────────────────────────────────────────────────────────────────

#[inline]
fn set_if(flags: &mut u64, cond: bool, bit: u64) {
    if cond {
        *flags |= bit;
    }
}

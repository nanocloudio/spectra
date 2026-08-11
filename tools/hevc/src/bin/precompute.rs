//! Derive a clip's BCM2712 phase-1/phase-2 constants from its own bitstream.
//!
//! The bare-metal driver in `modules/app/hevc_decode` embeds a test clip's
//! register values as constants, because a PIC module has no filesystem to
//! read a clip from. Those constants were originally derived by hand, which
//! does not scale past one clip and is exactly the kind of transcription work
//! that quietly goes wrong — a wrong `SEG_CFG` does not fail to build, it
//! decodes a wrong picture.
//!
//! This tool derives them mechanically instead: it parses the clip's real
//! SPS/PPS/slice headers with the same [`hevc`] parser the decode path uses,
//! feeds the same encoders in [`hevc_bcm2712`], tracks POC and DPB slots the
//! way the driver must, and prints a frame table ready to drop in as
//! `modules/app/hevc_decode/clip.rs`.
//!
//! Its correctness check is that it reproduces the **already-certified** 64x64
//! constants byte-identically (`--expect-certified`). Those values are proven
//! on silicon against an ffmpeg reference decode, so agreement is evidence the
//! derivation is right, not merely self-consistent.
//!
//! ```text
//! cargo run --bin precompute -- clip.hevc
//! ```
//!
//! **Clean-room Team B.** Every encoder it calls cites released spec r01; no
//! GPL source was read.

use spectra_hevc_tools::hevc;
use spectra_hevc_tools::hevc_bcm2712 as hw;
use spectra_hevc_tools::hevc_ctx_init;
use spectra_hevc_tools::hevc_detile::Surface;
use spectra_hevc_tools::hevc_dpb::{Dpb, DpbSlot};
use spectra_hevc_tools::hevc_slice_msg as msg;

/// The constants proven on silicon for the 64x64 single-CTB clip, as a
/// regression anchor for the derivation itself.
const CERTIFIED_64: Certified = Certified {
    seq_a: 0x0088_5263,
    seq_b: 0x0001_0000,
    pic: 0x0000_0095,
    seg_cfg: 0x4080_2000,
    qp: 25,
    ctb_last_col: 0,
    ctb_last_row: 0,
    p2_pic_cfg: 0x0002_1888,
    p2_pic_size: 0x0040_0040,
};

struct Certified {
    seq_a: u32,
    seq_b: u32,
    pic: u32,
    seg_cfg: u32,
    qp: i32,
    ctb_last_col: u16,
    ctb_last_row: u16,
    p2_pic_cfg: u32,
    p2_pic_size: u32,
}

/// Everything derived for one coded picture.
struct Frame {
    nal: Vec<u8>,
    data_off: u32,
    data_len: u32,
    pic: u32,
    seg_cfg: u32,
    qp: i32,
    ctx: Vec<u32>,
    msgs: Vec<u16>,
    poc: i32,
    p2_pic_cfg: u32,
    out_slot: u8,
    ref_slots: Vec<u8>,
    slice_type: hevc::SliceType,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| {
        eprintln!("usage: hevc_precompute <clip.hevc> [--expect-certified]");
        std::process::exit(2);
    });
    let expect_certified = args.any(|a| a == "--expect-certified");

    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("precompute: {path}: {e}");
        std::process::exit(1);
    });

    let mut sps: Option<hevc::Sps> = None;
    let mut pps: Option<hevc::Pps> = None;
    let mut rps = hevc::RpsTable::default();
    let mut poc_tracker = hevc::PocTracker::new();
    let mut dpb = Dpb::new();
    let mut frames: Vec<Frame> = Vec::new();

    for (start, end) in split_annexb(&bytes) {
        let nal = &bytes[start..end];
        let Some(h) = hevc::parse_nal_header(nal) else {
            continue;
        };
        match h.nal_type {
            hevc::NalType::Sps => sps = hevc::parse_sps_with_rps(nal, &mut rps),
            hevc::NalType::Pps => pps = hevc::parse_pps(nal),
            t if is_slice(t) => {
                let (Some(sps), Some(pps)) = (sps.as_ref(), pps.as_ref()) else {
                    eprintln!("precompute: slice before SPS/PPS");
                    std::process::exit(1);
                };
                let f = derive_frame(
                    nal,
                    t,
                    h.temporal_id,
                    sps,
                    pps,
                    &rps,
                    &mut poc_tracker,
                    &mut dpb,
                );
                frames.push(f);
            }
            _ => {}
        }
    }

    let (Some(sps), Some(pps)) = (sps, pps) else {
        eprintln!("precompute: no SPS/PPS in {path}");
        std::process::exit(1);
    };
    if frames.is_empty() {
        eprintln!("precompute: no slice NAL in {path}");
        std::process::exit(1);
    }

    // ─── sequence-level ────────────────────────────────────────────────────
    let seq_a = hw::p1_seq_a(&sps);
    let seq_b = hw::p1_seq_b(&sps);
    let ctb = 1u32 << sps.log2_ctb_size;
    let p2_pic_size = hw::p2_pic_size(sps.width as u16, sps.height as u16);
    let surface = Surface::new(sps.width, sps.height, sps.bit_depth_luma);
    let pu_bytes = hw::pu_stream_size(u64::from(sps.width), u64::from(sps.height));
    let coeff_bytes = hw::coeff_stream_size(u64::from(sps.width), u64::from(sps.height));

    if expect_certified {
        let f = &frames[0];
        check_certified(
            &CERTIFIED_64,
            seq_a,
            seq_b,
            f.pic,
            f.seg_cfg,
            f.qp,
            (sps.ctb_width - 1) as u16,
            (sps.ctb_height - 1) as u16,
            f.p2_pic_cfg,
            p2_pic_size,
        );
        println!("precompute: derivation MATCHES the certified 64x64 constants");
        return;
    }

    // Both intermediate streams are one fixed span per CTB row [r01 §4.2]:
    // span = align_down(bytes / rows, 64), programmed in 64-byte units.
    let rows = u64::from(sps.ctb_height);
    let pu_stride = hw::stride_units(pu_bytes / rows / 64 * 64);
    let coeff_stride = hw::stride_units(coeff_bytes / rows / 64 * 64);

    // How many distinct output surfaces the driver must allocate.
    let slots = frames.iter().map(|f| f.out_slot).max().unwrap_or(0) + 1;

    // ─── emit ──────────────────────────────────────────────────────────────
    let file = std::path::Path::new(&path)
        .file_name()
        .map_or_else(|| path.clone(), |f| f.to_string_lossy().into_owned());
    println!("//! Register constants for the embedded test clip `{file}`.");
    println!("//!");
    println!("//! GENERATED — do not hand-edit. Regenerate with:");
    println!("//!");
    println!("//! ```text");
    println!("//! cargo run --bin precompute -- <clip.hevc>   (tools/hevc)");
    println!("//! ```");
    println!("//!");
    println!(
        "//! SPS {}x{}, {}-bit, CTB {} ({}x{} CTBs), WPP {}, tiles {}.",
        sps.width,
        sps.height,
        sps.bit_depth_luma,
        ctb,
        sps.ctb_width,
        sps.ctb_height,
        pps.entropy_coding_sync_enabled,
        pps.tiles_enabled,
    );
    println!("//!");
    println!("//! {} coded picture(s):", frames.len());
    for (i, f) in frames.iter().enumerate() {
        println!(
            "//!   {i}: {:?} POC {} QP {} -> slot {}, refs {:?}",
            f.slice_type, f.poc, f.qp, f.out_slot, f.ref_slots
        );
    }
    println!("//!");
    println!("//! The derivation is proven by reproducing the silicon-certified 64x64");
    println!("//! constants (`--expect-certified`).");
    println!();
    println!("// ─── geometry ──────────────────────────────────────────────────────────────");
    println!("pub const WIDTH: u32 = {};", sps.width);
    println!("pub const HEIGHT: u32 = {};", sps.height);
    println!("pub const BIT_DEPTH: u8 = {};", sps.bit_depth_luma);
    println!("pub const CTB_SIZE: u32 = {ctb};");
    println!("pub const CTB_LAST_COL: u16 = {};", sps.ctb_width - 1);
    println!("pub const CTB_LAST_ROW: u16 = {};", sps.ctb_height - 1);
    println!("pub const CTB_COLS: u16 = {};", sps.ctb_width);
    println!("pub const CTB_ROWS: u16 = {};", sps.ctb_height);
    println!("/// Wavefront parallel processing — selects the §6.8 assembler.");
    println!("pub const WPP: bool = {};", pps.entropy_coding_sync_enabled);
    println!("/// Distinct output surfaces (DPB slots) the driver must allocate.");
    println!("pub const SLOTS: usize = {slots};");
    println!("/// Largest slice NAL, so the driver can size its bitstream region without");
    println!("/// reading a static in const context.");
    println!(
        "pub const MAX_NAL: u32 = {};",
        frames.iter().map(|f| f.nal.len()).max().unwrap_or(0)
    );
    println!();
    println!("// ─── sequence-level phase 1 ────────────────────────────────────────────────");
    println!("pub const SEQ_A: u32 = {seq_a:#010x};");
    println!("pub const SEQ_B: u32 = {seq_b:#010x};");
    println!();
    println!("// ─── intermediate streams [§4, §4.2] ───────────────────────────────────────");
    println!("pub const PU_BYTES: u32 = {pu_bytes};");
    println!("pub const COEFF_BYTES: u32 = {coeff_bytes};");
    println!("/// One span per CTB row, in 64-byte units.");
    println!("pub const PU_STRIDE: u32 = {pu_stride};");
    println!("pub const COEFF_STRIDE: u32 = {coeff_stride};");
    println!();
    println!("// ─── output surface [§8, §10] ──────────────────────────────────────────────");
    println!("pub const P2_PIC_SIZE_V: u32 = {p2_pic_size:#010x};");
    println!(
        "pub const OUT_Y_STRIDE: u32 = {};",
        surface.luma_stride_reg()
    );
    println!(
        "pub const OUT_C_STRIDE: u32 = {};",
        surface.chroma_stride_reg()
    );
    println!(
        "pub const OUT_Y_BYTES: u32 = {};",
        surface.luma_plane_bytes()
    );
    println!(
        "pub const OUT_C_BYTES: u32 = {};",
        surface.chroma_plane_bytes()
    );
    println!("/// Picture height in CTB rows — the write that launches phase 2.");
    println!("pub const P2_ROWS_V: u32 = {};", sps.ctb_height);
    println!();
    emit_frame_type();
    println!();

    for (i, f) in frames.iter().enumerate() {
        println!("// ─── picture {i}: {:?}, POC {} ───", f.slice_type, f.poc);
        print_bytes(&format!("NAL{i}"), &f.nal);
        print_words(&format!("CTX{i}"), &f.ctx);
        print_msgs(&format!("MSGS{i}"), &f.msgs);
        println!(
            "static REFS{i}: [u8; {}] = [{}];",
            f.ref_slots.len(),
            f.ref_slots
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!();
    }

    // Per-frame slices are reached through FUNCTIONS, not `&'static` fields in
    // the table. MEASURED on silicon: with the slices stored as fields, the
    // driver read a correct LENGTH (it lives inline in the fat pointer) but a
    // garbage ADDRESS — `ctx[0]` came back 0xaa0203f4 instead of 0x2505030e,
    // and phase 1 stopped mid-picture reporting an undecodable bitstream. A
    // `&STATIC` taken inside a function is PC-relative and relocates correctly
    // in a PIC module, which is the shape the single-clip driver always used.
    println!("/// The slice NAL for picture `i`.");
    println!("///");
    println!("/// Deliberately a function: see the note in this file's generator about");
    println!("/// static-to-static pointers not relocating in a PIC module.");
    emit_accessor("nal", "u8", "NAL", frames.len());
    println!("/// The CABAC context-init words for picture `i` [§7.1].");
    emit_accessor("ctx", "u32", "CTX", frames.len());
    println!("/// The slice-parameter messages for picture `i` [§7.3].");
    emit_accessor("msgs", "u16", "MSGS", frames.len());
    println!("/// The DPB slots picture `i` references, in L0-then-L1 order.");
    emit_accessor("ref_slots", "u8", "REFS", frames.len());
    println!();
    println!("/// The clip's coded pictures, in decode order.");
    println!("pub static FRAMES: [Frame; {}] = [", frames.len());
    for f in &frames {
        println!("    Frame {{");
        println!("        nal_len: {},", f.nal.len());
        println!("        data_off: {},", f.data_off);
        println!("        data_len: {},", f.data_len);
        println!("        pic: {:#010x},", f.pic);
        println!("        seg_cfg: {:#010x},", f.seg_cfg);
        println!("        qp: {},", f.qp);
        println!("        poc: {},", f.poc);
        println!("        p2_pic_cfg: {:#010x},", f.p2_pic_cfg);
        println!("        out_slot: {},", f.out_slot);
        println!("    }},");
    }
    println!("];");
}

/// The frame-table row type, emitted into the generated file so the driver and
/// the host tools agree on it without a shared crate (the driver is a PIC
/// module that path-mounts its dependencies).
fn emit_frame_type() {
    println!("/// One coded picture's phase-1 and phase-2 parameters.");
    println!("pub struct Frame {{");
    println!("    /// The slice NAL, embedded so the module is self-contained (a PIC");
    println!("    /// module has no filesystem, and fixtures/ is shadow-tracked).");
    println!("    /// Length only — the BYTES come from `nal()`. See the note there.");
    println!("    pub nal_len: u32,");
    println!("    /// Raw byte offset of the slice data within the NAL, checked free of");
    println!("    /// emulation-prevention bytes.");
    println!("    pub data_off: u32,");
    println!("    /// Slice data length, plus one byte for the RBSP stop bit's byte.");
    println!("    pub data_len: u32,");
    println!("    pub pic: u32,");
    println!("    pub seg_cfg: u32,");
    println!("    pub qp: i32,");
    println!("    pub poc: i32,");
    println!("    pub p2_pic_cfg: u32,");
    println!("    /// DPB slot this picture's output surface occupies.");
    println!("    pub out_slot: u8,");
    println!("}}");
}

#[allow(
    clippy::too_many_arguments,
    reason = "one-shot table-precompute example; the argument list mirrors the decoder call it certifies"
)]
fn derive_frame(
    nal: &[u8],
    nal_type: hevc::NalType,
    temporal_id: u8,
    sps: &hevc::Sps,
    pps: &hevc::Pps,
    rps: &hevc::RpsTable,
    poc_tracker: &mut hevc::PocTracker,
    dpb: &mut Dpb,
) -> Frame {
    let mut weights = hevc::PredWeightTable::default();
    let Some(sh) = hevc::parse_slice_header_weighted(nal, sps, pps, Some(rps), &mut weights) else {
        eprintln!("precompute: slice header parse failed");
        std::process::exit(1);
    };
    if !sh.full {
        eprintln!("precompute: slice header did not parse to byte_alignment");
        std::process::exit(1);
    }
    if !sh.first_slice_in_pic {
        eprintln!("precompute: multi-slice pictures are not supported yet");
        std::process::exit(1);
    }
    let slice_type = sh.slice_type.unwrap_or(hevc::SliceType::I);
    let poc = poc_tracker.compute(nal_type, sh.poc_lsb, sps.log2_max_poc_lsb, temporal_id);

    // The parser does not capture the pred_weight_table's weights, and §7.3
    // requires six extra messages per reference when weighted prediction
    // applies. Emitting the reference without them would program a weighted
    // slice as unweighted — a wrong picture, not an error. Refuse instead.
    let weighted = match slice_type {
        hevc::SliceType::P => pps.weighted_pred,
        hevc::SliceType::B => pps.weighted_bipred,
        hevc::SliceType::I => false,
    };
    // Temporal MVP needs collocated-motion-vector buffers written on one
    // picture and read on the next (§10 P2_MV_WBASE/P2_MV_RBASE). Until the
    // driver allocates them, programming the picture without them would
    // silently drop a merge candidate.
    if sh.temporal_mvp_enabled {
        eprintln!(
            "precompute: temporal MVP is enabled for this slice; collocated-MV \
             buffers are not modelled yet"
        );
        std::process::exit(1);
    }

    // The slice-data offset the parser reports is RBSP-relative — it counts
    // de-emulated bytes. The phase-1 bitstream feed is programmed with a RAW
    // byte offset, so the two agree only if the header carried no
    // emulation-prevention byte. Check rather than assume: the failure mode is
    // a silently misaligned bitstream rather than an error.
    let data_off = sh.data_byte_offset + 2;
    let header_raw = &nal[..(data_off as usize).min(nal.len())];
    if header_raw.windows(3).any(|w| w == [0, 0, 3]) {
        eprintln!(
            "precompute: slice header contains an emulation-prevention byte; the \
             RBSP-relative data_byte_offset is not the raw offset the hardware needs"
        );
        std::process::exit(1);
    }

    // Reference lists, as POCs, then as DPB slots.
    let (l0, l1) = match sh.rps {
        Some(r) => {
            let sets = hevc::derive_ref_pic_sets(poc, &r);
            let (a, b) = hevc::init_ref_pic_lists(
                &sets,
                slice_type,
                sh.num_ref_idx_l0_active,
                sh.num_ref_idx_l1_active,
            );
            (a.as_slice().to_vec(), b.as_slice().to_vec())
        }
        None => (Vec::new(), Vec::new()),
    };
    let mut ref_slots = Vec::new();
    let mut ref_pocs = Vec::new();
    for &p in l0.iter().chain(l1.iter()) {
        let Some(slot) = dpb.slot_of_poc(p) else {
            eprintln!("precompute: reference POC {p} is not in the DPB");
            std::process::exit(1);
        };
        ref_slots.push(slot);
        ref_pocs.push(p);
    }

    // ── phase-1 registers ──────────────────────────────────────────────────
    let pic = hw::p1_pic(
        pps,
        sps.log2_ctb_size,
        sh.slice_cb_qp_offset,
        sh.slice_cr_qp_offset,
    );
    let qp = hw::p1_qp(i32::from(sh.slice_qp), sps.bit_depth_luma);
    let ctb = 1u32 << sps.log2_ctb_size;
    let last_w = sps.width - (sps.ctb_width - 1) * ctb;
    let last_h = sps.height - (sps.ctb_height - 1) * ctb;
    let seg_cfg = hw::p1_seg_cfg(&hw::SegConfig {
        slice_type,
        max_merge_cand: sh.max_merge_cand,
        active_refs_l0: sh.num_ref_idx_l0_active,
        active_refs_l1: sh.num_ref_idx_l1_active,
        sao_luma: sh.sao_luma,
        sao_chroma: sh.sao_chroma,
        mvd_l1_zero: sh.mvd_l1_zero,
        last_ctb_width: last_w as u8,
        last_ctb_height: last_h as u8,
    });
    let ctx = hevc_ctx_init::to_words(&hevc_ctx_init::build(
        ctx_table_index(slice_type, sh.cabac_init),
        qp,
    ))
    .to_vec();

    // ── slice messages [§7.3] ──────────────────────────────────────────────
    // collocated-from-L0 is 1 when temporal MVP is OFF, or the slice is not B,
    // or the slice signals its collocated reference in L0 — NOT simply the
    // parsed syntax element, which is absent (and so false) whenever temporal
    // MVP is off.
    let collocated_from_l0 =
        !sh.temporal_mvp_enabled || slice_type != hevc::SliceType::B || sh.collocated_from_l0;
    let mut msgs = vec![msg::slice_command_word(
        slice_type,
        sh.num_ref_idx_l0_active,
        sh.num_ref_idx_l1_active,
        msg::no_backward_prediction(poc, &ref_pocs),
        sh.max_merge_cand,
        collocated_from_l0,
    )];
    // L0 entries come first, then L1; the weight table is indexed the same way,
    // so the list index has to be tracked rather than inferred from position.
    let l0_len = l0.len();
    for (n, (&slot, &rpoc)) in ref_slots.iter().zip(ref_pocs.iter()).enumerate() {
        let (list, idx) = if n < l0_len { (0, n) } else { (1, n - l0_len) };
        msgs.push(msg::ref_descriptor(slot, false, weighted));
        msgs.push((rpoc & 0xFFFF) as u16);
        if weighted {
            msgs.extend_from_slice(&msg::weight_words(
                weights.luma_denom,
                weights.luma_weight[list][idx],
                weights.luma_offset[list][idx],
                weights.chroma_denom,
                weights.chroma_weight[list][idx],
                weights.chroma_offset[list][idx],
            ));
        }
    }
    msgs.push(msg::deblocking_word(
        sh.beta_offset_div2,
        sh.tc_offset_div2,
        sh.deblocking_disabled,
        sh.loop_filter_across_slices,
        pps.loop_filter_across_tiles_enabled,
    ));
    msgs.push(msg::chroma_qp_word(
        sh.slice_cb_qp_offset,
        sh.slice_cr_qp_offset,
    ));

    // ── phase-2 ────────────────────────────────────────────────────────────
    let p2_pic_cfg = hw::p2_pic_cfg(sps, pps, false, false);

    // Allocate this picture's output slot. An IDR clears the DPB first: it
    // starts a new coded video sequence and nothing before it may be
    // referenced.
    if matches!(nal_type, hevc::NalType::IdrWRadl | hevc::NalType::IdrNLp) {
        dpb.mark_unused_except(&[]);
        dpb.evict_unused();
    }
    // Only POC and the reference marking matter here — the addresses are the
    // driver's to fill, since it owns the DMA allocation.
    let entry = DpbSlot {
        poc,
        used_for_ref: true,
        ..DpbSlot::default()
    };
    let Some(out_slot) = dpb.allocate(entry) else {
        eprintln!("precompute: DPB full");
        std::process::exit(1);
    };

    Frame {
        nal: nal.to_vec(),
        data_off,
        data_len: nal.len() as u32 - data_off + 1,
        pic,
        seg_cfg,
        qp,
        ctx,
        msgs,
        poc,
        p2_pic_cfg,
        out_slot,
        ref_slots,
        slice_type,
    }
}

/// True for the NAL types that carry a coded slice.
fn is_slice(t: hevc::NalType) -> bool {
    use hevc::NalType as N;
    matches!(
        t,
        N::IdrWRadl
            | N::IdrNLp
            | N::Cra
            | N::BlaWLp
            | N::BlaWRadl
            | N::BlaNLp
            | N::TrailN
            | N::TrailR
    )
}

/// CABAC init table index [§7.1]: I always uses table 0; P/B select between
/// tables 1 and 2 by `cabac_init_flag`.
fn ctx_table_index(slice_type: hevc::SliceType, cabac_init: bool) -> u8 {
    match slice_type {
        hevc::SliceType::I => 0,
        hevc::SliceType::P => {
            if cabac_init {
                2
            } else {
                1
            }
        }
        hevc::SliceType::B => {
            if cabac_init {
                1
            } else {
                2
            }
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one-shot table-precompute example; the argument list mirrors the decoder call it certifies"
)]
fn check_certified(
    c: &Certified,
    seq_a: u32,
    seq_b: u32,
    pic: u32,
    seg_cfg: u32,
    qp: i32,
    last_col: u16,
    last_row: u16,
    p2_pic_cfg: u32,
    p2_pic_size: u32,
) {
    let mut bad = 0;
    let mut cmp32 = |name: &str, got: u32, want: u32| {
        if got != want {
            eprintln!("MISMATCH {name}: derived {got:#010x}, certified {want:#010x}");
            bad += 1;
        }
    };
    cmp32("SEQ_A", seq_a, c.seq_a);
    cmp32("SEQ_B", seq_b, c.seq_b);
    cmp32("PIC", pic, c.pic);
    cmp32("SEG_CFG", seg_cfg, c.seg_cfg);
    cmp32("P2_PIC_CFG", p2_pic_cfg, c.p2_pic_cfg);
    cmp32("P2_PIC_SIZE", p2_pic_size, c.p2_pic_size);
    cmp32("QP", qp as u32, c.qp as u32);
    cmp32(
        "CTB_LAST_COL",
        u32::from(last_col),
        u32::from(c.ctb_last_col),
    );
    cmp32(
        "CTB_LAST_ROW",
        u32::from(last_row),
        u32::from(c.ctb_last_row),
    );
    if bad != 0 {
        eprintln!("precompute: {bad} constant(s) disagree with the certified clip");
        std::process::exit(1);
    }
}

fn print_words(name: &str, words: &[u32]) {
    println!("static {name}: [u32; {}] = [", words.len());
    // 8 per line is what rustfmt settles on at the default 100-column width;
    // matching it keeps regeneration a no-op under `cargo fmt --check`, which
    // reaches this file through the example that mounts it by path.
    for chunk in words.chunks(8) {
        let row: Vec<String> = chunk.iter().map(|w| format!("{w:#010x}")).collect();
        println!("    {},", row.join(", "));
    }
    println!("];");
}

/// Slice-message words. Short arrays fit on one line, long ones do not: an
/// 11-word `MSGS` (the weighted-prediction case) is 108 columns and rustfmt
/// splits it. Emit it split ourselves so regeneration stays a no-op under
/// `cargo fmt --check` — the same reason `print_words` packs 8 per line.
fn print_msgs(name: &str, msgs: &[u16]) {
    let row: Vec<String> = msgs.iter().map(|m| format!("{m:#06x}")).collect();
    let one_line = format!(
        "static {name}: [u16; {}] = [{}];",
        msgs.len(),
        row.join(", ")
    );
    if one_line.len() <= 100 {
        println!("{one_line}");
        return;
    }
    println!("static {name}: [u16; {}] = [", msgs.len());
    for chunk in row.chunks(11) {
        println!("    {},", chunk.join(", "));
    }
    println!("];");
}

fn print_bytes(name: &str, bytes: &[u8]) {
    println!("static {name}: [u8; {}] = [", bytes.len());
    for chunk in bytes.chunks(16) {
        let row: Vec<String> = chunk.iter().map(|b| format!("{b:#04x}")).collect();
        println!("    {},", row.join(", "));
    }
    println!("];");
}

/// Split an Annex-B stream into NAL payload ranges (start code excluded).
fn split_annexb(data: &[u8]) -> Vec<(usize, usize)> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &s) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(data.len(), |&next| next - 3);
        // A 4-byte start code leaves a trailing zero on the previous NAL.
        while end > s && data[end - 1] == 0 {
            end -= 1;
        }
        out.push((s, end));
    }
    out
}

/// Emit a `fn name(i: usize) -> &'static [ty]` that matches an index onto the
/// per-frame statics. Out-of-range falls back to picture 0 rather than
/// panicking: a panic in a PIC module is unrecoverable and drags `core::fmt`
/// into the image.
fn emit_accessor(name: &str, ty: &str, stem: &str, n: usize) {
    println!("#[must_use]");
    println!("pub fn {name}(i: usize) -> &'static [{ty}] {{");
    println!("    match i {{");
    for k in 1..n {
        println!("        {k} => &{stem}{k},");
    }
    println!("        _ => &{stem}0,");
    println!("    }}");
    println!("}}");
}

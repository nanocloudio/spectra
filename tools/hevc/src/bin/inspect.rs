//! Dump what the parser makes of a clip: every parameter set and slice header.
//!
//! Before a clip can be turned into driver constants it has to be known what it
//! actually asks of the hardware — whether temporal MVP is on, whether weighted
//! prediction applies, how many references are active, what the RPS resolves to.
//! Guessing those is how a decoder ends up programming a plausible but wrong
//! register file.
//!
//! ```text
//! cargo run --bin inspect -- <clip.hevc>
//! ```

use spectra_hevc_tools::hevc;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: hevc_inspect <clip.hevc>");
        std::process::exit(2);
    });
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("inspect: {path}: {e}");
        std::process::exit(1);
    });

    let mut sps: Option<hevc::Sps> = None;
    let mut pps: Option<hevc::Pps> = None;
    let mut rps = hevc::RpsTable::default();
    let mut poc_tracker = hevc::PocTracker::new();
    let mut frame = 0usize;

    for (start, end) in split_annexb(&bytes) {
        let nal = &bytes[start..end];
        let Some(h) = hevc::parse_nal_header(nal) else {
            continue;
        };
        match h.nal_type {
            hevc::NalType::Vps => println!("VPS  @{start} ({} bytes)", nal.len()),
            hevc::NalType::Sps => {
                let s = hevc::parse_sps_with_rps(nal, &mut rps);
                if let Some(s) = s {
                    println!("SPS  @{start} ({} bytes)", nal.len());
                    println!(
                        "     {}x{} coded, {}x{} cropped",
                        s.width, s.height, s.cropped_width, s.cropped_height
                    );
                    println!(
                        "     bit depth {}/{}, chroma_format_idc {}",
                        s.bit_depth_luma, s.bit_depth_chroma, s.chroma_format_idc
                    );
                    println!(
                        "     CTB {} ({}x{} CTBs), log2_max_poc_lsb {}",
                        1 << s.log2_ctb_size,
                        s.ctb_width,
                        s.ctb_height,
                        s.log2_max_poc_lsb
                    );
                    println!("     temporal_mvp_enabled {}", s.temporal_mvp_enabled);
                    println!(
                        "     amp {}  sao {}  pcm {}  scaling_list {}",
                        s.amp_enabled, s.sao_enabled, s.pcm_enabled, s.scaling_list_enabled
                    );
                    println!(
                        "     strong_intra_smoothing {}",
                        s.strong_intra_smoothing_enabled
                    );
                    println!(
                        "     num_short_term_ref_pic_sets {}",
                        s.num_short_term_ref_pic_sets
                    );
                    println!(
                        "     max_dec_pic_buffering {}  reorder {}",
                        s.max_dec_pic_buffering, s.max_num_reorder_pics
                    );
                    sps = Some(s);
                }
            }
            hevc::NalType::Pps => {
                if let Some(p) = hevc::parse_pps(nal) {
                    println!("PPS  @{start} ({} bytes)", nal.len());
                    println!(
                        "     init_qp {}  cb/cr qp offset {}/{}",
                        p.init_qp, p.cb_qp_offset, p.cr_qp_offset
                    );
                    println!(
                        "     tiles {}  entropy_coding_sync {}",
                        p.tiles_enabled, p.entropy_coding_sync_enabled
                    );
                    println!(
                        "     weighted_pred {}  weighted_bipred {}",
                        p.weighted_pred, p.weighted_bipred
                    );
                    println!(
                        "     num_ref_idx_l0/l1_default_active {}/{}",
                        p.num_ref_idx_l0_default_active, p.num_ref_idx_l1_default_active
                    );
                    println!(
                        "     lists_modification_present {}",
                        p.lists_modification_present
                    );
                    println!(
                        "     cu_qp_delta_enabled {}  transform_skip {}",
                        p.cu_qp_delta_enabled, p.transform_skip_enabled
                    );
                    println!(
                        "     log2_parallel_merge_level {}",
                        p.log2_parallel_merge_level
                    );
                    println!(
                        "     sign_data_hiding {}  constrained_intra_pred {}",
                        p.sign_data_hiding_enabled, p.constrained_intra_pred
                    );
                    pps = Some(p);
                }
            }
            t if is_slice(t) => {
                let (Some(s), Some(p)) = (sps.as_ref(), pps.as_ref()) else {
                    println!("slice @{start}: no SPS/PPS yet");
                    continue;
                };
                let Some(sh) = hevc::parse_slice_header_full(nal, s, p, Some(&rps)) else {
                    println!("slice @{start}: HEADER PARSE FAILED");
                    continue;
                };
                let poc = poc_tracker.compute(t, sh.poc_lsb, s.log2_max_poc_lsb, h.temporal_id);
                println!(
                    "SLICE @{start} ({} bytes)  frame {frame}  nal {t:?}",
                    nal.len()
                );
                println!(
                    "     type {:?}  POC {poc} (lsb {})  qp {}",
                    sh.slice_type, sh.poc_lsb, sh.slice_qp
                );
                println!(
                    "     first_in_pic {}  dependent {}",
                    sh.first_slice_in_pic, sh.dependent_slice_segment
                );
                println!(
                    "     active refs L0/L1 {}/{}",
                    sh.num_ref_idx_l0_active, sh.num_ref_idx_l1_active
                );
                println!(
                    "     temporal_mvp {}  collocated_from_l0 {}  collocated_ref_idx {}",
                    sh.temporal_mvp_enabled, sh.collocated_from_l0, sh.collocated_ref_idx
                );
                println!(
                    "     max_merge_cand {}  cabac_init {}  mvd_l1_zero {}",
                    sh.max_merge_cand, sh.cabac_init, sh.mvd_l1_zero
                );
                println!(
                    "     sao luma/chroma {}/{}  weighted_pred {}",
                    sh.sao_luma, sh.sao_chroma, sh.weighted_pred
                );
                println!(
                    "     deblocking_disabled {}  beta/tc {}/{}",
                    sh.deblocking_disabled, sh.beta_offset_div2, sh.tc_offset_div2
                );
                println!(
                    "     entry points {}  data_byte_offset {}",
                    sh.num_entry_point_offsets, sh.data_byte_offset
                );
                if let Some(r) = sh.rps {
                    let sets = hevc::derive_ref_pic_sets(poc, &r);
                    println!(
                        "     RPS: before {:?}  after {:?}  NumPicTotalCurr {}",
                        sets.st_curr_before(),
                        sets.st_curr_after(),
                        sets.num_pic_total_curr()
                    );
                    if let Some(st) = sh.slice_type {
                        let (l0, l1) = hevc::init_ref_pic_lists(
                            &sets,
                            st,
                            sh.num_ref_idx_l0_active,
                            sh.num_ref_idx_l1_active,
                        );
                        println!(
                            "     L0 POCs {:?}  L1 POCs {:?}",
                            l0.as_slice(),
                            l1.as_slice()
                        );
                    }
                } else {
                    println!("     RPS: none (IDR references nothing)");
                }
                frame += 1;
            }
            _ => {}
        }
    }
}

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
        while end > s && data[end - 1] == 0 {
            end -= 1;
        }
        out.push((s, end));
    }
    out
}

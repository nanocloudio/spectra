//! Dump the phase-1 command list a clip's parameters produce, decoded.
//!
//! The bare-metal driver reports only `P1_LIST_DONE` — how many commands the
//! engine applied. Turning "it stopped after N" into "it stopped on THIS
//! command" needs the list itself, and the list is built on-device from
//! constants. This runs the same assembler on the host and prints it.
//!
//! ```text
//! cargo run --bin cmdlist -- [stop_at]
//! ```
//!
//! `stop_at` marks a command index, so a `LIST_DONE` value read off the rig can
//! be pointed straight at the command that did not run.
//!
//! It reads the driver's own generated `clip.rs`, so the list dumped here is by
//! construction the list the board was given — not a re-derivation that could
//! drift from it.

use spectra_hevc_tools::hevc_bcm2712 as hw;
use spectra_hevc_tools::hevc_phase1::{assemble_intra, assemble_intra_wpp, Phase1Intra};

// The active clip, exactly as the bare-metal driver embeds it. Mounted by path
// rather than `include!`d so its inner doc comments stay legal.
#[allow(
    dead_code,
    reason = "the clip carries the full driver constant set; this tool needs only the phase-1 subset"
)]
#[path = "../../../../modules/app/hevc_decode/clip.rs"]
mod clip;

fn main() {
    let num = |n: usize| -> Option<usize> {
        std::env::args().nth(n).and_then(|s| {
            s.strip_prefix("0x")
                .map_or_else(|| s.parse().ok(), |h| usize::from_str_radix(h, 16).ok())
        })
    };
    let frame_idx = num(1).unwrap_or(0);
    let stop_at = num(2);

    let Some(f) = clip::FRAMES.get(frame_idx) else {
        eprintln!(
            "cmdlist: frame {frame_idx} out of range (clip has {})",
            clip::FRAMES.len()
        );
        std::process::exit(1);
    };

    // Addresses are irrelevant to which command is which, so use round numbers
    // that are easy to recognise in the dump.
    let params = Phase1Intra {
        seq_a: clip::SEQ_A,
        seq_b: clip::SEQ_B,
        pic: f.pic,
        seg_cfg: f.seg_cfg,
        qp: f.qp,
        ctb_last_col: clip::CTB_LAST_COL,
        ctb_last_row: clip::CTB_LAST_ROW,
        bs_base: 0x1000,
        bs_len_bytes: f.data_len,
        bs_byte_offset: f.data_off as u8,
        ctx_init_words: clip::ctx(frame_idx),
        slice_msg_words: clip::msgs(frame_idx),
        slice_seq: 0,
        pu_write_base: 0x2000,
        pu_write_stride: clip::PU_STRIDE,
        coeff_write_base: 0x3000,
        coeff_write_stride: clip::COEFF_STRIDE,
    };

    let mut words = Vec::new();
    let count = if clip::WPP {
        assemble_intra_wpp(&params, clip::CTB_COLS, clip::CTB_ROWS, &mut |w| {
            words.push(w)
        })
    } else {
        assemble_intra(&params, &mut |w| words.push(w))
    };

    println!(
        "clip frame {frame_idx}/{}: {} CTB cols x {} rows, WPP {}, POC {}, refs {:?} -> {count} commands",
        clip::FRAMES.len(),
        clip::CTB_COLS,
        clip::CTB_ROWS,
        clip::WPP,
        f.poc,
        clip::ref_slots(frame_idx),
    );
    println!("{:>5}  {:>8}  {:>10}  meaning", "idx", "target", "value");
    for (i, &w) in words.iter().enumerate() {
        let target = (w & 0xFFFF) as u32;
        let value = (w >> 32) as u32;
        let mark = match stop_at {
            Some(n) if i == n => "  <-- FIRST COMMAND NOT APPLIED",
            Some(n) if i + 1 == n => "  <-- last applied",
            _ => "",
        };
        println!(
            "{i:>5}  {target:>#8x}  {value:>#10x}  {}{mark}",
            describe(target, value)
        );
    }
}

/// Name the register a command targets, decoding the fields that matter for
/// reading a WPP sequence.
fn describe(target: u32, value: u32) -> String {
    if (hw::P1_WIN_CABAC_INIT..hw::P1_WIN_CABAC_INIT + 4 * 39).contains(&target) {
        return format!("CABAC_INIT[{}]", (target - hw::P1_WIN_CABAC_INIT) / 4);
    }
    if (hw::P1_WIN_SLICE_MSG..hw::P1_WIN_SLICE_MSG + 64).contains(&target) {
        return format!("SLICE_MSG[{}]", (target - hw::P1_WIN_SLICE_MSG) / 4);
    }
    match target {
        hw::P1_SEQ_A => "SEQ_A".into(),
        hw::P1_SEQ_B => "SEQ_B".into(),
        hw::P1_PIC => "PIC".into(),
        hw::P1_SEG_CFG => "SEG_CFG".into(),
        hw::P1_QP => "QP".into(),
        hw::P1_BS_BASE => "BS_BASE".into(),
        hw::P1_BS_LEN => "BS_LEN".into(),
        hw::P1_BS_CTRL => "BS_CTRL".into(),
        hw::P1_PU_WBASE => "PU_WBASE".into(),
        hw::P1_PU_WSTRIDE => "PU_WSTRIDE".into(),
        hw::P1_COEFF_WBASE => "COEFF_WBASE".into(),
        hw::P1_COEFF_WSTRIDE => "COEFF_WSTRIDE".into(),
        hw::P1_MSG_CTRL => "MSG_CTRL (commit)".into(),
        hw::P1_RGN_FIRST => format!("RGN_FIRST  ({}, {})", value & 0xFFFF, value >> 16),
        hw::P1_RGN_LAST => format!("RGN_LAST   ({}, {})", value & 0xFFFF, value >> 16),
        hw::P1_RGN_LAST_INDEP => format!("RGN_LAST_INDEP ({}, {})", value & 0xFFFF, value >> 16),
        hw::P1_SEG_FIRST => format!("SEG_FIRST  ({}, {})", value & 0xFFFF, value >> 16),
        hw::P1_RUN_FROM => format!("RUN_FROM   ({}, {})", value & 0xFFFF, value >> 16),
        hw::P1_RUN_MODE => format!(
            "RUN_MODE   low={:#x} last_col={} last_row={} resume={}",
            value & 0xFFFF,
            (value >> 17) & 1,
            (value >> 18) & 1,
            (value >> 16) & 1
        ),
        hw::P1_CHECKPOINT => format!(
            "CHECKPOINT reason={} at ({}, {})",
            value & 0x1F,
            (value >> 5) & 0x1FFF,
            (value >> 18) & 0x3FFF
        ),
        hw::P1_CTX_XFER => match value {
            hw::P1_CTX_SAVE => "CTX_XFER   SAVE".into(),
            hw::P1_CTX_RESTORE => "CTX_XFER   RESTORE".into(),
            v => format!("CTX_XFER   {v:#x}"),
        },
        t => format!("<target {t:#x}>"),
    }
}

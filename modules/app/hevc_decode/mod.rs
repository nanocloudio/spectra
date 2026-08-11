//! BCM2712 HEVC phase-1 execution probe — bare-metal bring-up rung 3.
//!
//! After the version read (MMIO + clock proven) and the DMA round-trip
//! (allocator + cache path proven), this is the first attempt to make the
//! **entropy engine actually run**: stage a real single-slice intra HEVC NAL in
//! DMA memory, assemble the phase-1 command list with the same
//! shared-core assembler the driver will use, launch, and poll
//! `P1_LIST_DONE` [r01 §5].
//!
//! Success criterion, straight from the spec: `P1_LIST_DONE == P1_LIST_COUNT`
//! means the engine fetched and applied every command. A short count means it
//! stopped early, and `P1_CHECKPOINT` bits 3/4 say whether an intermediate
//! stream overflowed (enlarge and retry) or the bitstream was undecodable
//! [r01 §5, §6.4]. Polling sidesteps needing an IRQ binding at this rung.
//!
//! The register values, CABAC context array and slice messages are
//! **precomputed on the host** from the embedded clip and checked in as
//! constants, so a failure here isolates to the hardware sequence rather than
//! to on-device parsing. The command list itself is built live by
//! `hevc_phase1::assemble_intra` — the real assembler, not a copy.
//!
//! **Clean-room Team B.** Every register offset and sequence rule cites the
//! released spec; no GPL source was read.

#![no_std]
#![allow(
    dead_code,
    unused_imports,
    unreachable_patterns,
    reason = "PIC build path-mounts the SDK and the modules/common HEVC cores; each compile sees the full surface and uses a subset"
)]

use core::ffi::c_void;

#[path = "../../../target/fluxor/fluxor-abi/sdk/abi.rs"]
mod abi;
use abi::SyscallTable;

include!("../../../target/fluxor/fluxor-abi/sdk/runtime.rs");

// The real decode logic, mounted from modules/common by path — the same
// pattern the codec module uses for mkv_demux, and the dependency set the
// production driver needs. `crate::` paths inside these resolve to this
// module's root.
#[path = "../../common/bitreader.rs"]
mod bitreader;
#[path = "../../common/hevc.rs"]
mod hevc;
#[path = "../../common/hevc_bcm2712.rs"]
mod hevc_bcm2712;
#[path = "../../common/hevc_detile.rs"]
mod hevc_detile;
#[path = "../../common/hevc_phase1.rs"]
mod hevc_phase1;
#[path = "../../common/hevc_program.rs"]
mod hevc_program;
#[path = "../../common/hevc_slice_msg.rs"]
mod hevc_slice_msg;

use hevc_bcm2712 as hw;
use hevc_detile::{luma_offset_8bit, Surface};
use hevc_phase1::{assemble_intra, assemble_intra_wpp, launch, Phase1Intra};
use hevc_program::{program_phase2, Phase2Launch, RefPic};

// ─── mmio_dma opcodes [Fluxor SDK platform/bcm2712/mmio_dma.rs] ─────────────
const MMIO_READ32: u32 = 0x0CE4;
const MMIO_WRITE32: u32 = 0x0CE5;
const DMA_ALLOC_CONTIG: u32 = 0x0CE6;
const DMA_FLUSH: u32 = 0x0CEA;
const DMA_INVALIDATE: u32 = 0x0CEB;

/// Per-build marker, echoed in every log line.
///
/// standards/rig.md §4 warns that a green verdict is not proof the DUT ran
/// your code, and this probe was bitten by exactly that: runs booted an image
/// logging a previous revision's format while source, fmod, image and staged
/// kernel were all freshly rebuilt and timestamp-consistent. Without a marker
/// there is no way to tell "my change did nothing" from "my change never
/// shipped". Bump this every build; if the board does not echo the value you
/// just set, you are reading a stale image and nothing else in the line means
/// anything.
const BUILD_NONCE: u32 = 69269;

/// HEVC main register block base [spec §2].
const HEVC_MAIN_BASE: u64 = 0x10_0080_0000;
/// Interrupt-control block base [spec §2]. One register at +0x00 [§3].
const HEVC_IRQ_BASE: u64 = 0x10_0084_0000;
// ─── The embedded clip ──────────────────────────────────────────────────────
//
// Every register constant, the CABAC context-init array and the slice NAL are
// DERIVED from the clip's own bitstream by the host tool
// `cargo run --bin precompute` (tools/hevc), which parses it with
// the same `hevc` parser the decode path uses and feeds the same
// `hevc_bcm2712` encoders. They were hand-derived once; that does not scale
// past one clip, and a mistranscribed `SEG_CFG` does not fail to build, it
// decodes a wrong picture.
//
// The derivation is trusted because it reproduces the already-silicon-certified
// 64x64 constants byte-identically (`--expect-certified`) — agreement with a
// value proven against an ffmpeg reference decode, not mere self-consistency.
mod clip;

use clip::{
    COEFF_BYTES, COEFF_STRIDE, CTB_COLS, CTB_LAST_COL, CTB_LAST_ROW, CTB_ROWS, FRAMES, MAX_NAL,
    OUT_C_BYTES, OUT_C_STRIDE, OUT_Y_BYTES, OUT_Y_STRIDE, P2_ROWS_V, PU_BYTES, PU_STRIDE, SEQ_A,
    SEQ_B, SLOTS,
};

// ─── DMA buffer layout within one contiguous allocation ────────────────────
//
// Derived from the clip's own sizes rather than fixed, since a 256x256 picture
// needs ~40x the intermediate-stream space of the original 64x64 one and a
// layout that silently overlapped would corrupt a neighbouring region rather
// than fail loudly.
/// Round up to a 4 KiB boundary so every region starts page-aligned.
const fn page_up(x: u32) -> u32 {
    (x + 0xFFF) & !0xFFF
}

const OFF_BITSTREAM: u64 = 0x0000;
const OFF_CMDLIST: u64 = page_up(MAX_NAL) as u64;
/// The command list: one 64-bit word per command. WPP adds ~7 commands per CTB
/// row on top of the ~50 fixed setup commands, so 4 KiB (512 words) is ample
/// for any clip this module embeds.
const CMDLIST_BYTES: u32 = 0x1000;
const OFF_PU: u64 = OFF_CMDLIST + CMDLIST_BYTES as u64;
const OFF_COEFF: u64 = OFF_PU + page_up(PU_BYTES) as u64;

// One output surface per DPB slot. Inter prediction reads a previously decoded
// picture, so the reference and the picture being written must be DISTINCT
// buffers — decoding a P frame on top of its own reference would corrupt the
// reference mid-read.
const SURF_Y: u32 = page_up(OUT_Y_BYTES);
const SURF_C: u32 = page_up(OUT_C_BYTES);
const OFF_SURFACES: u64 = OFF_COEFF + page_up(COEFF_BYTES) as u64;

const fn off_out_y(slot: u8) -> u64 {
    OFF_SURFACES + (slot as u64) * (SURF_Y as u64 + SURF_C as u64)
}
const fn off_out_c(slot: u8) -> u64 {
    off_out_y(slot) + SURF_Y as u64
}

const BUF_BYTES: u32 = OFF_SURFACES as u32 + (SLOTS as u32) * (SURF_Y + SURF_C);

/// Polls per `module_step`. The scheduler is cooperative, so a step must
/// never spin: an early version polled up to 200k times inside one step,
/// starved the net stack so no telemetry ever came out, and looked exactly
/// like a dead board. A real driver has the same constraint — poll a little,
/// return, resume next step.
const POLLS_PER_STEP: u32 = 64;
/// Total polls before giving up, spread across steps. MEASURED on the rig: the
/// module is stepped ~50x/sec, not once per 1 ms tick as first assumed, so a
/// budget must be sized in steps, not ticks. At 64 polls/step this is ~100
/// steps ≈ 2 s — ample for a 64x64 intra frame, and short enough that a
/// never-completing engine still reports inside the scenario window.
const POLL_BUDGET: u32 = 6_400;
/// Steps to dwell after launching phase 2 before reading the output surface.
const P2_DWELL_STEPS: u32 = 20;

/// Phase-2 (reconstruction) launch, currently **disabled**.
///
/// Phase 1 is proven on silicon (rung 3: LIST_DONE == LIST_COUNT, no overflow,
/// verdict Passed). Enabling phase 2 makes the board fault-reset in a loop —
/// telemetry stops entirely, which is why the symptom first read as a dead
/// board. Hypotheses eliminated so far, each by a rig run:
///
///   * not the module failing to load — with the decode disabled the module
///     logs happily alongside a bound DHCP lease;
///   * not the mounted `hevc_detile` / `hevc_program` code — same run;
///   * not a formatted-panic pull-in of `core::fmt` (removed anyway, since a
///     bare-metal driver must not panic);
///   * not the interrupt block at 0x10_0084_0000 — bisected out, still faults.
///
/// The leading remaining hypothesis is the reconstruction engine's own DMA:
/// phase 1 proved the block can *fetch* from the allocator's address, but
/// nothing has yet proven where its *writes* land. r02 §B.2 notes the decoder
/// is address-agnostic and works unchanged "if the IOMMU is placed in bypass,
/// or programmed with an identity map" — and bypass has never been confirmed
/// for this block. A stray engine write would corrupt memory and reset the
/// board exactly as observed. Next step is to verify the IOMMU state before
/// re-enabling this.
const ENABLE_PHASE2: bool = true;

/// Steps to wait before touching the hardware. `module_new` and the first
/// steps run before DHCP binds, so anything logged then is emitted into a net
/// debug channel that is not up yet and is simply lost (standards/rig.md §5).
/// Waiting means every stage transition is observable — without this a fault
/// during the decode kills the board before ANY line escapes, which reads as a
/// dead board rather than as a fault at a known stage.
const WARMUP_STEPS: u32 = 150;

/// Execution state machine [see `module_step`].
const ST_INIT: u8 = 0;
const ST_POLL_P1: u8 = 1;
const ST_POLL_P2: u8 = 2;
const ST_DONE: u8 = 3;

#[repr(C)]
struct DecodeState {
    syscalls: *const SyscallTable,
    out_chan: i32,
    steps: u32,
    done: bool,
    /// Outcome, logged repeatedly so the UDP monitor is sure to see it.
    dma_base: u64,
    cmd_count: u32,
    list_done: u32,
    checkpoint: u32,
    polls: u32,
    /// Phase-2 outcome: whether the completion latch fired, and one decoded
    /// luma sample per CTB row read back from the output surface.
    stage: u8,
    p2_done: bool,
    p2_polls: u32,
    luma: [u32; SAMPLES],
    /// Index of the picture being decoded, and how many have completed. The
    /// last picture is the one sampled: for an inter clip it is the P frame,
    /// whose correctness depends on every frame before it.
    frame: usize,
    frames_done: u32,
}

unsafe fn mmio_read32(sys: &SyscallTable, addr: u64) -> u32 {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    if (sys.provider_call)(-1, MMIO_READ32, buf.as_mut_ptr(), 12) < 0 {
        return 0;
    }
    u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]])
}

unsafe fn mmio_write32(sys: &SyscallTable, addr: u64, val: u32) {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    buf[8..12].copy_from_slice(&val.to_le_bytes());
    (sys.provider_call)(-1, MMIO_WRITE32, buf.as_mut_ptr(), 12);
}

unsafe fn dma_alloc_contig(sys: &SyscallTable, size: u32, align: u32) -> u64 {
    let mut buf = [0u8; 16];
    buf[..4].copy_from_slice(&size.to_le_bytes());
    buf[4..8].copy_from_slice(&align.to_le_bytes());
    if (sys.provider_call)(-1, DMA_ALLOC_CONTIG, buf.as_mut_ptr(), 16) < 0 {
        return 0;
    }
    u64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ])
}

unsafe fn dma_flush(sys: &SyscallTable, addr: u64, size: u32) {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    buf[8..12].copy_from_slice(&size.to_le_bytes());
    (sys.provider_call)(-1, DMA_FLUSH, buf.as_mut_ptr(), 12);
}

unsafe fn dma_invalidate(sys: &SyscallTable, addr: u64, size: u32) {
    let mut buf = [0u8; 12];
    buf[..8].copy_from_slice(&addr.to_le_bytes());
    buf[8..12].copy_from_slice(&size.to_le_bytes());
    (sys.provider_call)(-1, DMA_INVALIDATE, buf.as_mut_ptr(), 12);
}

/// Run phase 1 for the current frame: stage the bitstream, build and stage the
/// command list, launch, poll for completion.
///
/// Called once per coded picture. The DMA buffer is allocated on the first
/// frame and reused: the intermediate streams are scratch that phase 2 consumes
/// before the next frame overwrites them, while the output surfaces are
/// per-slot and must persist, because a later picture references them.
unsafe fn run_phase1(s: &mut DecodeState) {
    let sys = &*s.syscalls;

    if s.dma_base == 0 {
        let base = dma_alloc_contig(sys, BUF_BYTES, 64);
        s.dma_base = base;
        if base == 0 {
            s.done = true;
            return;
        }
    }
    let base = s.dma_base;
    let f = &FRAMES[s.frame];

    // 1. Stage the slice NAL. Written 32 bits at a time through the mediated
    //    syscall (the module does not map DMA memory itself). The final word
    //    covers the odd tail byte; the buffer has ample slack.
    let nal = clip::nal(s.frame);
    let n = nal.len();
    let mut i = 0usize;
    while i < n {
        let b0 = nal[i];
        let b1 = if i + 1 < n { nal[i + 1] } else { 0 };
        let b2 = if i + 2 < n { nal[i + 2] } else { 0 };
        let b3 = if i + 3 < n { nal[i + 3] } else { 0 };
        mmio_write32(
            sys,
            base + OFF_BITSTREAM + i as u64,
            u32::from_le_bytes([b0, b1, b2, b3]),
        );
        i += 4;
    }

    // 2. Assemble the command list with the real assembler and stage it. Each
    //    64-bit command word becomes two 32-bit writes, little-endian.
    let params = Phase1Intra {
        seq_a: SEQ_A,
        seq_b: SEQ_B,
        pic: f.pic,
        seg_cfg: f.seg_cfg,
        qp: f.qp,
        ctb_last_col: CTB_LAST_COL,
        ctb_last_row: CTB_LAST_ROW,
        // The 64-byte-aligned block holding the slice data, and the data's
        // offset within it [§6.2].
        bs_base: ((base + OFF_BITSTREAM) >> 6) as u32,
        bs_len_bytes: f.data_len,
        bs_byte_offset: f.data_off as u8,
        ctx_init_words: clip::ctx(s.frame),
        // For an inter slice this carries a (descriptor, POC) pair per
        // reference between the command word and the deblocking word [§7.3].
        slice_msg_words: clip::msgs(s.frame),
        slice_seq: 0,
        pu_write_base: ((base + OFF_PU) >> 6) as u32,
        // One fixed span per CTB row, on both the phase-1 writer and the
        // phase-2 reader [§4.2].
        pu_write_stride: PU_STRIDE,
        coeff_write_base: ((base + OFF_COEFF) >> 6) as u32,
        coeff_write_stride: COEFF_STRIDE,
    };

    let mut w = 0u64;
    let mut stage_word = |word: u64| {
        let addr = base + OFF_CMDLIST + w * 8;
        mmio_write32(sys, addr, (word & 0xFFFF_FFFF) as u32);
        mmio_write32(sys, addr + 4, (word >> 32) as u32);
        w += 1;
    };
    // WPP needs the §6.8 row-pause/context-save/restore sequence; without it
    // the engine would run straight through a row boundary and decode every
    // row after the first from the wrong CABAC state.
    let count = if clip::WPP {
        assemble_intra_wpp(&params, CTB_COLS, CTB_ROWS, &mut stage_word)
    } else {
        assemble_intra(&params, &mut stage_word)
    };
    s.cmd_count = count;

    // 3. Push everything to the point of coherency before the engine reads it.
    dma_flush(sys, base, BUF_BYTES);

    // 4. Launch. The final write (P1_LIST_BASE) starts the engine [§5].
    launch(
        &params,
        ((base + OFF_CMDLIST) >> 6) as u32,
        count,
        &mut |off: u32, val: u32| mmio_write32(sys, HEVC_MAIN_BASE + u64::from(off), val),
    );

    // Launched. Completion is polled across subsequent steps [§5].
    s.stage = ST_POLL_P1;
}

/// Poll `P1_LIST_DONE` for a bounded slice of this step [§5].
unsafe fn poll_phase1(s: &mut DecodeState) {
    let sys = &*s.syscalls;
    let mut i = 0u32;
    while i < POLLS_PER_STEP {
        s.list_done = mmio_read32(sys, HEVC_MAIN_BASE + u64::from(hw::P1_LIST_DONE));
        s.polls += 1;
        if s.list_done >= s.cmd_count {
            s.checkpoint = mmio_read32(sys, HEVC_MAIN_BASE + u64::from(hw::P1_CHECKPOINT));
            // Reconstruction may only run once phase 1 consumed the whole
            // list, since it reads phase 1's intermediate streams [§11].
            if ENABLE_PHASE2 {
                start_phase2(s);
            } else {
                s.stage = ST_DONE;
                s.done = true;
            }
            return;
        }
        if s.polls >= POLL_BUDGET {
            s.checkpoint = mmio_read32(sys, HEVC_MAIN_BASE + u64::from(hw::P1_CHECKPOINT));
            s.stage = ST_DONE;
            s.done = true;
            return;
        }
        i += 1;
    }
}

/// Poll the engine-2 completion latch for a bounded slice of this step [§3].
unsafe fn poll_phase2(s: &mut DecodeState) {
    let sys = &*s.syscalls;
    // Dwell without touching the interrupt block (see start_phase2). A 64x64
    // intra frame is microseconds of work, so a handful of steps is ample.
    let _ = sys;
    s.p2_polls += 1;
    if s.p2_polls < P2_DWELL_STEPS {
        return;
    }
    s.frames_done += 1;

    // More pictures to decode? The next one may reference the surface this one
    // just wrote, which is why each picture decodes into its own DPB slot.
    if s.frame + 1 < FRAMES.len() {
        s.frame += 1;
        s.p2_polls = 0;
        s.polls = 0;
        s.list_done = 0;
        s.cmd_count = 0;
        s.stage = ST_INIT;
        return;
    }

    s.p2_done = true;
    finish_phase2(s);
}

/// Read back one decoded luma sample per CTB row and finish [§8].
///
/// Sampling the FIRST row only would be nearly worthless for WPP: row 0 is the
/// one row that needs no context restore, so a completely broken wavefront
/// still decodes it correctly. Each sample is therefore taken from a different
/// CTB row, in the picture's LAST CTB column — the far end of a row, which the
/// engine only reaches after being released from column 2 and running to the
/// region end [§6.8].
unsafe fn finish_phase2(s: &mut DecodeState) {
    let sys = &*s.syscalls;
    let base = s.dma_base;
    // Drop stale CPU lines before reading what the engine wrote.
    // Sample the LAST picture decoded — for an inter clip that is the P frame,
    // which is only correct if motion compensation read the right reference.
    let slot = FRAMES[FRAMES.len() - 1].out_slot;
    let out_y = off_out_y(slot);
    dma_invalidate(sys, base + out_y, OUT_Y_BYTES);

    let surface = Surface::new(clip::WIDTH, clip::HEIGHT, clip::BIT_DEPTH);
    let mut k = 0usize;
    while k < SAMPLES {
        let (x, y) = sample_point(k);
        let off = luma_offset_8bit(surface, x, y) as u64;
        // Never read outside the plane the engine was told to write.
        s.luma[k] = if off + 4 <= u64::from(OUT_Y_BYTES) {
            mmio_read32(sys, base + out_y + off)
        } else {
            0
        };
        k += 1;
    }
    s.stage = ST_DONE;
    s.done = true;
}

/// Number of decoded-luma probe points reported.
const SAMPLES: usize = 4;

/// Probe point `k`: CTB row `k` (clamped to the picture), 20 pixels into the
/// picture's last CTB column. Derived from geometry rather than hardcoded, so
/// the same driver samples sensibly for a 1-CTB clip and a 4x4 one alike.
/// x is 4-aligned so the mediated 32-bit read stays aligned.
fn sample_point(k: usize) -> (u32, u32) {
    let x = (u32::from(CTB_LAST_COL) * clip::CTB_SIZE + 20).min(clip::WIDTH - 4) & !3;
    // Clamp the PIXEL row, not the CTB index: for a picture with fewer CTB rows
    // than samples that still spreads the probes down the picture instead of
    // collapsing them onto the last CTB row's first line.
    let y = ((k as u32) * clip::CTB_SIZE).min(clip::HEIGHT - 1);
    (x, y)
}

/// Program and launch the reconstruction engine on phase 1's output [§10, §3].
/// Completion is polled across subsequent steps.
unsafe fn start_phase2(s: &mut DecodeState) {
    let sys = &*s.syscalls;
    let base = s.dma_base;

    // NOTE: the interrupt block (§3) is deliberately NOT touched here. It sits
    // at 0x10_0084_0000, a different page from the main register block, and is
    // the only access outside the block that the version/DMA rungs proved
    // reachable — so it is bisected out while the phase-2 register path itself
    // is under test. Completion is instead detected by dwelling a fixed number
    // of steps, which is sound for a 64x64 intra frame.

    let f = &FRAMES[s.frame];

    // Reference slot n must hold the addresses of the picture in DPB slot n —
    // that is the link between the slice messages' descriptor (bits 3:0) and
    // these registers [§7.3, §9].
    //
    // Every one of the 16 sets must hold a VALID picture address so a
    // malformed index cannot fetch from an unprogrammed slot [§9]. Slots this
    // clip never uses are filled with slot 0's surface rather than left zero.
    let slot_ref = |slot: u8| RefPic {
        luma_base: ((base + off_out_y(slot)) >> 6) as u32,
        luma_stride: OUT_Y_STRIDE,
        chroma_base: ((base + off_out_c(slot)) >> 6) as u32,
        chroma_stride: OUT_C_STRIDE,
    };
    let mut refs = [slot_ref(0); 16];
    let mut k = 0usize;
    while k < SLOTS {
        refs[k] = slot_ref(k as u8);
        k += 1;
    }

    let out_y = ((base + off_out_y(f.out_slot)) >> 6) as u32;
    let out_c = ((base + off_out_c(f.out_slot)) >> 6) as u32;

    let p2 = Phase2Launch {
        surface: Surface::new(clip::WIDTH, clip::HEIGHT, clip::BIT_DEPTH),
        pu_read_base: ((base + OFF_PU) >> 6) as u32,
        // Same base and same stride as the phase-1 writer [§4.2].
        pu_read_stride: PU_STRIDE,
        coeff_read_base: ((base + OFF_COEFF) >> 6) as u32,
        coeff_read_stride: COEFF_STRIDE,
        pic_cfg: f.p2_pic_cfg,
        out_luma_base: out_y,
        out_chroma_base: out_c,
        // Temporal MVP is off for every clip this module embeds (the host
        // precompute refuses a clip that enables it), so there is no
        // collocated-MV traffic [§10].
        mv_write_base: 0,
        mv_read_base: 0,
        mv_stride: 0,
        poc: f.poc,
        ctb_rows: P2_ROWS_V,
        refs,
    };

    program_phase2(&p2, &mut |off: u32, val: u32| {
        mmio_write32(sys, HEVC_MAIN_BASE + u64::from(off), val)
    });

    // Launched; the completion latch is polled across subsequent steps [§3].
    s.stage = ST_POLL_P2;
}

// Both writers are BOUNDS-CHECKED and silently stop at the end of the buffer.
//
// They were not, and it cost a long misdiagnosis: adding the st=/p2=/luma=
// fields took the line from 100 to 134 bytes while the buffer stayed
// [u8; 128]. The 6-byte overrun smashed the stack on every log, faulting the
// board into a reset loop — with all telemetry gone, which reads as "the
// module broke the hardware" rather than "the module has an off-by-six". A
// truncated diagnostic line is always better than an unrecoverable fault, so
// these can no longer write past the end regardless of what a caller adds.
fn hex32(out: &mut [u8], n: &mut usize, v: u32) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for shift in (0..8).rev() {
        if *n >= out.len() {
            return;
        }
        out[*n] = HEX[((v >> (shift * 4)) & 0xF) as usize];
        *n += 1;
    }
}

fn emit(out: &mut [u8], n: &mut usize, bytes: &[u8]) {
    for &b in bytes {
        if *n >= out.len() {
            return;
        }
        out[*n] = b;
        *n += 1;
    }
}

/// `[hevc] p1 base=0x… cnt=NN done=NN cp=0x… polls=NN ok=<0|1>`
fn format_result(out: &mut [u8; 256], s: &DecodeState) -> usize {
    let mut n = 0usize;
    emit(out, &mut n, b"[hevc] n=0x");
    hex32(out, &mut n, BUILD_NONCE);
    emit(out, &mut n, b" p1 base=0x");
    hex32(out, &mut n, (s.dma_base >> 32) as u32);
    hex32(out, &mut n, s.dma_base as u32);
    emit(out, &mut n, b" cnt=0x");
    hex32(out, &mut n, s.cmd_count);
    emit(out, &mut n, b" done=0x");
    hex32(out, &mut n, s.list_done);
    emit(out, &mut n, b" cp=0x");
    hex32(out, &mut n, s.checkpoint);
    emit(out, &mut n, b" polls=0x");
    hex32(out, &mut n, s.polls);
    emit(out, &mut n, b" st=");
    emit(out, &mut n, &[b'0' + s.stage]);
    emit(out, &mut n, b" fr=0x");
    hex32(out, &mut n, s.frames_done);
    // What the driver ACTUALLY sees through the frame table's &'static
    // pointers. A PIC module's static-to-static references need relocations,
    // and a wrong one here would hand phase 1 the wrong bitstream or CABAC
    // array — which presents as "undecodable bitstream", not as a crash.
    emit(out, &mut n, b" nl=0x");
    hex32(out, &mut n, FRAMES[s.frame].nal_len);
    emit(out, &mut n, b" cx=0x");
    hex32(out, &mut n, clip::ctx(s.frame)[0]);
    emit(out, &mut n, b" ml=0x");
    hex32(out, &mut n, clip::msgs(s.frame).len() as u32);
    emit(out, &mut n, b" p2=");
    emit(out, &mut n, if s.p2_done { b"1" } else { b"0" });
    // One sample per CTB row, so a wavefront that lost its context after row 0
    // is visible as a wrong value in luma1..3 rather than hidden.
    emit(out, &mut n, b" luma=0x");
    let mut k = 0usize;
    while k < SAMPLES {
        hex32(out, &mut n, s.luma[k]);
        k += 1;
    }
    let ok = s.cmd_count != 0 && s.list_done == s.cmd_count;
    emit(out, &mut n, if ok { b" ok=1" } else { b" ok=0" });
    n
}

#[no_mangle]
#[link_section = ".text.module_state_size"]
pub extern "C" fn module_state_size() -> u32 {
    core::mem::size_of::<DecodeState>() as u32
}

/// # Safety
///
/// A module ABI entry point: the kernel's loader is the only caller, and it
/// passes the syscall table it built for this instance. Not callable from Rust.
#[no_mangle]
#[link_section = ".text.module_init"]
pub unsafe extern "C" fn module_init(_syscalls: *const c_void) {}

#[no_mangle]
#[link_section = ".text.module_new"]
pub extern "C" fn module_new(
    _in_chan: i32,
    out_chan: i32,
    _ctrl_chan: i32,
    _params: *const u8,
    _params_len: usize,
    state: *mut u8,
    state_size: usize,
    syscalls: *const c_void,
) -> i32 {
    unsafe {
        if syscalls.is_null() || state.is_null() {
            return -1;
        }
        if state_size < core::mem::size_of::<DecodeState>() {
            return -2;
        }
        let s = &mut *(state as *mut DecodeState);
        s.syscalls = syscalls as *const SyscallTable;
        s.out_chan = out_chan;
        s.steps = 0;
        s.done = false;
        s.dma_base = 0;
        s.cmd_count = 0;
        s.list_done = 0;
        s.checkpoint = 0;
        s.polls = 0;
        s.stage = ST_INIT;
        s.p2_done = false;
        s.p2_polls = 0;
        s.luma = [0; SAMPLES];
        s.frame = 0;
        s.frames_done = 0;
        dev_log(&*s.syscalls, 3, b"[hevc] init".as_ptr(), 11);
        0
    }
}

#[no_mangle]
#[link_section = ".text.module_step"]
pub extern "C" fn module_step(state: *mut u8) -> i32 {
    unsafe {
        if state.is_null() {
            return -1;
        }
        let s = &mut *(state as *mut DecodeState);
        if s.syscalls.is_null() {
            return 0;
        }
        s.steps = s.steps.wrapping_add(1);
        // Let the net stack come up first, so stage transitions are visible.
        if s.steps < WARMUP_STEPS {
            if s.steps.is_multiple_of(100) {
                let sys = &*s.syscalls;
                dev_log(sys, 3, b"[hevc] warmup".as_ptr(), 13);
            }
            return 0;
        }
        // One bounded slice of work per step — never spin the scheduler.
        match s.stage {
            ST_INIT => run_phase1(s),
            ST_POLL_P1 => poll_phase1(s),
            ST_POLL_P2 => poll_phase2(s),
            _ => {}
        }
        // Log the outcome as a recurring steady-state line the UDP monitor is
        // guaranteed to observe (standards/rig.md §5).
        if s.steps.is_multiple_of(20) {
            let sys = &*s.syscalls;
            let mut line = [0u8; 256];
            let len = format_result(&mut line, s);
            dev_log(sys, 3, line.as_ptr(), len);
        }
        0
    }
}

include!("../../../target/fluxor/fluxor-abi/sdk/runtime/wasm_entry.rs");

//! BCM2712 HEVC phase-1 (entropy) command-list assembly — the simple case.
//!
//! Phase 1 is driven by a *command list* in DMA memory: a sequence of 64-bit
//! `(target, value)` words the engine fetches and applies [r01 §5]. This module
//! assembles that list for the **single-slice, single-tile, no-WPP,
//! no-scaling-list** case (spec §6.7), composing the register encoders of
//! [`crate::hevc_bcm2712`], the CABAC context-init array of
//! [`crate::hevc_ctx_init`], and the slice messages of
//! [`crate::hevc_slice_msg`] into one ordered program.
//!
//! It is deliberately the simplest decode mode — one intra slice covering the
//! whole picture as one tile, with the CABAC context initialised fresh. That is
//! the mode a non-WPP, single-slice test clip exercises, and it avoids the
//! intricate §6.8 WPP row-pause/context-restore sequence, which is a separate,
//! harder assembler validated only against real decodes.
//!
//! Like [`crate::hevc_program`] it drives callbacks rather than allocating: an
//! `emit(word)` for the command-list words (written to DMA), and a
//! `write(offset, value)` for the launch MMIO registers, whose last write
//! (`P1_LIST_BASE`) starts the engine [r01 §5].
//!
//! **Clean-room Team B.** Sequence and register offsets from released spec r01
//! §5/§6.2/§6.5/§6.6/§6.7/§7.1/§7.3. No GPL source read.

use crate::hevc_bcm2712 as hw;

/// Everything needed to assemble and launch one intra slice's phase-1 program.
/// Addresses are the encoded byte-address>>6 register forms.
pub struct Phase1Intra<'a> {
    /// Precomputed sequence/picture registers [r01 §6.5].
    pub seq_a: u32,
    pub seq_b: u32,
    pub pic: u32,
    /// Per-entry-point slice config [r01 §6.6].
    pub seg_cfg: u32,
    /// Initial luma QP for context init: `slice QP + 6·(luma_depth−8)`
    /// [r01 §6.5/§6.6].
    pub qp: i32,
    /// Picture's last coding-tree-block coordinates (0-based).
    pub ctb_last_col: u16,
    pub ctb_last_row: u16,
    /// Bitstream buffer: base (>>6), length in **bytes**, and the byte offset
    /// of the slice data within its 64-byte-aligned block [r01 §6.2].
    pub bs_base: u32,
    pub bs_len_bytes: u32,
    pub bs_byte_offset: u8,
    /// The 39 CABAC context-init words [r01 §7.1].
    pub ctx_init_words: &'a [u32],
    /// The slice-parameter message words (16-bit, one per FIFO slot) [r01 §7.3].
    pub slice_msg_words: &'a [u16],
    /// Per-picture slice sequence number for the message commit [r01 §7.3].
    pub slice_seq: u8,
    /// Intermediate stream write bases/strides, programmed at launch [r01 §5].
    pub pu_write_base: u32,
    pub pu_write_stride: u32,
    pub coeff_write_base: u32,
    pub coeff_write_stride: u32,
}

/// `P1_BS_CTRL` bit 7 — mark stream end/termination [r01 §6.2].
const BS_CTRL_END: u32 = 1 << 7;
/// `P1_BS_CTRL` bit 6 — commit the fetch with emulation-prevention removal
/// [r01 §6.2].
const BS_CTRL_COMMIT_EPB: u32 = 1 << 6;

/// `P1_RUN_MODE` tile mode (run to region end) [r01 §6.6].
const RUN_MODE_TILE: u32 = 0xFFFF;
/// `P1_RUN_MODE` bit 17 — region's last CTB column is the picture's last.
const RUN_MODE_LAST_COL: u32 = 1 << 17;
/// `P1_RUN_MODE` bit 18 — region's last CTB row is the picture's last.
const RUN_MODE_LAST_ROW: u32 = 1 << 18;

/// `P1_RUN_MODE` low half for **WPP**: pause at each row end [r01 §6.6].
const RUN_MODE_WPP: u32 = 1;
/// `P1_RUN_MODE` written when RESUMING a paused WPP row mid-row [r01 §6.6].
/// Bit 16 marks the resume (open point O-5) and the low half is zero.
const RUN_MODE_WPP_RESUME: u32 = 0x3_0000;
/// As above, on the picture's final row (bit 18 additionally set). Not reached
/// by the single-slice row loop, which only advances rows *before* the last;
/// retained because it is a real spec value that the mid-row slice-end path of
/// §6.8 needs, and inventing it later from memory is how transcription errors
/// get in.
#[allow(
    dead_code,
    reason = "spec value needed by the not-yet-built mid-row slice-end path"
)]
const RUN_MODE_WPP_RESUME_LAST_ROW: u32 = 0x7_0000;

/// Assemble the phase-1 command list for one intra slice, calling `emit` once
/// per 64-bit command word in the order the engine must apply them [r01 §6.7].
/// Returns the number of words emitted — the value written to `P1_LIST_COUNT`
/// at launch.
pub fn assemble_intra(p: &Phase1Intra<'_>, emit: &mut impl FnMut(u64)) -> u32 {
    let mut count = 0u32;
    let cmd = |target: u16, value: u32, count: &mut u32, emit: &mut dyn FnMut(u64)| {
        emit(hw::command(target, value));
        *count += 1;
    };

    // 1. Bitstream feed [r01 §6.2]: base, length (bytes), then the two BS_CTRL
    //    writes — end, then commit-with-EPB-removal, in that order.
    cmd(hw::P1_BS_BASE as u16, p.bs_base, &mut count, emit);
    cmd(hw::P1_BS_LEN as u16, p.bs_len_bytes, &mut count, emit);
    let off = u32::from(p.bs_byte_offset) & 0x3F;
    cmd(hw::P1_BS_CTRL as u16, off | BS_CTRL_END, &mut count, emit);
    cmd(
        hw::P1_BS_CTRL as u16,
        off | BS_CTRL_COMMIT_EPB,
        &mut count,
        emit,
    );

    // 2. CABAC context init [r01 §7.1]: write the array, then save it to the
    //    backup bank so it can be restored later.
    for (k, &word) in p.ctx_init_words.iter().enumerate() {
        cmd(
            (hw::P1_WIN_CABAC_INIT + 4 * k as u32) as u16,
            word,
            &mut count,
            emit,
        );
    }
    cmd(hw::P1_CTX_XFER as u16, hw::P1_CTX_SAVE, &mut count, emit);

    // 3. Slice messages [r01 §7.3]: write each 16-bit message to its FIFO slot,
    //    then commit the batch.
    for (m, &word) in p.slice_msg_words.iter().enumerate() {
        cmd(
            (hw::P1_WIN_SLICE_MSG + 4 * m as u32) as u16,
            u32::from(word),
            &mut count,
            emit,
        );
    }
    let commit = hw::command(
        hw::P1_MSG_CTRL as u16,
        crate::hevc_slice_msg::msg_commit(p.slice_msg_words.len() as u8, p.slice_seq),
    );
    emit(commit);
    count += 1;

    // 4. Sequence/picture registers, then the segment's first CTB [r01 §6.5].
    cmd(hw::P1_SEQ_A as u16, p.seq_a, &mut count, emit);
    cmd(hw::P1_SEQ_B as u16, p.seq_b, &mut count, emit);
    cmd(hw::P1_PIC as u16, p.pic, &mut count, emit);
    cmd(
        hw::P1_SEG_FIRST as u16,
        hw::ctb_addr(0, 0),
        &mut count,
        emit,
    );

    // 5. Entry point for the single tile that is the whole picture [r01 §6.6].
    let last = hw::ctb_addr(p.ctb_last_col, p.ctb_last_row);
    cmd(
        hw::P1_RGN_FIRST as u16,
        hw::ctb_addr(0, 0),
        &mut count,
        emit,
    );
    cmd(hw::P1_RGN_LAST as u16, last, &mut count, emit);
    // Independent slice segment: RGN_LAST_INDEP repeats RGN_LAST.
    cmd(hw::P1_RGN_LAST_INDEP as u16, last, &mut count, emit);
    cmd(hw::P1_SEG_CFG as u16, p.seg_cfg, &mut count, emit);
    cmd(hw::P1_QP as u16, p.qp as u32, &mut count, emit);
    // Tile mode, running to the region end; the region ends at the picture's
    // last column and row, so both edge bits are set.
    cmd(
        hw::P1_RUN_MODE as u16,
        RUN_MODE_TILE | RUN_MODE_LAST_COL | RUN_MODE_LAST_ROW,
        &mut count,
        emit,
    );
    // Commit: writing P1_RUN_FROM starts decode from (0,0).
    cmd(hw::P1_RUN_FROM as u16, hw::ctb_addr(0, 0), &mut count, emit);

    // 6. Checkpoint: reason 1 (end of slice) at the picture's last CTB [r01 §6.6].
    cmd(
        hw::P1_CHECKPOINT as u16,
        hw::checkpoint(
            hw::CHECKPOINT_SLICE_END,
            u32::from(p.ctb_last_col),
            u32::from(p.ctb_last_row),
        ),
        &mut count,
        emit,
    );

    count
}

/// Launch phase 1 [r01 §5]. Programs the intermediate-stream write registers,
/// the command count, and finally the command-list base — whose write **starts
/// the engine** and must therefore be last, with all earlier writes ordered
/// before it. `list_base` is the command-list buffer, byte-address>>6;
/// `command_count` is the value [`assemble_intra`] returned.
pub fn launch(
    p: &Phase1Intra<'_>,
    list_base: u32,
    command_count: u32,
    write: &mut impl FnMut(u32, u32),
) {
    write(hw::P1_PU_WBASE, p.pu_write_base);
    write(hw::P1_PU_WSTRIDE, p.pu_write_stride);
    write(hw::P1_COEFF_WBASE, p.coeff_write_base);
    write(hw::P1_COEFF_WSTRIDE, p.coeff_write_stride);
    write(hw::P1_LIST_COUNT, command_count);
    // LAUNCH — must be last [r01 §5].
    write(hw::P1_LIST_BASE, list_base);
}

/// Assemble the phase-1 command list for a **single-slice intra picture coded
/// with wavefront parallel processing** (WPP) [r01 §6.8].
///
/// This is the mode real content uses: every x265 stream enables
/// `entropy_coding_sync`, so [`assemble_intra`]'s single-tile path — while
/// correct for a hand-made clip — cannot decode a normal file at all.
///
/// The shape, and why it is intricate: WPP decodes one CTB **row-span** at a
/// time, and each row's entropy state seeds the next. So for every row before
/// the last the list must (a) pause the row early, right after column 1, and
/// **save** the context — that saved top-left state is what row r+1 starts
/// from; (b) release the engine to finish the rest of row r from column 2; (c)
/// checkpoint the row end; (d) **restore** the saved context; (e) open the
/// entry point for row r+1. Getting that dance wrong does not produce a
/// structural error — it produces a wrong picture or a hung engine, which is
/// why it is validated against a reference decode on real hardware rather than
/// by inspection.
///
/// `ctb_cols`/`ctb_rows` are the picture's coding-tree-block dimensions. The
/// **2-CTB-wide special case** is honoured: with a picture that narrow there is
/// no "column 2" to release to, so the spec emits a save rather than a restore
/// (the saved state is already the row-start state) and no row pause is used.
pub fn assemble_intra_wpp(
    p: &Phase1Intra<'_>,
    ctb_cols: u16,
    ctb_rows: u16,
    emit: &mut impl FnMut(u64),
) -> u32 {
    let mut count = 0u32;
    let cmd = |target: u16, value: u32, count: &mut u32, emit: &mut dyn FnMut(u64)| {
        emit(hw::command(target, value));
        *count += 1;
    };

    let last_col = ctb_cols.saturating_sub(1);
    let last_row = ctb_rows.saturating_sub(1);
    // A row pause needs a column 2 to release the engine to [r01 §6.8].
    let wide_enough_to_pause = ctb_cols > 2;

    // ── Shared setup, exactly as the single-tile path [r01 §6.7 steps 2-5] ──
    cmd(hw::P1_BS_BASE as u16, p.bs_base, &mut count, emit);
    cmd(hw::P1_BS_LEN as u16, p.bs_len_bytes, &mut count, emit);
    let off = u32::from(p.bs_byte_offset) & 0x3F;
    cmd(hw::P1_BS_CTRL as u16, off | BS_CTRL_END, &mut count, emit);
    cmd(
        hw::P1_BS_CTRL as u16,
        off | BS_CTRL_COMMIT_EPB,
        &mut count,
        emit,
    );
    for (k, &word) in p.ctx_init_words.iter().enumerate() {
        cmd(
            (hw::P1_WIN_CABAC_INIT + 4 * k as u32) as u16,
            word,
            &mut count,
            emit,
        );
    }
    cmd(hw::P1_CTX_XFER as u16, hw::P1_CTX_SAVE, &mut count, emit);
    for (m, &word) in p.slice_msg_words.iter().enumerate() {
        cmd(
            (hw::P1_WIN_SLICE_MSG + 4 * m as u32) as u16,
            u32::from(word),
            &mut count,
            emit,
        );
    }
    cmd(
        hw::P1_MSG_CTRL as u16,
        crate::hevc_slice_msg::msg_commit(p.slice_msg_words.len() as u8, p.slice_seq),
        &mut count,
        emit,
    );
    cmd(hw::P1_SEQ_A as u16, p.seq_a, &mut count, emit);
    cmd(hw::P1_SEQ_B as u16, p.seq_b, &mut count, emit);
    cmd(hw::P1_PIC as u16, p.pic, &mut count, emit);
    cmd(
        hw::P1_SEG_FIRST as u16,
        hw::ctb_addr(0, 0),
        &mut count,
        emit,
    );

    // ── Row 0 entry point ──────────────────────────────────────────────────
    // In WPP the region is always the whole picture and RGN_LAST's row is the
    // row being entered [r01 §6.8].
    let open_row = |row: u16, first: bool, count: &mut u32, emit: &mut dyn FnMut(u64)| {
        let stop = hw::ctb_addr(last_col, row);
        emit(hw::command(hw::P1_RGN_FIRST as u16, hw::ctb_addr(0, 0)));
        *count += 1;
        emit(hw::command(hw::P1_RGN_LAST as u16, stop));
        *count += 1;
        if first {
            // Only the slice's own first entry point begins an independent
            // slice segment [r01 §6.6 step 3].
            emit(hw::command(hw::P1_RGN_LAST_INDEP as u16, stop));
            *count += 1;
        }
        emit(hw::command(hw::P1_SEG_CFG as u16, p.seg_cfg));
        *count += 1;
        if first {
            // QP is written only where the context is freshly initialised;
            // later rows restore it instead [r01 §6.6 step 5].
            emit(hw::command(hw::P1_QP as u16, p.qp as u32));
            *count += 1;
        }
        // In WPP the region is the whole picture, so the region's last column
        // IS the picture's last — bit 17 is therefore always set here
        // [r01 §6.6, §6.8].
        //
        // The low half selects whether this row PAUSES. WPP mode (1) pauses
        // after column 1 so the next row may start, and something must then
        // release the engine to finish the row from column 2. §6.8 emits that
        // release only "for each row completed before the slice's last row",
        // so the last row has nobody to release it: started in pause mode it
        // parks after column 1 forever, and the slice-end checkpoint — which
        // blocks until decode reaches the last CTB — is never satisfied.
        //
        // MEASURED on silicon: with the last row in pause mode the engine
        // applied 92 of 93 commands and stalled on exactly that checkpoint,
        // with P1_CHECKPOINT reading back reason 5 (WPP row pause). The last
        // row therefore runs to region end, as §6.7 does [r01 §6.6, §6.8].
        let mode = if row == last_row {
            RUN_MODE_TILE | RUN_MODE_LAST_COL | RUN_MODE_LAST_ROW
        } else {
            RUN_MODE_WPP | RUN_MODE_LAST_COL
        };
        emit(hw::command(hw::P1_RUN_MODE as u16, mode));
        *count += 1;
        // Writing RUN_FROM commits the entry point [r01 §6.6 step 7].
        emit(hw::command(hw::P1_RUN_FROM as u16, hw::ctb_addr(0, row)));
        *count += 1;
    };
    open_row(0, true, &mut count, emit);

    // ── Per-row advance for every row before the last [r01 §6.8] ───────────
    for row in 0..last_row {
        if wide_enough_to_pause {
            // Pause after column 1 and save the top-left context: this is the
            // state row r+1 begins from.
            cmd(
                hw::P1_CHECKPOINT as u16,
                hw::checkpoint(hw::CHECKPOINT_WPP_ROW_PAUSE, 1, u32::from(row)),
                &mut count,
                emit,
            );
            cmd(hw::P1_CTX_XFER as u16, hw::P1_CTX_SAVE, &mut count, emit);
            // This loop covers rows BEFORE the last, so the last-row resume
            // form is unreachable from here; it is named and kept for the
            // single-row and slice-end paths that will need it.
            cmd(
                hw::P1_RUN_MODE as u16,
                RUN_MODE_WPP_RESUME,
                &mut count,
                emit,
            );
            // Release the engine to finish this row from column 2.
            cmd(
                hw::P1_RUN_FROM as u16,
                hw::ctb_addr(2, row),
                &mut count,
                emit,
            );
        }
        // End of this row's span.
        cmd(
            hw::P1_CHECKPOINT as u16,
            hw::checkpoint(
                hw::CHECKPOINT_REGION_END,
                u32::from(last_col),
                u32::from(row),
            ),
            &mut count,
            emit,
        );
        // Restore the saved row-start context for the next row — except on a
        // 2-CTB-wide picture, where the spec emits a save instead because the
        // saved state IS the row-start state [r01 §6.8].
        let xfer = if wide_enough_to_pause {
            hw::P1_CTX_RESTORE
        } else {
            hw::P1_CTX_SAVE
        };
        cmd(hw::P1_CTX_XFER as u16, xfer, &mut count, emit);
        open_row(row + 1, false, &mut count, emit);
    }

    // ── Slice end at the picture's last CTB [r01 §6.6, §6.7 step 7] ────────
    cmd(
        hw::P1_CHECKPOINT as u16,
        hw::checkpoint(
            hw::CHECKPOINT_SLICE_END,
            u32::from(last_col),
            u32::from(last_row),
        ),
        &mut count,
        emit,
    );

    count
}

/// `P1_RUN_MODE` value for the whole-picture single-tile case, exposed for
/// tests and callers that build the entry point by hand.
#[must_use]
pub const fn run_mode_single_tile() -> u32 {
    RUN_MODE_TILE | RUN_MODE_LAST_COL | RUN_MODE_LAST_ROW
}

//! HEVC CABAC context-variable initialisation (H.265 §9.3.2.2).
//!
//! Given an 8-bit `initValue` (the per-context constant tabulated in H.265
//! Tables 9-4…9-42) and the slice's luma QP, this derives the initial CABAC
//! context state: the probability-state index and the most-probable-symbol
//! value. It is **pure public-standard arithmetic** — it comes from ITU-T
//! H.265 §9.3.2.2, the same source the rest of this crate's HEVC parsing was
//! built from, and it is independent of any particular decoder or hardware.
//!
//! It exists because every HEVC decode back end needs it: a software CABAC
//! engine seeds its contexts with `(pstate_idx, val_mps)` directly, and the
//! BCM2712 hardware wants the same states packed into its context-init array.
//! Keeping the derivation here, standalone and tested against the standard,
//! means both back ends share one validated implementation rather than each
//! re-deriving it — and, notably, the hardware path can be built on top of
//! this without the derivation itself depending on any reverse-engineered
//! detail.

use crate::hevc::SliceType;

/// An initialised CABAC context: a 6-bit probability-state index (0..=62) and
/// the most-probable-symbol value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CtxState {
    /// `pStateIdx`, 0..=62 [H.265 §9.3.2.2].
    pub pstate_idx: u8,
    /// `valMps`, the most-probable symbol (0 or 1).
    pub val_mps: u8,
}

impl CtxState {
    /// The combined 7-bit form some hardware uses: `(pStateIdx << 1) | valMps`.
    /// Provided as a convenience; the hardware's exact byte packing is its own
    /// concern and lives with the hardware back end, not here.
    #[must_use]
    pub const fn packed7(self) -> u8 {
        (self.pstate_idx << 1) | self.val_mps
    }
}

/// Clip `v` into `[lo, hi]` — H.265's `Clip3`.
#[inline]
const fn clip3(lo: i32, hi: i32, v: i32) -> i32 {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

/// Derive a context's initial state from its `initValue` and the slice QP,
/// per H.265 §9.3.2.2.
///
/// The steps, verbatim from the standard:
/// ```text
/// slopeIdx    = initValue >> 4
/// offsetIdx   = initValue & 15
/// m           = slopeIdx * 5 − 45
/// n           = (offsetIdx << 3) − 16
/// preCtxState = Clip3(1, 126, ((m * Clip3(0, 51, SliceQpY)) >> 4) + n)
/// valMps      = preCtxState <= 63 ? 0 : 1
/// pStateIdx   = valMps ? preCtxState − 64 : 63 − preCtxState
/// ```
///
/// The `>> 4` is an arithmetic shift on a signed product (`m` is negative for
/// low slope indices), which Rust's `>>` on `i32` provides. Getting that shift
/// unsigned would skew every context toward the wrong symbol at low QP, which
/// is why it is called out.
#[must_use]
pub fn init_ctx(init_value: u8, slice_qp: i32) -> CtxState {
    let slope_idx = i32::from(init_value >> 4);
    let offset_idx = i32::from(init_value & 0x0F);
    let m = slope_idx * 5 - 45;
    let n = (offset_idx << 3) - 16;

    let qp = clip3(0, 51, slice_qp);
    // Arithmetic right shift on the signed product, per the standard.
    let pre_ctx_state = clip3(1, 126, ((m * qp) >> 4) + n);

    let val_mps = u8::from(pre_ctx_state > 63);
    let pstate_idx = if val_mps == 1 {
        pre_ctx_state - 64
    } else {
        63 - pre_ctx_state
    };

    CtxState {
        // pstate_idx is in 0..=62 by construction (pre_ctx_state in 1..=126).
        pstate_idx: pstate_idx as u8,
        val_mps,
    }
}

/// Which of the three init-value tables a slice uses [H.265 §9.3.2.2, Table
/// 9-4 selection]. An I slice always uses table 0; a P or B slice uses table 1
/// or 2 depending on `cabac_init_flag`, which swaps the P and B assignments.
///
/// Returned as an index 0/1/2 so a caller can index whichever tabulation it
/// holds. The mapping: I → 0. With `cabac_init_flag` clear, P → 1, B → 2; with
/// it set, the two swap (P → 2, B → 1). (The flag only ever applies to P and B
/// slices.)
#[must_use]
pub const fn init_table_index(slice_type: SliceType, cabac_init_flag: bool) -> u8 {
    match slice_type {
        SliceType::I => 0,
        SliceType::P => {
            if cabac_init_flag {
                2
            } else {
                1
            }
        }
        SliceType::B => {
            if cabac_init_flag {
                1
            } else {
                2
            }
        }
    }
}

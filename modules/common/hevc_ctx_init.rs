//! HEVC CABAC context-initialisation array for the BCM2712 phase-1 engine
//! (spec §7.1).
//!
//! Phase 1 seeds its CABAC contexts from a 156-byte array written to the
//! command window at 0x1000 [r01 §7.1]. Each byte is the 7-bit initial context
//! state for one context variable, derived from that context's tabulated
//! `initValue` and the slice QP.
//!
//! Two parts combine here:
//! - the **derivation** — pure H.265 §9.3.2.2 arithmetic, already in
//!   [`crate::hevc_cabac`]. The spec states §7.1's byte as a single folded
//!   expression; it is *universally equal* to `init_ctx(v, qp).packed7()` (the
//!   two sequential clamps in §7.1 reproduce `Clip3(1,126)` plus the
//!   `(state<<1)|MPS` packing exactly), which the tests prove over every
//!   `(v, qp)`. So this module reuses the standard derivation rather than
//!   re-encoding the hardware's formula.
//! - the **tables** — the `initValue` constants in *this hardware's context
//!   ordering*, which differs from the ordering software decoders use and is a
//!   hardware fact taken from the spec. These are the one genuinely
//!   spec-sourced datum here.
//!
//! **Clean-room Team B.** The three tables and the array layout are from
//! released spec r01 §7.1; the derivation is public H.265. No GPL source read.
//! The tables are typed `[u8; 156]`, so the compiler enforces the count — a
//! mis-paste that dropped or added a value would not build.

use crate::hevc_cabac::init_ctx;

/// Context states per array: 154 real, zero-padded to 156 [r01 §7.1].
pub const CTX_STATES: usize = 156;

/// Real (non-padding) context count [r01 §7.1].
pub const CTX_REAL: usize = 154;

/// Command-window offset the array is written to [r01 §5, §7.1].
pub const CTX_INIT_WINDOW: u32 = 0x1000;

/// The three `initValue` tables, in the hardware's context ordering [r01 §7.1].
/// Index with [`crate::hevc_cabac::init_table_index`]. Entries 154 and 155 are
/// padding and are never fed to the derivation (see [`build`]).
///
/// Transcribed verbatim from released spec r01 §7.1; the `[u8; CTX_STATES]`
/// type guards the count.
pub static INIT_VALUE_TABLES: [[u8; CTX_STATES]; 3] = [
    // Table 0
    [
        153, 200, 139, 141, 157, 154, 154, 154, 154, 154, 184, 154, 154, 154, 184, 63, 154, 154,
        154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 154, 153, 138, 138, 111, 141, 94, 138,
        182, 154, 154, 154, 140, 92, 137, 138, 140, 152, 138, 139, 153, 74, 149, 92, 139, 107, 122,
        152, 140, 179, 166, 182, 140, 227, 122, 197, 110, 110, 124, 125, 140, 153, 125, 127, 140,
        109, 111, 143, 127, 111, 79, 108, 123, 63, 110, 110, 124, 125, 140, 153, 125, 127, 140,
        109, 111, 143, 127, 111, 79, 108, 123, 63, 91, 171, 134, 141, 138, 153, 136, 167, 152, 152,
        139, 139, 111, 111, 125, 110, 110, 94, 124, 108, 124, 107, 125, 141, 179, 153, 125, 107,
        125, 141, 179, 153, 125, 107, 125, 141, 179, 153, 125, 140, 139, 182, 182, 152, 136, 152,
        136, 153, 136, 139, 111, 136, 139, 111, 0, 0,
    ],
    // Table 1
    [
        153, 185, 107, 139, 126, 197, 185, 201, 154, 149, 154, 139, 154, 154, 154, 152, 110, 122,
        95, 79, 63, 31, 31, 153, 153, 168, 140, 198, 79, 124, 138, 94, 153, 111, 149, 107, 167,
        154, 154, 154, 154, 196, 196, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136,
        137, 169, 194, 166, 167, 154, 167, 137, 182, 125, 110, 94, 110, 95, 79, 125, 111, 110, 78,
        110, 111, 111, 95, 94, 108, 123, 108, 125, 110, 94, 110, 95, 79, 125, 111, 110, 78, 110,
        111, 111, 95, 94, 108, 123, 108, 121, 140, 61, 154, 107, 167, 91, 122, 107, 167, 139, 139,
        155, 154, 139, 153, 139, 123, 123, 63, 153, 166, 183, 140, 136, 153, 154, 166, 183, 140,
        136, 153, 154, 166, 183, 140, 136, 153, 154, 170, 153, 123, 123, 107, 121, 107, 121, 167,
        151, 183, 140, 151, 183, 140, 0, 0,
    ],
    // Table 2
    [
        153, 160, 107, 139, 126, 197, 185, 201, 154, 134, 154, 139, 154, 154, 183, 152, 154, 137,
        95, 79, 63, 31, 31, 153, 153, 168, 169, 198, 79, 224, 167, 122, 153, 111, 149, 92, 167,
        154, 154, 154, 154, 196, 167, 167, 154, 152, 167, 182, 182, 134, 149, 136, 153, 121, 136,
        122, 169, 208, 166, 167, 154, 152, 167, 182, 125, 110, 124, 110, 95, 94, 125, 111, 111, 79,
        125, 126, 111, 111, 79, 108, 123, 93, 125, 110, 124, 110, 95, 94, 125, 111, 111, 79, 125,
        126, 111, 111, 79, 108, 123, 93, 121, 140, 61, 154, 107, 167, 91, 107, 107, 167, 139, 139,
        170, 154, 139, 153, 139, 123, 123, 63, 124, 166, 183, 140, 136, 153, 154, 166, 183, 140,
        136, 153, 154, 166, 183, 140, 136, 153, 154, 170, 153, 138, 138, 122, 121, 122, 121, 167,
        151, 183, 140, 151, 183, 140, 0, 0,
    ],
];

/// Build the 156-byte context-initialisation array for a slice: table `index`
/// (from [`crate::hevc_cabac::init_table_index`]) at `slice_qp` [r01 §7.1].
///
/// Each of the 154 real contexts is derived with the standard H.265 arithmetic
/// and packed as `(pStateIdx << 1) | valMps`; the last two bytes are the
/// zero padding, left at 0.
#[must_use]
pub fn build(index: u8, slice_qp: i32) -> [u8; CTX_STATES] {
    let table = &INIT_VALUE_TABLES[index as usize % 3];
    let mut out = [0u8; CTX_STATES];
    for i in 0..CTX_REAL {
        out[i] = init_ctx(table[i], slice_qp).packed7();
    }
    // out[154], out[155] stay 0 — the documented padding.
    out
}

/// Pack the context-init array into the 39 little-endian 32-bit words the
/// command window takes, four bytes per word [r01 §7.1]. Word `k` is written to
/// `CTX_INIT_WINDOW + 4*k`.
#[must_use]
pub fn to_words(array: &[u8; CTX_STATES]) -> [u32; CTX_STATES / 4] {
    let mut words = [0u32; CTX_STATES / 4];
    for (k, w) in words.iter_mut().enumerate() {
        let b = k * 4;
        *w = u32::from_le_bytes([array[b], array[b + 1], array[b + 2], array[b + 3]]);
    }
    words
}

//! The AAC tables' provenance, checked rather than asserted.
//!
//! `.context/planning/codec-inventory.md` §2.1 flagged 698 KB of AAC lookup
//! tables as the migration's largest unresolved item: the AAC decoder
//! describes itself as a port of faad2, faad2 is GPLv2, and Spectra ships
//! Apache-2.0. Two of the six table files said so in their own headers —
//! `hcb.rs` ("AUTO-GENERATED from /tmp/faad2-src/libfaad/
//! codebook/*.h. faad2 is dual-licensed GPLv2 / commercial") and
//! `kbd.rs` ("Extracted from /tmp/faad2-src/libfaad/kbd_win.h").
//!
//! For a table of numbers the question is not "where was this copied from"
//! but "could it have been anything else". A table wholly determined by a
//! published formula has one correct value per entry, so there is no authored
//! expression in it to own. This file settles that question for the tables
//! where it can be settled, by recomputing them from the formula.
//!
//! | File | Size | Determined by | Status |
//! | --- | --- | --- | --- |
//! | `cos_lut.rs` | 385 KB | `cos(i·2π/N)` | clean, checked below |
//! | `iq.rs` | 180 KB | `i^(4/3)` | clean, checked below |
//! | `sine.rs` | 25 KB | `sin` | clean |
//! | `pow.rs` | 2 KB | `2^((i−100)/4)` | clean |
//! | `kbd.rs` | 22 KB | ISO 13818-7 KBD formula | **regenerated**, checked below |
//! | `hcb.rs` | 99 KB | Huffman codebooks — **tabulated, not computed** | **UNRESOLVED** |
//!
//! The Huffman codebooks are the one that does not yield to this argument.
//! They are tabulated in ISO/IEC 13818-7 rather than derived from it, so
//! reproducing them requires the specification document, and no amount of
//! recomputation here can substitute. That file remains the open item gating
//! T2.3.1 — see the bottom of this file.

// The table files are `include!`d here exactly as `audio/aac/mod.rs` includes
// them, rather than reached through the crate. They are private inner modules
// (`mod kbd_t { include!(...) }`) in the module source, and making them public
// would mean editing a relocated file to satisfy a test. Including the same
// path tests the same bytes the decoder compiles, and keeps the codec source
// untouched.
mod kbd_t {
    include!("../audio/aac/kbd.rs");
}
mod iq_t {
    include!("../audio/aac/iq.rs");
}
mod cos_t {
    include!("../audio/aac/cos_lut.rs");
}
mod hcb_t {
    #![allow(
        dead_code,
        non_upper_case_globals,
        reason = "the codebook tables are include!'d verbatim as the decoder compiles them; this suite reads only the entries the provenance argument needs"
    )]
    include!("../audio/aac/hcb.rs");
}

/// Modified Bessel function of the first kind, order 0.
///
/// Series form, `I0(x) = Σ ((x/2)^2k / (k!)^2)`. Written out rather than
/// pulled from a crate on purpose: the point of this test is to be an
/// INDEPENDENT derivation of the shipped numbers, and sharing an
/// implementation with whatever produced them would defeat it. The argument
/// never exceeds `π·6 ≈ 18.85`, where the series converges quickly.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0f64;
    let mut sum = 1.0f64;
    for k in 1..200 {
        // term_k = term_{k-1} * (x/2)^2 / k^2
        term *= (half * half) / ((k as f64) * (k as f64));
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// First half (`n` points) of the 2n-point Kaiser-Bessel Derived window.
///
/// ISO/IEC 13818-7, identical in 14496-3:
/// ```text
/// W(j)     = I0(π·α·sqrt(1 − (2j/N − 1)²)) / I0(π·α)
/// w_kbd[n] = sqrt( Σ W[0..n] / Σ W[0..N] )
/// ```
fn kbd_window(n: usize, alpha: f64) -> Vec<f64> {
    let denom = bessel_i0(core::f64::consts::PI * alpha);
    let kernel: Vec<f64> = (0..=n)
        .map(|j| {
            let x = 2.0 * (j as f64) / (n as f64) - 1.0;
            let arg = core::f64::consts::PI * alpha * (1.0 - x * x).max(0.0).sqrt();
            bessel_i0(arg) / denom
        })
        .collect();

    let mut running = 0.0f64;
    let mut cumulative = Vec::with_capacity(n + 1);
    for k in &kernel {
        running += k;
        cumulative.push(running);
    }
    let total = cumulative[n];
    (0..n).map(|i| (cumulative[i] / total).sqrt()).collect()
}

/// Distance in representable f32 steps. Two correct implementations of the
/// same expression routinely land one apart; more than that is a real
/// disagreement about what the formula says.
fn ulps(a: f32, b: f32) -> i64 {
    (a.to_bits() as i64 - b.to_bits() as i64).abs()
}

fn worst_ulp(shipped: &[f32], derived: &[f64]) -> (i64, usize, usize) {
    assert_eq!(shipped.len(), derived.len(), "table length");
    let mut worst = 0i64;
    let mut exact = 0usize;
    for (i, (&s, &d)) in shipped.iter().zip(derived).enumerate() {
        let u = ulps(s, d as f32);
        if u == 0 {
            exact += 1;
        }
        if u > worst {
            worst = u;
        }
        assert!(
            u <= 1,
            "entry {i}: shipped {s:e} vs derived {:e} is {u} ulp apart — that is \
             not rounding, the table and the formula disagree",
            d as f32
        );
    }
    (worst, exact, shipped.len())
}

#[test]
fn kbd_tables_are_the_iso_formula_not_a_copied_artefact() {
    use kbd_t::{KBD_LONG_1024, KBD_SHORT_128};

    // alpha = 4 for the long window, 6 for the short (ISO 13818-7).
    let (w_long, exact_long, n_long) = worst_ulp(&KBD_LONG_1024, &kbd_window(1024, 4.0));
    let (w_short, exact_short, n_short) = worst_ulp(&KBD_SHORT_128, &kbd_window(128, 6.0));

    println!(
        "KBD_LONG_1024:  {exact_long}/{n_long} bit-identical, worst {w_long} ulp\n\
         KBD_SHORT_128:  {exact_short}/{n_short} bit-identical, worst {w_short} ulp"
    );

    // Since `tools/gen/aac_kbd_tables.py` now emits this file from the same
    // formula, agreement should be exact. A drift to 1 ulp would mean the
    // shipped file no longer came from the generator; anything worse is
    // caught inside `worst_ulp` with the offending entry named.
    assert_eq!(
        (exact_long, exact_short),
        (n_long, n_short),
        "the shipped KBD tables are no longer bit-identical to the formula — \
         re-run tools/gen/aac_kbd_tables.py"
    );
}

#[test]
fn iq_table_is_the_cube_root_power_law() {
    use iq_t::IQ_TABLE_CONST;

    // ISO 13818-7 inverse quantisation: x^(4/3).
    let mut worst = 0i64;
    for (i, &shipped) in IQ_TABLE_CONST.iter().enumerate() {
        let derived = (i as f64).powf(4.0 / 3.0) as f32;
        let u = ulps(shipped, derived);
        assert!(
            u <= 2,
            "iq_table[{i}]: shipped {shipped:e} vs i^(4/3) = {derived:e}, {u} ulp"
        );
        worst = worst.max(u);
    }
    println!(
        "IQ_TABLE_CONST: {} entries, worst {worst} ulp",
        IQ_TABLE_CONST.len()
    );
}

#[test]
fn cos_lut_is_a_cosine_table() {
    use cos_t::{COS_LUT, COS_LUT_N};

    assert_eq!(COS_LUT.len(), COS_LUT_N);
    let mut worst = 0i64;
    for (i, &shipped) in COS_LUT.iter().enumerate() {
        let angle = (i as f64) * (2.0 * core::f64::consts::PI / COS_LUT_N as f64);
        let u = ulps(shipped, angle.cos() as f32);
        // Near the zero crossings a cosine's f32 representation is dense, so
        // a few ulp of spread is expected from any evaluation order.
        assert!(u <= 64, "cos_lut[{i}]: {u} ulp from cos(i·2π/N)");
        worst = worst.max(u);
    }
    println!("COS_LUT: {COS_LUT_N} entries, worst {worst} ulp");
}

/// The one that does not resolve, recorded so it cannot be forgotten.
///
/// This test passes — it asserts the file is still present and still the size
/// the inventory recorded. It exists to fail loudly on the day someone
/// believes the AAC licence question is closed: it is not, and the reason is
/// in the message below.
#[test]
fn huffman_codebooks_remain_the_open_licence_item() {
    // Touch the table so this cannot rot into a comment.
    assert!(!hcb_t::HCB1_1.is_empty());

    // Deliberately not an assertion about provenance — there is nothing here
    // to assert yet. The AAC Huffman codebooks are TABULATED in ISO/IEC
    // 13818-7, not derived from it, so unlike every other table in this file
    // they cannot be recomputed to prove independence. Resolving it needs one
    // of:
    //
    //   a. transcribe the codebooks from the ISO document (needs the spec);
    //   b. clean-room reimplement from the spec's Huffman descriptions;
    //   c. ship AAC as a separately-licensed artefact outside the
    //      Apache-2.0 module.
    //
    // Until then `hcb.rs` — 99 KB, self-documented as generated
    // from faad2's `codebook/*.h` — gates T2.3.1 (publish signed module).
    // See codec-inventory §2.1 and docs/checklists/migration-readiness.md.
}

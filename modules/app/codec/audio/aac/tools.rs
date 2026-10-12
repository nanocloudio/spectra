//! Spectral reconstruction tools [spec r01 §7]: dequantisation, mid/side,
//! intensity stereo, perceptual noise substitution and temporal noise
//! shaping, applied in the order §7 gives, on the natural spectrum layout.
//!
//! All of it is integer arithmetic: a reconstructed coefficient is held as
//! Q`SPEC_FRAC`, every gain 2^(x/4) is an integer shift times a Q30 mantissa
//! from `POW2_FRAC`, and products are formed in i64 and rounded back.

use super::syntax::{
    Channel, MsInfo, Rate, CB_INTENSITY_IN, CB_INTENSITY_OUT, CB_NOISE, CB_ZERO, SEQ_EIGHT_SHORT,
    TNS_MAX_ORDER,
};
use super::tables::{IQ_CBRT, POW2_FRAC};

/// Fraction bits of a reconstructed spectral coefficient: the coefficient of
/// §7 times 2^4. The 2/N inverse of §8.1 pairs with a forward transform that
/// carries a factor of 2, so a block of in-range 16-bit audio is bounded by
/// 2 · 32768 · Σ W[n], largest for the long sine window at ≈ 2^26.35 — inside
/// the 2^27 an i32 holds at this scale. Values a malformed stream names past
/// that saturate rather than wrap. Rounding at 2^−5 lands far below one
/// output LSB once the IMDCT's 2/N has been applied.
pub const SPEC_FRAC: i32 = 4;

/// `v · 2^−sh` rounded to nearest (ties upward) and saturated to i32; a
/// negative `sh` shifts left.
pub fn round_shift(v: i64, sh: i32) -> i32 {
    if sh <= 0 {
        let s = (-sh).min(32) as u32;
        let lim = i64::from(i32::MAX) >> s;
        (v.clamp(-lim - 1, lim) << s) as i32
    } else if sh >= 63 {
        0
    } else {
        ((v + (1 << (sh - 1))) >> sh).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }
}

/// `v · 2^(x/4) · 2^−frac`, rounded and saturated, for |v| < 2^32: the
/// integer part of x/4 is a shift, the quarter power a Q30 mantissa
/// [spec r01 §7.1].
fn scale_quarter(v: i64, x: i32, frac: i32) -> i32 {
    round_shift(
        v * i64::from(POW2_FRAC[(x & 3) as usize]),
        30 + frac - (x >> 2),
    )
}

/// The coefficient range of band `b` in window `w` of a channel.
fn band_range(ch: &Channel, rate: &Rate, w: usize, b: usize) -> (usize, usize) {
    let (starts, _) = rate.bands(ch.info.sequence);
    let base = if ch.info.sequence == SEQ_EIGHT_SHORT {
        w * 128
    } else {
        0
    };
    (
        base + usize::from(starts[b]),
        base + usize::from(starts[b + 1]),
    )
}

/// First window of group `g`.
fn group_window0(ch: &Channel, g: usize) -> usize {
    ch.info.group_len[..g].iter().map(|&l| usize::from(l)).sum()
}

/// `sign(q) * |q|^(4/3) * 2^((sf - 100) / 4)` on every coded band, from
/// quantised integers to Q`SPEC_FRAC` [spec r01 §7.1]. `|q|^(4/3)` is
/// `|q| · ∛|q|`: the cube root is a Q26 table entry, which the gain mantissa
/// multiplies first, then the exact integer `|q|`.
pub fn dequantise(ch: &mut Channel, rate: &Rate) {
    for g in 0..usize::from(ch.info.num_groups) {
        let w0 = group_window0(ch, g);
        for b in 0..usize::from(ch.info.max_band) {
            let cb = ch.band_cb[g][b];
            if cb == CB_ZERO || cb >= CB_NOISE {
                continue;
            }
            let x = i32::from(ch.sf[g][b]) - 100;
            let mantissa = i64::from(POW2_FRAC[(x & 3) as usize]);
            let shift = 26 - SPEC_FRAC - (x >> 2);
            for w in w0..w0 + usize::from(ch.info.group_len[g]) {
                let (lo, hi) = band_range(ch, rate, w, b);
                for q in &mut ch.spectrum[lo..hi] {
                    let mag = q.unsigned_abs().min(8191) as usize;
                    // ∛|q| · 2^((x & 3)/4), Q26.
                    let root = (i64::from(IQ_CBRT[mag]) * mantissa + (1 << 29)) >> 30;
                    let v = round_shift(mag as i64 * root, shift);
                    *q = if *q < 0 { -v } else { v };
                }
            }
        }
    }
}

/// Mid/side on the bands the mask names [spec r01 §7.3].
pub fn mid_side(left: &mut Channel, right: &mut Channel, ms: &MsInfo, rate: &Rate) {
    if ms.mode == 0 {
        return;
    }
    for g in 0..usize::from(left.info.num_groups) {
        let w0 = group_window0(left, g);
        for b in 0..usize::from(left.info.max_band) {
            let rcb = right.band_cb[g][b];
            if ms.used[g][b] == 0
                || rcb == CB_INTENSITY_OUT
                || rcb == CB_INTENSITY_IN
                || left.band_cb[g][b] == CB_NOISE
            {
                continue;
            }
            for w in w0..w0 + usize::from(left.info.group_len[g]) {
                let (lo, hi) = band_range(left, rate, w, b);
                for k in lo..hi {
                    let (m, s) = (left.spectrum[k], right.spectrum[k]);
                    left.spectrum[k] = m.saturating_add(s);
                    right.spectrum[k] = m.saturating_sub(s);
                }
            }
        }
    }
}

/// Intensity stereo: the right channel's intensity bands become scaled
/// copies of the left [spec r01 §7.4]. An intensity band is inverted when
/// the M/S mask names it; `INVERT_UNDER_MS_MODE_2` says whether the all-bands
/// mask (mode 2) counts as naming every band, as the standard's syntax reads,
/// or only an explicit per-band mask (mode 1) does.
const INVERT_UNDER_MS_MODE_2: bool = true;

pub fn intensity(left: &Channel, right: &mut Channel, ms: &MsInfo, rate: &Rate) {
    for g in 0..usize::from(right.info.num_groups) {
        let w0 = group_window0(right, g);
        for b in 0..usize::from(right.info.max_band) {
            let cb = right.band_cb[g][b];
            if cb != CB_INTENSITY_OUT && cb != CB_INTENSITY_IN {
                continue;
            }
            // The copy is `sign · 2^(−p/4)` times the left coefficient.
            let p = i32::from(right.sf[g][b]).clamp(-120, 120);
            let mut sign = 1i64;
            if cb == CB_INTENSITY_OUT {
                sign = -sign;
            }
            let masked =
                ms.used[g][b] == 1 && (ms.mode == 1 || (ms.mode == 2 && INVERT_UNDER_MS_MODE_2));
            if masked {
                sign = -sign;
            }
            for w in w0..w0 + usize::from(right.info.group_len[g]) {
                let (lo, hi) = band_range(right, rate, w, b);
                for k in lo..hi {
                    right.spectrum[k] = scale_quarter(sign * i64::from(left.spectrum[k]), -p, 0);
                }
            }
        }
    }
}

/// A 32-bit xorshift generator: zero mean, no periodicity within a frame,
/// state kept across frames [spec r01 §7.5].
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Noise(pub u32);

impl Noise {
    pub const fn new() -> Self {
        Self(0x2545_F491)
    }

    /// The next value: the generator word's top 16 bits, a signed integer
    /// in −2^15..2^15.
    fn next(&mut self) -> i32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as i32) >> 16
    }
}

/// `1/√m` in Q30 for `m` in Q28 on [1, 4) [spec r01 §7.5 asks for the
/// value, not a method]. Newton steps `y ← y(3 − m·y²)/2` from the line
/// `y₀ = 1.5 − m/4`, which stays below √(3/m) on the whole range, so every
/// step converges; six take the worst start to full precision.
fn rsqrt_q30(m: i64) -> i64 {
    let mut y = (3 << 29) - m;
    for _ in 0..6 {
        let y2 = (y * y) >> 30;
        let my2 = (m * y2) >> 28;
        y = (y * ((3 << 30) - my2)) >> 31;
    }
    y
}

/// Fill one noise band of one window with raw generator values and return
/// the vector's energy.
fn fill_noise(spectrum: &mut [i32], rng: &mut Noise) -> u64 {
    let mut energy = 0u64;
    for v in spectrum.iter_mut() {
        let r = rng.next();
        *v = r;
        energy += u64::from(r.unsigned_abs() * r.unsigned_abs());
    }
    energy
}

/// Scale a band of raw noise values to energy `2^(e/2)`: each becomes
/// `r · 2^(e/4) / √energy`, in Q`SPEC_FRAC`.
fn scale_band(spectrum: &mut [i32], energy: u64, e: i32) {
    if energy == 0 {
        spectrum.fill(0);
        return;
    }
    // energy = m · 2^(2k) with m in [2^28, 2^30), read as Q28 on [1, 4);
    // then 1/√energy = rsqrt(m) · 2^−(14 + k).
    let bits = 64 - energy.leading_zeros() as i32;
    let two_k = (bits - 29) & !1;
    let m = if two_k >= 0 {
        energy >> two_k
    } else {
        energy << -two_k
    } as i64;
    // 2^((e & 3)/4) / √m, Q30.
    let gain = (rsqrt_q30(m) * i64::from(POW2_FRAC[(e & 3) as usize]) + (1 << 29)) >> 30;
    let shift = 30 + 14 + (two_k >> 1) - (e >> 2) - SPEC_FRAC;
    for v in spectrum.iter_mut() {
        *v = round_shift(i64::from(*v) * gain, shift);
    }
}

/// Noise substitution on an SCE or LFE [spec r01 §7.5].
pub fn noise_single(ch: &mut Channel, rate: &Rate, rng: &mut Noise) {
    for g in 0..usize::from(ch.info.num_groups) {
        let w0 = group_window0(ch, g);
        for b in 0..usize::from(ch.info.max_band) {
            if ch.band_cb[g][b] != CB_NOISE {
                continue;
            }
            let e = i32::from(ch.sf[g][b]).clamp(-120, 120);
            for w in w0..w0 + usize::from(ch.info.group_len[g]) {
                let (lo, hi) = band_range(ch, rate, w, b);
                let energy = fill_noise(&mut ch.spectrum[lo..hi], rng);
                scale_band(&mut ch.spectrum[lo..hi], energy, e);
            }
        }
    }
}

/// Noise substitution on a channel pair: a band noise-coded in both channels
/// with `ms_used` set shares one random vector [spec r01 §7.5].
pub fn noise_pair(
    left: &mut Channel,
    right: &mut Channel,
    ms: &MsInfo,
    rate: &Rate,
    rng: &mut Noise,
) {
    for g in 0..usize::from(left.info.num_groups) {
        let w0 = group_window0(left, g);
        for b in 0..usize::from(left.info.max_band) {
            let (ln, rn) = (
                left.band_cb[g][b] == CB_NOISE,
                right.band_cb[g][b] == CB_NOISE,
            );
            if !ln && !rn {
                continue;
            }
            let le = i32::from(left.sf[g][b]).clamp(-120, 120);
            let re = i32::from(right.sf[g][b]).clamp(-120, 120);
            let correlated = ln && rn && ms.mode != 0 && ms.used[g][b] == 1;
            for w in w0..w0 + usize::from(left.info.group_len[g]) {
                let (lo, hi) = band_range(left, rate, w, b);
                if correlated {
                    let energy = fill_noise(&mut left.spectrum[lo..hi], rng);
                    right.spectrum[lo..hi].copy_from_slice(&left.spectrum[lo..hi]);
                    scale_band(&mut left.spectrum[lo..hi], energy, le);
                    scale_band(&mut right.spectrum[lo..hi], energy, re);
                } else {
                    if ln {
                        let energy = fill_noise(&mut left.spectrum[lo..hi], rng);
                        scale_band(&mut left.spectrum[lo..hi], energy, le);
                    }
                    if rn {
                        let energy = fill_noise(&mut right.spectrum[lo..hi], rng);
                        scale_band(&mut right.spectrum[lo..hi], energy, re);
                    }
                }
            }
        }
    }
}

/// `a · r` for a Q31 `r`, rounded, for any `a` below 2^62: the product is
/// split at bit 31 so no partial product leaves i64.
fn mul_q31(a: i64, r: i32) -> i64 {
    let r = i64::from(r);
    (a >> 31) * r + (((a & 0x7FFF_FFFF) * r + (1 << 30)) >> 31)
}

/// Temporal noise shaping: each filter's reflection coefficients become
/// direct-form coefficients by the step-up recursion, then an all-pole filter
/// runs along the frequency axis of its window over its band range
/// [spec r01 §7.6]. The recursion runs in Q40 (an order-20 coefficient can
/// reach C(20,10) < 2^18); the filter takes its coefficients as i32 at the
/// finest Q that keeps its accumulator inside i64.
pub fn tns(ch: &mut Channel, rate: &Rate) {
    let sequence = ch.info.sequence;
    let (starts, num_bands) = rate.bands(sequence);
    let limit = rate.tns_limit(sequence);
    let max_band = usize::from(ch.info.max_band);
    let mut bottom_of = [num_bands; 8];
    for f in 0..usize::from(ch.tns_count) {
        let filter = ch.tns[f];
        let w = usize::from(filter.window);
        let top = bottom_of[w];
        let bottom = top.saturating_sub(usize::from(filter.length));
        bottom_of[w] = bottom;
        let order = usize::from(filter.order);
        if order == 0 {
            continue;
        }
        let base = if sequence == SEQ_EIGHT_SHORT {
            w * 128
        } else {
            0
        };
        let lo = base + usize::from(starts[bottom.min(limit).min(max_band)]);
        let hi = base + usize::from(starts[top.min(limit).min(max_band)]);
        if hi <= lo {
            continue;
        }
        // Step-up: a^(m) from a^(m-1) and refl[m-1]; a^(m-1) is read whole
        // before being overwritten.
        let mut a = [0i64; TNS_MAX_ORDER + 1];
        let mut prev = [0i64; TNS_MAX_ORDER + 1];
        a[0] = 1 << 40;
        for m in 1..=order {
            prev.copy_from_slice(&a);
            let r = filter.coef[m - 1];
            a[m] = i64::from(r) << 9;
            for i in 1..m {
                a[i] = prev[i] + mul_q31(prev[m - i], r);
            }
        }
        // With Σ|a| below 2^(31 − qa) in real terms, every accumulator of a
        // 32-bit sample and 32-bit history stays below 2^63.
        let total: i64 = a[..=order].iter().map(|v| v.abs()).sum();
        let qa = (71 - (64 - total.leading_zeros() as i32)).min(30);
        let mut coef = [0i32; TNS_MAX_ORDER + 1];
        for i in 1..=order {
            coef[i] = round_shift(a[i], 40 - qa);
        }
        // y[n] = x[n] - Σ a[i] y[n-i], memory cleared per run.
        let mut mem = [0i32; TNS_MAX_ORDER];
        let len = hi - lo;
        for step in 0..len {
            let k = if filter.direction == 0 {
                lo + step
            } else {
                hi - 1 - step
            };
            let mut acc = i64::from(ch.spectrum[k]) << qa;
            for i in 0..order {
                acc -= i64::from(coef[i + 1]) * i64::from(mem[i]);
            }
            let y = round_shift(acc, qa);
            for i in (1..order).rev() {
                mem[i] = mem[i - 1];
            }
            mem[0] = y;
            ch.spectrum[k] = y;
        }
    }
}

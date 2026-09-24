//! Spectral reconstruction tools [spec r01 §7]: dequantisation, mid/side,
//! intensity stereo, perceptual noise substitution and temporal noise
//! shaping, applied in the order §7 gives, on the natural spectrum layout.

use super::syntax::{
    Channel, MsInfo, Rate, CB_INTENSITY_IN, CB_INTENSITY_OUT, CB_NOISE, CB_ZERO, SEQ_EIGHT_SHORT,
    TNS_MAX_ORDER,
};
use super::tables::{IQ_TABLE, POW2_QUARTER};

/// `2^(x/4)` for x in −128..=160 [spec r01 §7.1].
fn pow2_quarter(x: i32) -> f32 {
    POW2_QUARTER[(x + 128).clamp(0, 288) as usize]
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

/// `sign(q) * |q|^(4/3) * 2^((sf - 100) / 4)` on every coded band [spec r01 §7.1].
pub fn dequantise(ch: &mut Channel, rate: &Rate) {
    for g in 0..usize::from(ch.info.num_groups) {
        let w0 = group_window0(ch, g);
        for b in 0..usize::from(ch.info.max_band) {
            let cb = ch.band_cb[g][b];
            if cb == CB_ZERO || cb >= CB_NOISE {
                continue;
            }
            let gain = pow2_quarter(i32::from(ch.sf[g][b]) - 100);
            for w in w0..w0 + usize::from(ch.info.group_len[g]) {
                let (lo, hi) = band_range(ch, rate, w, b);
                for q in &mut ch.spectrum[lo..hi] {
                    let mag = IQ_TABLE[(q.abs() as usize).min(8191)];
                    *q = if *q < 0.0 { -mag } else { mag } * gain;
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
                    left.spectrum[k] = m + s;
                    right.spectrum[k] = m - s;
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
            let p = i32::from(right.sf[g][b]).clamp(-120, 120);
            let mut scale = pow2_quarter(-p);
            if cb == CB_INTENSITY_OUT {
                scale = -scale;
            }
            let masked =
                ms.used[g][b] == 1 && (ms.mode == 1 || (ms.mode == 2 && INVERT_UNDER_MS_MODE_2));
            if masked {
                scale = -scale;
            }
            for w in w0..w0 + usize::from(right.info.group_len[g]) {
                let (lo, hi) = band_range(right, rate, w, b);
                for k in lo..hi {
                    right.spectrum[k] = scale * left.spectrum[k];
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

    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as i32) as f32 * (1.0 / 2_147_483_648.0)
    }
}

/// `1/sqrt(x)` for x > 0: an initial estimate from the exponent field, then
/// Newton steps [spec r01 §7.5 asks for the value, not a method].
fn rsqrt(x: f32) -> f32 {
    let i = 0x5F37_5A86u32.wrapping_sub(x.to_bits() >> 1);
    let mut y = f32::from_bits(i);
    for _ in 0..3 {
        y *= 1.5 - 0.5 * x * y * y;
    }
    y
}

/// Fill one noise band of one window and return the vector's energy.
fn fill_noise(spectrum: &mut [f32], rng: &mut Noise) -> f32 {
    let mut energy = 0.0;
    for v in spectrum.iter_mut() {
        let r = rng.next();
        *v = r;
        energy += r * r;
    }
    energy
}

fn scale_band(spectrum: &mut [f32], energy: f32, e: i32) {
    if energy <= 0.0 {
        for v in spectrum.iter_mut() {
            *v = 0.0;
        }
        return;
    }
    let gain = pow2_quarter(e) * rsqrt(energy);
    for v in spectrum.iter_mut() {
        *v *= gain;
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

/// Temporal noise shaping: each filter's reflection coefficients become
/// direct-form coefficients by the step-up recursion, then an all-pole filter
/// runs along the frequency axis of its window over its band range
/// [spec r01 §7.6].
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
        let mut a = [0.0f32; TNS_MAX_ORDER + 1];
        let mut prev = [0.0f32; TNS_MAX_ORDER + 1];
        a[0] = 1.0;
        for m in 1..=order {
            prev.copy_from_slice(&a);
            let r = filter.coef[m - 1];
            a[m] = r;
            for i in 1..m {
                a[i] = prev[i] + r * prev[m - i];
            }
        }
        // y[n] = x[n] - Σ a[i] y[n-i], memory cleared per run.
        let mut mem = [0.0f32; TNS_MAX_ORDER];
        let len = hi - lo;
        for step in 0..len {
            let k = if filter.direction == 0 {
                lo + step
            } else {
                hi - 1 - step
            };
            let mut y = ch.spectrum[k];
            for i in 0..order {
                y -= a[i + 1] * mem[i];
            }
            for i in (1..order).rev() {
                mem[i] = mem[i - 1];
            }
            mem[0] = y;
            ch.spectrum[k] = y;
        }
    }
}

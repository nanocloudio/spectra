//! The filterbank [spec r01 §8]: inverse MDCT, window shapes, window
//! sequences and overlap-add.
//!
//! The IMDCT of §8.1 is computed as a DCT-IV followed by an unfold with
//! signs, which follows from the definition by the symmetries
//! `y'[2M−1−m] = −y'[m]` and `y'[m+2M] = −y'[m]` of the cosine kernel; the
//! DCT-IV runs on an M/2-point complex FFT with the twiddle
//! `exp(−iπ(j+1/8)/M)` applied before and after. `tools/gen/aac_tables.py`
//! checks this decomposition against the direct definition on every run.
//! Every angle involved is a multiple of 2π/16384, so all twiddles and the
//! sine windows are read from one quarter-wave cosine table.
//!
//! Everything is fixed point. Twiddles and windows are Q30; the spectrum
//! arrives in the format `tools::SPEC_FRAC` names; time-domain samples leave
//! as Q`TIME_FRAC` PCM units (16-bit sample value times 2^12, sixteen times
//! full scale of headroom). The transform runs as block floating point: the
//! input is normalised so its peak sits just under 2^29, and a stage of the
//! FFT halves the whole buffer when its peak would otherwise reach 2^29, so
//! every rounding happens at about 29 bits below the block's own peak.

use super::syntax::{Channel, SEQ_EIGHT_SHORT, SEQ_LONG_START, SEQ_LONG_STOP};
use super::tables::{COS_QUARTER, KBD_LONG, KBD_SHORT};
use super::tools::round_shift;
pub use super::tools::SPEC_FRAC;

/// Fraction bits of a time-domain sample: PCM value times 2^12.
pub const TIME_FRAC: u32 = 12;

/// Peak magnitude a transform buffer may hold entering a step that can grow
/// it: a twiddle grows a component by at most √2 and a butterfly by at most
/// 1 + √2, which keeps 2^29 inside i32.
const PEAK_LIMIT: u32 = 1 << 29;

/// Working memory for one frame of one channel.
#[repr(C)]
pub struct Scratch {
    /// The windowed frame `z[0..2047]` [spec r01 §8.3], Q`TIME_FRAC`.
    pub z: [i32; 2048],
    /// DCT-IV output, in the block's floating scale.
    pub y: [i32; 1024],
    /// FFT real / imaginary parts, in the block's floating scale.
    pub fr: [i32; 512],
    pub fi: [i32; 512],
}

impl Scratch {
    pub const fn zero() -> Self {
        Self {
            z: [0; 2048],
            y: [0; 1024],
            fr: [0; 512],
            fi: [0; 512],
        }
    }
}

/// `cos(2π i / 16384)` in Q30 for any `i`, by quadrant symmetry of the
/// stored quarter wave [spec r01 §8.1, §8.2].
fn cos_idx(i: usize) -> i32 {
    let i = i % 16384;
    match i {
        0..=4096 => COS_QUARTER[i],
        4097..=8192 => -COS_QUARTER[8192 - i],
        8193..=12288 => -COS_QUARTER[i - 8192],
        _ => COS_QUARTER[16384 - i],
    }
}

fn sin_idx(i: usize) -> i32 {
    cos_idx(i + 16384 - 4096)
}

/// The sine window's rising half [spec r01 §8.2]: `sin(π(n+½)/N)`, Q30.
fn sine_window(n: usize, long: bool) -> i32 {
    if long {
        sin_idx(4 * n + 2)
    } else {
        sin_idx(32 * n + 16)
    }
}

/// Rising-half window value `W[n]` for a shape and size, Q30 [spec r01 §8.2].
pub fn window(shape: u8, long: bool, n: usize) -> i32 {
    match (shape, long) {
        (1, true) => KBD_LONG[n],
        (1, false) => KBD_SHORT[n],
        (_, l) => sine_window(n, l),
    }
}

/// `a * c + b * s` for Q30 `c`, `s`, rounded to the scale of `a` and `b`.
fn rotate(a: i32, b: i32, c: i32, s: i32) -> i32 {
    let p = i64::from(a) * i64::from(c) + i64::from(b) * i64::from(s);
    ((p + (1 << 29)) >> 30) as i32
}

/// `x * w` for a Q30 window value `w`, rounded to the scale of `x`.
fn windowed(x: i32, w: i32) -> i32 {
    ((i64::from(x) * i64::from(w) + (1 << 29)) >> 30) as i32
}

/// Largest component magnitude in the first `n` complex points.
fn peak(re: &[i32], im: &[i32], n: usize) -> u32 {
    let mut top = 0u32;
    for (&a, &b) in re[..n].iter().zip(&im[..n]) {
        top = top.max(a.unsigned_abs()).max(b.unsigned_abs());
    }
    top
}

/// In-place radix-2 complex FFT of `n` points (n a power of two ≤ 512),
/// forward (twiddle `exp(−2πi k/n)`). Before each stage whose input peak has
/// reached `PEAK_LIMIT` the buffer is halved; the result is the transform
/// times `2^−shifts`, and `shifts` is returned.
fn fft(re: &mut [i32], im: &mut [i32], n: usize) -> u32 {
    // Bit-reversal permutation.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut shifts = 0;
    let mut top = peak(re, im, n);
    let mut len = 2;
    while len <= n {
        while top >= PEAK_LIMIT {
            for v in re[..n].iter_mut().chain(im[..n].iter_mut()) {
                *v = (*v + 1) >> 1;
            }
            top = (top + 1) >> 1;
            shifts += 1;
        }
        top = 0;
        let half = len / 2;
        // 16384 / len as a shift, and the blocks walked by hand: a runtime
        // divisor or `step_by` would bring in a panic path the module cannot
        // link.
        let step = 16384 >> len.trailing_zeros();
        for k in 0..half {
            let (c, s) = (cos_idx(k * step), -sin_idx(k * step));
            let mut at = k;
            while at < n {
                let (ar, ai) = (re[at + half], im[at + half]);
                let tr = rotate(ar, -ai, c, s);
                let ti = rotate(ar, ai, s, c);
                let (br, bi) = (re[at], im[at]);
                let (p, q, u, v) = (br + tr, bi + ti, br - tr, bi - ti);
                re[at] = p;
                im[at] = q;
                re[at + half] = u;
                im[at + half] = v;
                top = top
                    .max(p.unsigned_abs())
                    .max(q.unsigned_abs())
                    .max(u.unsigned_abs())
                    .max(v.unsigned_abs());
                at += len;
            }
        }
        len *= 2;
    }
    shifts
}

/// DCT-IV of `m` points (1024 or 128): `y[k] = Σ X[j] cos(π/m (k+½)(j+½))`,
/// through an m/2-point FFT with twiddles `exp(−iπ(j+1/8)/m)`. The input is
/// first scaled by `2^norm`; the output is the DCT-IV of the scaled input
/// times `2^−shifts`, and `shifts` is returned.
fn dct4(x: &[i32], norm: i32, y: &mut [i32], m: usize, fr: &mut [i32], fi: &mut [i32]) -> u32 {
    let q = m / 2;
    // The twiddle angle π(j+1/8)/m = 2π(8j+1)/(16m); on the 16384 grid that
    // is (8j+1)·(1024/m).
    let unit = 1024 >> m.trailing_zeros();
    for j in 0..q {
        let idx = (8 * j + 1) * unit;
        let (c, s) = (cos_idx(idx), sin_idx(idx));
        let a = round_shift(i64::from(x[2 * j]), -norm);
        let b = round_shift(i64::from(x[m - 1 - 2 * j]), -norm);
        // (a + ib) · (c − is)
        fr[j] = rotate(a, b, c, s);
        fi[j] = rotate(b, -a, c, s);
    }
    // The FFT leaves every component below 2^29·(1 + √2), and the
    // post-twiddle grows one by at most √2, still inside i32.
    let shifts = fft(&mut fr[..q], &mut fi[..q], q);
    for j in 0..q {
        let idx = (8 * j + 1) * unit;
        let (c, s) = (cos_idx(idx), sin_idx(idx));
        let (a, b) = (fr[j], fi[j]);
        y[2 * j] = rotate(a, b, c, s);
        y[m - 1 - 2 * j] = -rotate(b, -a, c, s);
    }
    shifts
}

/// IMDCT of `m` coefficients (Q`SPEC_FRAC`) into `2m` samples
/// (Q`TIME_FRAC`) [spec r01 §8.1].
pub fn imdct(x: &[i32], out: &mut [i32], m: usize, scratch: &mut Scratch) {
    let n = 2 * m;
    let top = x[..m].iter().fold(0u32, |t, v| t.max(v.unsigned_abs()));
    if top == 0 {
        out[..n].fill(0);
        return;
    }
    // Normalise the peak into [2^28, 2^29).
    let norm = 29 - (32 - top.leading_zeros()) as i32;
    let shifts = dct4(
        x,
        norm,
        &mut scratch.y[..m],
        m,
        &mut scratch.fr,
        &mut scratch.fi,
    );
    // out = (2/N)·y, from the block scale 2^(norm − shifts) and Q`SPEC_FRAC`
    // to Q`TIME_FRAC`.
    let sh = norm + n.trailing_zeros() as i32 - 1 - shifts as i32 + SPEC_FRAC - TIME_FRAC as i32;
    let y = &scratch.y;
    for (i, o) in out.iter_mut().enumerate().take(n) {
        let v = if i < m / 2 {
            y[i + m / 2]
        } else if i < 3 * m / 2 {
            -y[3 * m / 2 - 1 - i]
        } else {
            -y[i - 3 * m / 2]
        };
        *o = round_shift(i64::from(v), sh);
    }
}

/// One channel's frame: window the transform per the sequence, overlap-add
/// with the previous frame's tail, produce 1024 samples and the new overlap,
/// all Q`TIME_FRAC` [spec r01 §8.3, §8.4].
pub fn frame(
    ch: &Channel,
    prev_shape: u8,
    overlap: &mut [i32; 1024],
    out: &mut [i32; 1024],
    scratch: &mut Scratch,
) {
    let cur = ch.info.shape;
    if ch.info.sequence == SEQ_EIGHT_SHORT {
        scratch.z = [0; 2048];
        let mut xs = [0i32; 256];
        for j in 0..8 {
            imdct(&ch.spectrum[j * 128..j * 128 + 128], &mut xs, 128, scratch);
            let rising = if j == 0 { prev_shape } else { cur };
            let at = 448 + 128 * j;
            for n in 0..128 {
                let z = &mut scratch.z;
                z[at + n] = z[at + n].saturating_add(windowed(xs[n], window(rising, false, n)));
                z[at + 128 + n] = z[at + 128 + n]
                    .saturating_add(windowed(xs[128 + n], window(cur, false, 127 - n)));
            }
        }
    } else {
        let mut x = [0i32; 2048];
        imdct(&ch.spectrum, &mut x, 1024, scratch);
        let z = &mut scratch.z;
        match ch.info.sequence {
            SEQ_LONG_START => {
                for n in 0..1024 {
                    z[n] = windowed(x[n], window(prev_shape, true, n));
                }
                z[1024..1472].copy_from_slice(&x[1024..1472]);
                for n in 0..128 {
                    z[1472 + n] = windowed(x[1472 + n], window(cur, false, 127 - n));
                }
                z[1600..2048].fill(0);
            }
            SEQ_LONG_STOP => {
                z[..448].fill(0);
                for n in 0..128 {
                    z[448 + n] = windowed(x[448 + n], window(prev_shape, false, n));
                }
                z[576..1024].copy_from_slice(&x[576..1024]);
                for n in 0..1024 {
                    z[1024 + n] = windowed(x[1024 + n], window(cur, true, 1023 - n));
                }
            }
            _ => {
                for n in 0..1024 {
                    z[n] = windowed(x[n], window(prev_shape, true, n));
                    z[1024 + n] = windowed(x[1024 + n], window(cur, true, 1023 - n));
                }
            }
        }
    }
    for n in 0..1024 {
        out[n] = overlap[n].saturating_add(scratch.z[n]);
        overlap[n] = scratch.z[1024 + n];
    }
}

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

use super::syntax::{Channel, SEQ_EIGHT_SHORT, SEQ_LONG_START, SEQ_LONG_STOP};
use super::tables::{COS_QUARTER, KBD_LONG, KBD_SHORT};

/// Working memory for one frame of one channel.
#[repr(C)]
pub struct Scratch {
    /// The windowed frame `z[0..2047]` [spec r01 §8.3].
    pub z: [f32; 2048],
    /// DCT-IV output.
    pub y: [f32; 1024],
    /// FFT real / imaginary parts.
    pub fr: [f32; 512],
    pub fi: [f32; 512],
}

impl Scratch {
    pub const fn zero() -> Self {
        Self {
            z: [0.0; 2048],
            y: [0.0; 1024],
            fr: [0.0; 512],
            fi: [0.0; 512],
        }
    }
}

/// `cos(2π i / 16384)` for any `i`, by quadrant symmetry of the stored
/// quarter wave [spec r01 §8.1, §8.2].
fn cos_idx(i: usize) -> f32 {
    let i = i % 16384;
    match i {
        0..=4096 => COS_QUARTER[i],
        4097..=8192 => -COS_QUARTER[8192 - i],
        8193..=12288 => -COS_QUARTER[i - 8192],
        _ => COS_QUARTER[16384 - i],
    }
}

fn sin_idx(i: usize) -> f32 {
    cos_idx(i + 16384 - 4096)
}

/// The sine window's rising half [spec r01 §8.2]: `sin(π(n+½)/N)`.
fn sine_window(n: usize, long: bool) -> f32 {
    if long {
        sin_idx(4 * n + 2)
    } else {
        sin_idx(32 * n + 16)
    }
}

/// Rising-half window value `W[n]` for a shape and size [spec r01 §8.2].
pub fn window(shape: u8, long: bool, n: usize) -> f32 {
    match (shape, long) {
        (1, true) => KBD_LONG[n],
        (1, false) => KBD_SHORT[n],
        (_, l) => sine_window(n, l),
    }
}

/// In-place radix-2 complex FFT of `n` points (n a power of two ≤ 512),
/// forward (twiddle `exp(−2πi k/n)`).
fn fft(re: &mut [f32], im: &mut [f32], n: usize) {
    // Bit-reversal permutation.
    let bits = n.trailing_zeros();
    for i in 0..n {
        let j = i.reverse_bits() >> (usize::BITS - bits);
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let half = len / 2;
        let step = 16384 / len;
        for start in (0..n).step_by(len) {
            for k in 0..half {
                let (c, s) = (cos_idx(k * step), -sin_idx(k * step));
                let (ar, ai) = (re[start + k + half], im[start + k + half]);
                let tr = ar * c - ai * s;
                let ti = ar * s + ai * c;
                let (br, bi) = (re[start + k], im[start + k]);
                re[start + k] = br + tr;
                im[start + k] = bi + ti;
                re[start + k + half] = br - tr;
                im[start + k + half] = bi - ti;
            }
        }
        len *= 2;
    }
}

/// DCT-IV of `m` points (1024 or 128): `y[k] = Σ X[j] cos(π/m (k+½)(j+½))`,
/// through an m/2-point FFT with twiddles `exp(−iπ(j+1/8)/m)`.
fn dct4(x: &[f32], y: &mut [f32], m: usize, fr: &mut [f32], fi: &mut [f32]) {
    let q = m / 2;
    // The twiddle angle π(j+1/8)/m = 2π(8j+1)/(16m); on the 16384 grid that
    // is (8j+1)·(1024/m).
    let unit = 1024 / m;
    for j in 0..q {
        let idx = (8 * j + 1) * unit;
        let (c, s) = (cos_idx(idx), sin_idx(idx));
        let (a, b) = (x[2 * j], x[m - 1 - 2 * j]);
        // (a + ib) · (c − is)
        fr[j] = a * c + b * s;
        fi[j] = b * c - a * s;
    }
    fft(fr, fi, q);
    for j in 0..q {
        let idx = (8 * j + 1) * unit;
        let (c, s) = (cos_idx(idx), sin_idx(idx));
        let (a, b) = (fr[j], fi[j]);
        let wr = a * c + b * s;
        let wi = b * c - a * s;
        y[2 * j] = wr;
        y[m - 1 - 2 * j] = -wi;
    }
}

/// IMDCT of `m` coefficients into `2m` samples [spec r01 §8.1].
pub fn imdct(x: &[f32], out: &mut [f32], m: usize, scratch: &mut Scratch) {
    let n = 2 * m;
    dct4(x, &mut scratch.y[..m], m, &mut scratch.fr, &mut scratch.fi);
    let y = &scratch.y;
    let scale = 2.0 / n as f32;
    for (i, o) in out.iter_mut().enumerate().take(n) {
        *o = if i < m / 2 {
            scale * y[i + m / 2]
        } else if i < 3 * m / 2 {
            -scale * y[3 * m / 2 - 1 - i]
        } else {
            -scale * y[i - 3 * m / 2]
        };
    }
}

/// One channel's frame: window the transform per the sequence, overlap-add
/// with the previous frame's tail, produce 1024 samples and the new overlap
/// [spec r01 §8.3, §8.4].
pub fn frame(
    ch: &Channel,
    prev_shape: u8,
    overlap: &mut [f32; 1024],
    out: &mut [f32; 1024],
    scratch: &mut Scratch,
) {
    let cur = ch.info.shape;
    if ch.info.sequence == SEQ_EIGHT_SHORT {
        scratch.z = [0.0; 2048];
        let mut xs = [0.0f32; 256];
        for j in 0..8 {
            imdct(&ch.spectrum[j * 128..j * 128 + 128], &mut xs, 128, scratch);
            let rising = if j == 0 { prev_shape } else { cur };
            let at = 448 + 128 * j;
            for n in 0..128 {
                scratch.z[at + n] += xs[n] * window(rising, false, n);
                scratch.z[at + 128 + n] += xs[128 + n] * window(cur, false, 127 - n);
            }
        }
    } else {
        let mut x = [0.0f32; 2048];
        imdct(&ch.spectrum, &mut x, 1024, scratch);
        let z = &mut scratch.z;
        match ch.info.sequence {
            SEQ_LONG_START => {
                for n in 0..1024 {
                    z[n] = x[n] * window(prev_shape, true, n);
                }
                z[1024..1472].copy_from_slice(&x[1024..1472]);
                for n in 0..128 {
                    z[1472 + n] = x[1472 + n] * window(cur, false, 127 - n);
                }
                z[1600..2048].fill(0.0);
            }
            SEQ_LONG_STOP => {
                z[..448].fill(0.0);
                for n in 0..128 {
                    z[448 + n] = x[448 + n] * window(prev_shape, false, n);
                }
                z[576..1024].copy_from_slice(&x[576..1024]);
                for n in 0..1024 {
                    z[1024 + n] = x[1024 + n] * window(cur, true, 1023 - n);
                }
            }
            _ => {
                for n in 0..1024 {
                    z[n] = x[n] * window(prev_shape, true, n);
                    z[1024 + n] = x[1024 + n] * window(cur, true, 1023 - n);
                }
            }
        }
    }
    for n in 0..1024 {
        out[n] = overlap[n] + scratch.z[n];
        overlap[n] = scratch.z[1024 + n];
    }
}

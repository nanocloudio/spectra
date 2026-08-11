//! Nearest-neighbour source-coordinate mapping, shared by every family that
//! resamples a raster.
//!
//! # Why this is one file and not five
//!
//! It used to be five. The four image formats each heap-allocated a `dst_w`
//! entry `u32` map and filled it with the same expression; the video path
//! wrote a different expression inline. That is five chances to disagree, and
//! they did — image sampled from pixel CENTRES, video from pixel CORNERS.
//!
//! The distinction is not cosmetic. Corner sampling,
//! `src = (d * src_len) / dst_len`, takes the top-left corner of each
//! destination pixel's footprint, which shifts the whole image up and left by
//! half a destination pixel and, on upscale, duplicates the first source row
//! and column while dropping the last. Centre sampling,
//! `src = ((2d + 1) * src_len) / (2 * dst_len)`, takes the middle, which is
//! symmetric and is what every other resampler means by "nearest neighbour".
//! Centre is the one kept here.
//!
//! # Why it is division-free
//!
//! The obvious loop evaluates that expression per output pixel. On the video
//! path that was two software divisions per pixel — 332k per 768x432 frame,
//! on the one path that has to sustain 25 fps. Splitting the constant step
//! into its integer and fractional parts ONCE turns the per-pixel cost into
//! an add, a compare, and a predictable subtract:
//!
//! ```text
//!     src(d+1) - src(d)  ==  step + (1 if the remainder carries)
//! ```
//!
//! which is exactly Bresenham's line algorithm. `Nearest` is that, with the
//! division done in [`Nearest::new`] and nowhere else.
//!
//! The image formats keep no map at all now, which also drops one heap
//! allocation per decode from each of the four.

use core::num::NonZeroUsize;

/// Walks destination coordinates, yielding the source coordinate each one
/// samples from.
///
/// Exactly equal to `((2 * d + 1) * src_len) / (2 * dst_len)` for every
/// `d` in `0..dst_len`, without evaluating that division per step.
#[derive(Clone, Copy)]
pub struct Nearest {
    /// Current source coordinate.
    pos: usize,
    /// Fractional part of the current position, over `den`.
    rem: usize,
    /// Integer part of one destination step.
    step: usize,
    /// Fractional part of one destination step, over `den`.
    step_rem: usize,
    /// Common denominator, `2 * dst_len`.
    ///
    /// `NonZeroUsize` rather than `usize`, and that is load-bearing on the
    /// PIC targets rather than mere documentation. `2 * dst_len` can wrap to
    /// zero for a `dst_len` past `usize::MAX / 2`, so with a plain `usize`
    /// LLVM keeps a division-by-zero panic branch — and
    /// `core::panicking::panic_const_div_by_zero` has no definition to link
    /// against in a `no_std` PIC module. The bare-metal build fails at the
    /// linker; the host build never notices. Dividing by a `NonZeroUsize`
    /// cannot panic, so no such branch is emitted.
    den: NonZeroUsize,
    /// `pos` / `rem` at d = 0, kept so a per-row [`restart`](Self::restart)
    /// costs no division.
    pos0: usize,
    rem0: usize,
}

impl Nearest {
    /// Map `dst_len` destination samples onto `src_len` source samples.
    ///
    /// `dst_len == 0` yields a walker that is never stepped (the caller's
    /// loop is then empty), so it is safe rather than a division by zero.
    pub fn new(src_len: usize, dst_len: usize) -> Self {
        // Saturating, not wrapping: see the `den` field. `NonZeroUsize::MIN`
        // covers both `dst_len == 0` and the (unreachable in practice —
        // dimensions arrive as u16) overflow case, and in each the walker is
        // either never stepped or degenerate rather than trapping.
        let den = NonZeroUsize::new(dst_len.saturating_mul(2)).unwrap_or(NonZeroUsize::MIN);
        // A zero-length destination samples nothing, and its `den` fell back
        // to 1 above — which would divide out to `src_len`, an index one past
        // the end of the source. Zero the numerators so the walker is inert
        // AND in range, rather than merely unread-in-practice.
        let (num0, twice_src) = if dst_len == 0 {
            (0, 0)
        } else {
            // d = 0 has numerator `src_len`; each step adds `2 * src_len`.
            (src_len, src_len.saturating_mul(2))
        };
        let pos0 = num0 / den;
        let rem0 = num0 % den;
        Self {
            pos: pos0,
            rem: rem0,
            step: twice_src / den,
            step_rem: twice_src % den,
            den,
            pos0,
            rem0,
        }
    }

    /// Source coordinate for the current destination coordinate.
    #[inline(always)]
    pub fn index(&self) -> usize {
        self.pos
    }

    /// Advance one destination coordinate.
    #[inline(always)]
    pub fn advance(&mut self) {
        self.pos += self.step;
        self.rem += self.step_rem;
        // `step_rem` and `rem` are both < den, so their sum carries at most
        // once — a single compare, never a loop.
        if self.rem >= self.den.get() {
            self.rem -= self.den.get();
            self.pos += 1;
        }
    }

    /// Rewind to the first destination coordinate. Used to restart the column
    /// walk on each new row, where the mapping is identical.
    #[inline(always)]
    pub fn restart(&mut self) {
        self.pos = self.pos0;
        self.rem = self.rem0;
    }
}

//! RBSP bit reader shared by the H.264 and H.265 front ends.
//!
//! Both standards use the same low-level syntax layer — big-endian bit
//! packing, unsigned and signed exponential-Golomb, and the emulation
//! prevention byte — so it lives once here rather than twice in the codecs.
//!
//! # Emulation prevention
//!
//! A NAL payload may not contain the byte sequences `00 00 00`, `00 00 01`,
//! `00 00 02` or `00 00 03`, because a decoder scanning for start codes would
//! mistake them. Encoders insert a `0x03` after any two zero bytes to prevent
//! it; a reader must remove it again. Failing to strip it does not fail
//! loudly — it silently shifts every subsequent bit, so a stream parses to
//! plausible-looking nonsense. That is why it is handled here, in the one
//! place both codecs share, rather than in each parser.
//!
//! # Failure model
//!
//! Every read is bounds-checked and returns `None` past the end. Nothing
//! panics, nothing wraps, and no read can advance past the buffer — the input
//! is a byte stream from a file, so `docs/specification.md`'s requirement that
//! malformed input produce a bounded error applies to every method here.
//!
//! Exp-Golomb codes are additionally capped: a run of zero bits longer than
//! 32 is refused rather than shifted into oblivion, because a corrupt stream
//! can otherwise ask for a 4-billion-bit code word.

/// Bit-level reader over an RBSP, stripping emulation prevention bytes.
pub struct RbspReader<'a> {
    data: &'a [u8],
    /// Next bit position, counted over the *emulation-stripped* stream.
    bit: usize,
    /// Byte offset in `data` of the next unconsumed input byte.
    byte_pos: usize,
    /// Consecutive zero bytes seen, for emulation detection.
    zeros: u8,
    /// Small decoded window: bits are served from here.
    cache: u64,
    /// Valid bits currently in `cache`, most-significant-first.
    cache_bits: u8,
    /// Set once the input is exhausted.
    exhausted: bool,
}

impl<'a> RbspReader<'a> {
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            bit: 0,
            byte_pos: 0,
            zeros: 0,
            cache: 0,
            cache_bits: 0,
            exhausted: false,
        }
    }

    /// Bits consumed so far, over the emulation-stripped stream.
    #[must_use]
    pub fn bits_read(&self) -> usize {
        self.bit
    }

    /// Pull the next emulation-stripped byte, or `None` at end of input.
    fn next_byte(&mut self) -> Option<u8> {
        while self.byte_pos < self.data.len() {
            let b = self.data[self.byte_pos];
            self.byte_pos += 1;
            if self.zeros >= 2 && b == 0x03 {
                // Emulation prevention byte: drop it, and reset the run so
                // `00 00 03 00 00 03` strips both.
                self.zeros = 0;
                continue;
            }
            self.zeros = if b == 0 {
                self.zeros.saturating_add(1)
            } else {
                0
            };
            return Some(b);
        }
        self.exhausted = true;
        None
    }

    /// Ensure at least `n` bits are in the cache. Returns false at end of
    /// input.
    fn fill(&mut self, n: u8) -> bool {
        while self.cache_bits < n {
            match self.next_byte() {
                Some(b) => {
                    self.cache = (self.cache << 8) | u64::from(b);
                    self.cache_bits += 8;
                }
                None => return false,
            }
        }
        true
    }

    /// Read `n` bits (0..=32) as an unsigned value.
    pub fn u(&mut self, n: u8) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        if n > 32 || !self.fill(n) {
            return None;
        }
        let shift = self.cache_bits - n;
        let v = (self.cache >> shift) & ((1u64 << n) - 1);
        self.cache_bits -= n;
        self.cache &= (1u64 << self.cache_bits) - 1;
        self.bit += n as usize;
        u32::try_from(v).ok()
    }

    /// Read a single flag.
    pub fn flag(&mut self) -> Option<bool> {
        Some(self.u(1)? != 0)
    }

    /// Skip `n` bits, in chunks so `n > 32` is allowed.
    pub fn skip(&mut self, n: usize) -> Option<()> {
        let mut left = n;
        while left > 0 {
            let take = if left > 32 { 32 } else { left as u8 };
            self.u(take)?;
            left -= take as usize;
        }
        Some(())
    }

    /// Unsigned exponential-Golomb, `ue(v)`.
    ///
    /// The leading-zero run is capped at 32: a corrupt stream can otherwise
    /// encode a code word longer than the buffer, and the natural loop would
    /// read to the end before failing.
    pub fn ue(&mut self) -> Option<u32> {
        let mut leading = 0u32;
        while self.u(1)? == 0 {
            leading += 1;
            if leading > 32 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        let rest = self.u(leading as u8)?;
        // (1 << leading) - 1 + rest, without overflowing at leading == 32.
        let base = if leading >= 32 {
            u32::MAX
        } else {
            (1u32 << leading) - 1
        };
        base.checked_add(rest)
    }

    /// Signed exponential-Golomb, `se(v)`.
    pub fn se(&mut self) -> Option<i32> {
        let k = self.ue()?;
        // k = 0 -> 0, odd -> positive, even -> negative.
        let magnitude = k.div_ceil(2);
        let m = i32::try_from(magnitude).ok()?;
        Some(if k % 2 == 1 { m } else { -m })
    }
}

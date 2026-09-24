//! Bounds-checked MSB-first bit reader [spec r01 §1.2].
//!
//! Every read is checked against the end of the data: running out of bits is
//! a decode fault, never a silent zero [spec r01 §10]. Positions are counted
//! in bits from the start of the slice, which is the start of the enclosing
//! structure for every "byte-align" the syntax asks for.

use super::Fault;

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Bits not yet read.
    pub fn left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    /// Current position in bits from the start.
    pub const fn pos(&self) -> usize {
        self.pos
    }

    /// Read an unsigned field of `n` bits, 0 < n <= 32.
    pub fn read(&mut self, n: u32) -> Result<u32, Fault> {
        let n = n as usize;
        if n == 0 {
            return Ok(0);
        }
        if n > self.left() {
            return Err(Fault::Truncated);
        }
        let mut v: u32 = 0;
        let mut got = 0;
        while got < n {
            let byte = self.data[self.pos / 8];
            let bit_off = self.pos % 8;
            let avail = 8 - bit_off;
            let take = (n - got).min(avail);
            let chunk = (u32::from(byte) >> (avail - take)) & ((1 << take) - 1);
            v = (v << take) | chunk;
            self.pos += take;
            got += take;
        }
        Ok(v)
    }

    pub fn bit(&mut self) -> Result<bool, Fault> {
        Ok(self.read(1)? != 0)
    }

    pub fn skip(&mut self, bits: usize) -> Result<(), Fault> {
        if bits > self.left() {
            return Err(Fault::Truncated);
        }
        self.pos += bits;
        Ok(())
    }

    /// Advance to the next multiple of 8 bits from the start of the data.
    pub fn align(&mut self) -> Result<(), Fault> {
        let rem = self.pos % 8;
        if rem != 0 {
            self.skip(8 - rem)?;
        }
        Ok(())
    }
}

//! Where a streaming sub-codec's bitstream comes from.
//!
//! The audio decoders were written against one source: bytes read off the
//! module's `encoded` channel, a container or bare essence stream sniffed by
//! the root. The encoded-media record stream (`audio_in`) is a second source —
//! the root unpacks each `UNIT` and hands the decoder the bitstream bytes — and
//! the decoders must not care which. [`Input`] is that seam: a channel, or the
//! root's [`ByteFifo`], behind the same `read` / `poll` the decoders already
//! used on the channel.
//!
//! The FIFO lives in the root's state, which the runtime never moves, so the
//! pointer a decoder holds stays valid for as long as the decoder does. No
//! statics: PIC modules have none.

use super::abi::SyscallTable;
use super::{POLL_HUP, POLL_IN};

/// Bitstream bytes staged between the record pump and a decoder: a few ADTS or
/// MP3 frames of lead.
pub const FIFO_CAP: usize = 16 * 1024;

#[repr(C)]
pub struct ByteFifo {
    head: u32,
    len: u32,
    /// The stream ended (`END`): once drained, the source reads as hung up.
    ended: u8,
    _pad: [u8; 3],
    buf: [u8; FIFO_CAP],
}

impl ByteFifo {
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.ended = 0;
    }

    pub fn free(&self) -> usize {
        FIFO_CAP - self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Mark the end of the stream.
    pub fn end(&mut self) {
        self.ended = 1;
    }

    pub fn ended_and_drained(&self) -> bool {
        self.ended != 0 && self.len == 0
    }

    /// Append `bytes` whole, or nothing: a bitstream split by a refused tail
    /// would read as corruption, not as backpressure.
    pub fn push(&mut self, bytes: &[u8]) -> bool {
        if bytes.len() > self.free() {
            return false;
        }
        let mut tail = (self.head as usize + self.len as usize) % FIFO_CAP;
        for &b in bytes {
            self.buf[tail] = b;
            tail = (tail + 1) % FIFO_CAP;
        }
        self.len += bytes.len() as u32;
        true
    }

    fn read_into(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.len as usize);
        for slot in out.iter_mut().take(n) {
            *slot = self.buf[self.head as usize];
            self.head = (self.head + 1) % FIFO_CAP as u32;
        }
        self.len -= n as u32;
        n
    }
}

/// A decoder's input: the channel it was given, or the root's FIFO.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Input {
    chan: i32,
    fifo: *mut ByteFifo,
}

impl Input {
    pub const fn channel(chan: i32) -> Self {
        Self {
            chan,
            fifo: core::ptr::null_mut(),
        }
    }

    pub fn fifo(fifo: *mut ByteFifo) -> Self {
        Self { chan: -1, fifo }
    }

    /// `channel_read`'s contract: bytes read, 0 when there are none.
    ///
    /// # Safety
    /// `buf` is valid for `len` bytes; a FIFO input's pointer is live.
    pub unsafe fn read(&self, sys: &SyscallTable, buf: *mut u8, len: usize) -> i32 {
        if self.fifo.is_null() {
            return (sys.channel_read)(self.chan, buf, len);
        }
        let out = core::slice::from_raw_parts_mut(buf, len);
        (*self.fifo).read_into(out) as i32
    }

    /// `channel_poll`'s contract for `POLL_IN` and `POLL_HUP`.
    ///
    /// # Safety
    /// A FIFO input's pointer is live.
    pub unsafe fn poll(&self, sys: &SyscallTable, events: u32) -> i32 {
        if self.fifo.is_null() {
            return (sys.channel_poll)(self.chan, events);
        }
        let f = &*self.fifo;
        let mut ready = 0u32;
        if !f.is_empty() {
            ready |= POLL_IN;
        }
        if f.ended_and_drained() {
            ready |= POLL_HUP;
        }
        (ready & events) as i32
    }
}

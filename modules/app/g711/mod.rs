//! G.711 µ-law telephony codec PIC module — the bidirectional PCM ↔ µ-law
//! bridge for a two-party voice path.
//!
//! Two independent one-way flows share the module:
//!   * **encode** — `pcm_in` (stereo PCM) averaged to mono, companded to µ-law,
//!     written to `ulaw_out` (feeds the RTP transmitter).
//!   * **decode** — `ulaw_in` (µ-law from the jitter buffer's playout) expanded
//!     to linear samples, duplicated L=R, written to `pcm_out` (feeds playout).
//!
//! The companding math lives in `modules/common/g711.rs` and is shared
//! verbatim with the host tests; this module is the channel wrapper. Relocated
//! from `fluxor/modules/app/voip` (the encode/decode halves) under Conclave plan
//! S4.2 (`Move G.711 to Spectra`, T4.2.2 module wrapping).

#![cfg_attr(not(feature = "host-test"), no_std)]
#![allow(
    dead_code,
    unused_imports,
    unreachable_patterns,
    reason = "PIC build path-mounts modules/sdk/* via include!/mod, so each module's compile sees the full ABI surface; consumers use a subset"
)]

use core::ffi::c_void;

#[path = "../../../target/fluxor/fluxor-abi/sdk/abi.rs"]
mod abi;
use abi::SyscallTable;

include!("../../../target/fluxor/fluxor-abi/sdk/runtime.rs");

// The companding core, mounted by `#[path]` so the module and the host tests
// compile the same bytes. Host-test builds expose it; PIC builds keep it private.
#[cfg(feature = "host-test")]
#[path = "../../common/g711.rs"]
pub mod g711;
#[cfg(not(feature = "host-test"))]
#[path = "../../common/g711.rs"]
mod g711;

/// Max µ-law bytes consumed per decode step (→ 4× PCM bytes out).
const DEC_IN_MAX: usize = 64;
/// PCM staging for one decode step: 64 µ-law → 64 stereo i16 = 256 bytes.
const DEC_OUT_BUF_SIZE: usize = 256;
/// Stereo PCM staging for one encode step.
const ENC_IN_BUF_SIZE: usize = 256;
/// µ-law staging for one encode step (256 stereo bytes = 64 samples).
const ENC_OUT_BUF_SIZE: usize = 64;

#[repr(C)]
struct G711State {
    syscalls: *const SyscallTable,

    pcm_in: i32,   // in[0]: stereo PCM to encode
    ulaw_out: i32, // out[0]: µ-law to the RTP transmitter
    ulaw_in: i32,  // in[1]: µ-law from playout
    pcm_out: i32,  // out[1]: stereo PCM to the speaker

    // Backpressure staging: bytes still owed to each output channel.
    enc_pending_out: u16,
    enc_pending_offset: u16,
    /// Bytes of an incomplete stereo PCM frame held over from the previous
    /// `step_encode`, parked at the front of `enc_in_buf` (0..=3).
    enc_carry_len: u8,
    _pad0: [u8; 3],
    dec_pending_out: u16,
    dec_pending_offset: u16,

    enc_in_buf: [u8; ENC_IN_BUF_SIZE],
    enc_out_buf: [u8; ENC_OUT_BUF_SIZE],
    dec_in_buf: [u8; DEC_IN_MAX],
    dec_out_buf: [u8; DEC_OUT_BUF_SIZE],
}

impl G711State {
    fn init(&mut self, syscalls: *const SyscallTable) {
        self.syscalls = syscalls;
        self.pcm_in = -1;
        self.ulaw_out = -1;
        self.ulaw_in = -1;
        self.pcm_out = -1;
        self.enc_pending_out = 0;
        self.enc_pending_offset = 0;
        self.enc_carry_len = 0;
        self.dec_pending_out = 0;
        self.dec_pending_offset = 0;
    }
}

// `drain_pending` / `track_pending` come from the SDK runtime (format.rs);
// `pending_out` tracks bytes still owed, `pending_offset` the write position.

/// encode: `pcm_in` (stereo) → mono µ-law → `ulaw_out`.
unsafe fn step_encode(s: &mut G711State) {
    let sys = &*s.syscalls;
    if s.pcm_in < 0 || s.ulaw_out < 0 {
        return;
    }
    if !drain_pending(
        sys,
        s.ulaw_out,
        s.enc_out_buf.as_ptr(),
        &mut s.enc_pending_out,
        &mut s.enc_pending_offset,
    ) {
        return;
    }
    let out_poll = (sys.channel_poll)(s.ulaw_out, POLL_OUT);
    if out_poll <= 0 || (out_poll as u32) & POLL_OUT == 0 {
        return;
    }
    let in_poll = (sys.channel_poll)(s.pcm_in, POLL_IN);
    if in_poll <= 0 || (in_poll as u32) & POLL_IN == 0 {
        return;
    }
    // A stereo PCM frame is 4 bytes, but `channel_read` is a byte-stream
    // read: it returns what is in the ring, not a whole number of frames.
    // Any 1–3 byte tail must be CARRIED, not dropped — discarding it would
    // shift every later frame in the stream by that many bytes, swapping
    // L/R and reading each sample across a frame boundary, silently and
    // permanently. Producers today write whole frames so the tail is
    // normally 0, but nothing in the channel contract promises that and
    // the failure mode is noise rather than an error.
    let carry = s.enc_carry_len as usize;
    let read = (sys.channel_read)(
        s.pcm_in,
        s.enc_in_buf.as_mut_ptr().add(carry),
        ENC_IN_BUF_SIZE - carry,
    );
    if read <= 0 {
        return;
    }
    let have = carry + read as usize;
    let samples = have / 4;
    let tail = have % 4;
    if samples == 0 {
        s.enc_carry_len = have as u8;
        return;
    }
    for i in 0..samples {
        let off = i * 4;
        let left = i16::from_le_bytes([s.enc_in_buf[off], s.enc_in_buf[off + 1]]);
        let right = i16::from_le_bytes([s.enc_in_buf[off + 2], s.enc_in_buf[off + 3]]);
        let mono = ((left as i32 + right as i32) / 2) as i16;
        s.enc_out_buf[i] = g711::ulaw_encode(mono);
    }
    // Move the partial frame to the front so the next read appends to it.
    for i in 0..tail {
        s.enc_in_buf[i] = s.enc_in_buf[samples * 4 + i];
    }
    s.enc_carry_len = tail as u8;
    let written = (sys.channel_write)(s.ulaw_out, s.enc_out_buf.as_ptr(), samples);
    track_pending(
        written,
        samples,
        &mut s.enc_pending_out,
        &mut s.enc_pending_offset,
    );
}

/// decode: `ulaw_in` → linear samples duplicated L=R → `pcm_out`.
unsafe fn step_decode(s: &mut G711State) {
    let sys = &*s.syscalls;
    if s.ulaw_in < 0 || s.pcm_out < 0 {
        return;
    }
    if !drain_pending(
        sys,
        s.pcm_out,
        s.dec_out_buf.as_ptr(),
        &mut s.dec_pending_out,
        &mut s.dec_pending_offset,
    ) {
        return;
    }
    let out_poll = (sys.channel_poll)(s.pcm_out, POLL_OUT);
    if out_poll <= 0 || (out_poll as u32) & POLL_OUT == 0 {
        return;
    }
    let in_poll = (sys.channel_poll)(s.ulaw_in, POLL_IN);
    if in_poll <= 0 || (in_poll as u32) & POLL_IN == 0 {
        return;
    }
    let read = (sys.channel_read)(s.ulaw_in, s.dec_in_buf.as_mut_ptr(), DEC_IN_MAX);
    if read <= 0 {
        return;
    }
    let count = read as usize;
    for i in 0..count {
        let sample = g711::ulaw_decode(s.dec_in_buf[i]);
        let b = sample.to_le_bytes();
        let o = i * 4;
        s.dec_out_buf[o] = b[0];
        s.dec_out_buf[o + 1] = b[1];
        s.dec_out_buf[o + 2] = b[0];
        s.dec_out_buf[o + 3] = b[1];
    }
    let out_bytes = count * 4;
    let written = (sys.channel_write)(s.pcm_out, s.dec_out_buf.as_ptr(), out_bytes);
    track_pending(
        written,
        out_bytes,
        &mut s.dec_pending_out,
        &mut s.dec_pending_offset,
    );
}

#[no_mangle]
#[link_section = ".text.module_state_size"]
pub extern "C" fn module_state_size() -> u32 {
    core::mem::size_of::<G711State>() as u32
}

#[no_mangle]
#[link_section = ".text.module_init"]
pub extern "C" fn module_init(_syscalls: *const c_void) {}

#[no_mangle]
#[link_section = ".text.module_new"]
pub extern "C" fn module_new(
    in_chan: i32,
    out_chan: i32,
    _ctrl_chan: i32,
    _params: *const u8,
    _params_len: usize,
    state: *mut u8,
    state_size: usize,
    syscalls: *const c_void,
) -> i32 {
    unsafe {
        if syscalls.is_null() {
            return -2;
        }
        if state.is_null() || state_size < core::mem::size_of::<G711State>() {
            return -5;
        }
        let s = &mut *(state as *mut G711State);
        s.init(syscalls as *const SyscallTable);
        let sys = &*s.syscalls;

        s.pcm_in = in_chan; // in[0]
        s.ulaw_out = out_chan; // out[0]
        let ch = dev_channel_port(sys, 0, 1);
        if ch >= 0 {
            s.ulaw_in = ch; // in[1]
        }
        let ch = dev_channel_port(sys, 1, 1);
        if ch >= 0 {
            s.pcm_out = ch; // out[1]
        }
        0
    }
}

#[no_mangle]
#[link_section = ".text.module_step"]
pub extern "C" fn module_step(state: *mut u8) -> i32 {
    unsafe {
        if state.is_null() {
            return -1;
        }
        let s = &mut *(state as *mut G711State);
        if s.syscalls.is_null() {
            return -1;
        }
        step_encode(s);
        step_decode(s);
        0
    }
}

include!("../../../target/fluxor/fluxor-abi/sdk/runtime/wasm_entry.rs");

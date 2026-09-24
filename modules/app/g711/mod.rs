//! G.711 telephony codec PIC module — the bidirectional PCM ↔ PCMU/PCMA
//! bridge for a two-party voice path.
//!
//! Two independent one-way flows share the module:
//!   * **encode** — `pcm_in` (stereo 8 kHz PCM) averaged to mono, companded
//!     under the `codec` parameter's law (µ-law or A-law), and emitted on
//!     `encoded_out` as the fluxor encoded-media record stream
//!     (`abi::contracts::encoded`): one `STREAM` naming PCMU or PCMA at 8 kHz,
//!     then one `UNIT` per `ptime` of audio, stamped in samples.
//!   * **decode** — `encoded_in` (a PCMU or PCMA record stream, e.g. from
//!     Wave's `jitter`; the `STREAM` record says which) expanded to linear
//!     samples, duplicated L=R, written to `pcm_out`. A `DISCONTINUITY` unit
//!     is preceded by silence for the samples its `pts` says were lost —
//!     concealment is the decoder's job, and silence is G.711's (RFC 3551
//!     defines no PLC for either law).
//!
//! The companding math lives in `modules/common/g711.rs` and is shared
//! verbatim with the host tests; this module is the stream wrapper.

#![cfg_attr(not(feature = "host-test"), no_std)]
#![allow(
    dead_code,
    unused_imports,
    unreachable_patterns,
    reason = "PIC build path-mounts modules/sdk/* via include!/mod, so each module's compile sees the full ABI surface; consumers use a subset"
)]
#![allow(
    clippy::not_unsafe_ptr_arg_deref,
    reason = "the module ABI entry points take raw state, params and syscall pointers whose validity is the runtime's half of the contract; the signature is fixed by that contract rather than chosen here"
)]

use core::ffi::c_void;

#[path = "../../../target/fluxor/fluxor-abi/sdk/abi.rs"]
mod abi;
use abi::contracts::encoded as enc;
use abi::SyscallTable;

include!("../../../target/fluxor/fluxor-abi/sdk/runtime.rs");
include!("../../../target/fluxor/fluxor-abi/sdk/runtime/params.rs");

// The companding core, mounted by `#[path]` so the module and the host tests
// compile the same bytes. Host-test builds expose it; PIC builds keep it private.
#[cfg(feature = "host-test")]
#[path = "../../common/g711.rs"]
pub mod g711;
#[cfg(not(feature = "host-test"))]
#[path = "../../common/g711.rs"]
mod g711;

/// G.711's clock (RFC 3551 §4.5.14, §4.5.1): 8 kHz, one byte per sample.
const G711_CLOCK: u32 = 8_000;
/// `codec` when the parameter named neither law; `module_new` refuses it.
const CODEC_UNSUPPORTED: u8 = 0xFF;
/// Samples per millisecond at that clock.
const SAMPLES_PER_MS: usize = 8;
/// Largest `ptime`: 40 ms, the most a single encoded unit carries — the
/// `max_payload` fact on `encoded_out`.
const PTIME_MAX_MS: usize = 40;
const UNIT_MAX: usize = PTIME_MAX_MS * SAMPLES_PER_MS;

/// Largest unit payload `encoded_in` accepts — its `max_payload` fact, one
/// RTP packet's worth.
const DEC_PAYLOAD_MAX: usize = 1460;
/// Carry for the incoming record stream: one whole record at most.
const DEC_CARRY: usize = enc::UNIT_HEADER + DEC_PAYLOAD_MAX;
/// Decoded PCM staging: one unit, stereo i16.
const DEC_OUT_BUF: usize = DEC_PAYLOAD_MAX * 4;
/// Largest gap concealed with silence. A longer one is a stream that stopped
/// and restarted, not a loss, and inventing seconds of silence for it would
/// only delay what follows.
const CONCEAL_MAX_SAMPLES: u32 = G711_CLOCK;

/// Stereo PCM staging for one encode read: whole 4-byte frames plus a carried
/// partial one.
const ENC_IN_BUF: usize = 256;
/// One `STREAM` record for PCMU (no config).
const ENC_STREAM_LEN: usize = enc::STREAM_HEADER;
/// Encoded staging: a `STREAM` (once) plus one `UNIT`.
const ENC_OUT_BUF: usize = ENC_STREAM_LEN + enc::UNIT_HEADER + UNIT_MAX;

#[repr(C)]
struct G711State {
    syscalls: *const SyscallTable,

    pcm_in: i32,      // in[0]: stereo PCM to encode
    encoded_out: i32, // out[0]: PCMU record stream
    encoded_in: i32,  // in[1]: PCMU record stream
    pcm_out: i32,     // out[1]: stereo PCM

    // ── encode ──
    ptime_ms: u8,
    /// The law encode companding uses: `CODEC_PCMU` or `CODEC_PCMA`.
    codec: u8,
    /// A `STREAM` record has gone out.
    enc_opened: u8,
    /// Bytes of an incomplete stereo PCM frame held over from the previous
    /// read, parked at the front of `enc_in_buf` (0..=3).
    enc_carry_len: u8,
    _pad0: u8,
    /// µ-law samples accumulated toward the next unit.
    enc_unit_len: u16,
    /// Bytes of `enc_out_buf` still owed to `encoded_out`.
    enc_pending_out: u16,
    enc_pending_offset: u16,
    _pad1: u16,
    /// `pts` of the next unit: samples emitted so far.
    enc_pts: i64,

    // ── decode ──
    /// The open input stream is G.711 at 8 kHz mono.
    dec_stream_ok: u8,
    /// The open input stream's law.
    dec_codec: u8,
    _pad2: [u8; 2],
    dec_carry_len: u16,
    dec_pending_out: u16,
    dec_pending_offset: u16,
    _pad3: u16,
    /// Silence samples still owed ahead of the pending unit.
    dec_conceal_owed: u32,
    /// `pts` the next unit should carry if nothing was lost; `None` until the
    /// stream's first unit.
    dec_expect_pts: Option<i64>,
    dec_sequence: enc::Sequence,
    /// Streams refused (not PCMU at 8 kHz mono) and faults. Counted, never
    /// silent.
    dec_refused: u32,
    dec_faults: u32,

    enc_in_buf: [u8; ENC_IN_BUF],
    enc_unit: [u8; UNIT_MAX],
    enc_out_buf: [u8; ENC_OUT_BUF],
    dec_carry: [u8; DEC_CARRY],
    dec_out_buf: [u8; DEC_OUT_BUF],
}

impl G711State {
    fn init(&mut self, syscalls: *const SyscallTable) {
        self.syscalls = syscalls;
        self.pcm_in = -1;
        self.encoded_out = -1;
        self.encoded_in = -1;
        self.pcm_out = -1;
        self.ptime_ms = 20;
        self.codec = enc::CODEC_PCMU;
        self.enc_opened = 0;
        self.enc_carry_len = 0;
        self.enc_unit_len = 0;
        self.enc_pending_out = 0;
        self.enc_pending_offset = 0;
        self.enc_pts = 0;
        self.dec_stream_ok = 0;
        self.dec_codec = enc::CODEC_PCMU;
        self.dec_carry_len = 0;
        self.dec_pending_out = 0;
        self.dec_pending_offset = 0;
        self.dec_conceal_owed = 0;
        self.dec_expect_pts = None;
        self.dec_sequence = enc::Sequence::new();
        self.dec_refused = 0;
        self.dec_faults = 0;
    }

    fn unit_samples(&self) -> usize {
        self.ptime_ms as usize * SAMPLES_PER_MS
    }
}

mod params_def {
    use super::enc;
    use super::G711State;
    use super::SCHEMA_MAX;
    use super::{p_u8, CODEC_UNSUPPORTED, PTIME_MAX_MS};

    define_params! {
        G711State;
        // Milliseconds of audio per encoded unit: 10, 20, 30 or 40. Anything
        // else reads as the RFC 3551 default of 20.
        1, ptime, u8, 20 => |s, d, len| {
            let v = p_u8(d, len, 0, 20);
            s.ptime_ms = if v >= 10 && v as usize <= PTIME_MAX_MS && v.is_multiple_of(10) { v } else { 20 };
        };
        // The law encode uses: `pcmu` (µ-law, the default) or `pcma` (A-law).
        // Absent (the default dispatch passes no bytes) leaves µ-law.
        2, codec, str, 0 => |s, d, len| {
            if len != 0 {
                s.codec = match core::slice::from_raw_parts(d, len) {
                    b"pcmu" => enc::CODEC_PCMU,
                    b"pcma" => enc::CODEC_PCMA,
                    _ => CODEC_UNSUPPORTED,
                };
            }
        };
    }
}

// `drain_pending` / `track_pending` come from the SDK runtime (format.rs);
// `pending_out` tracks bytes still owed, `pending_offset` the write position.

/// encode: `pcm_in` (stereo) → mono µ-law units → `encoded_out`.
unsafe fn step_encode(s: &mut G711State) {
    let sys = &*s.syscalls;
    if s.pcm_in < 0 || s.encoded_out < 0 {
        return;
    }
    if !drain_pending(
        sys,
        s.encoded_out,
        s.enc_out_buf.as_ptr(),
        &mut s.enc_pending_out,
        &mut s.enc_pending_offset,
    ) {
        return;
    }
    let in_poll = (sys.channel_poll)(s.pcm_in, POLL_IN);
    if in_poll <= 0 || (in_poll as u32) & POLL_IN == 0 {
        return;
    }
    // Read no more whole frames than complete the current unit, so a unit is
    // emitted the step it fills and nothing is left over to stage.
    let need = s.unit_samples() - s.enc_unit_len as usize;
    let carry = s.enc_carry_len as usize;
    let want = (need * 4).min(ENC_IN_BUF) - carry.min(need * 4);
    // A stereo PCM frame is 4 bytes, but `channel_read` is a byte-stream read:
    // any 1–3 byte tail is CARRIED, never dropped — dropping it would shift
    // every later frame, swapping L/R, silently and permanently.
    let read = (sys.channel_read)(s.pcm_in, s.enc_in_buf.as_mut_ptr().add(carry), want);
    if read <= 0 {
        return;
    }
    let have = carry + read as usize;
    let frames = have / 4;
    for i in 0..frames {
        let off = i * 4;
        let left = i16::from_le_bytes([s.enc_in_buf[off], s.enc_in_buf[off + 1]]);
        let right = i16::from_le_bytes([s.enc_in_buf[off + 2], s.enc_in_buf[off + 3]]);
        let mono = ((left as i32 + right as i32) / 2) as i16;
        s.enc_unit[s.enc_unit_len as usize + i] = if s.codec == enc::CODEC_PCMA {
            g711::alaw_encode(mono)
        } else {
            g711::ulaw_encode(mono)
        };
    }
    let tail = have % 4;
    for i in 0..tail {
        s.enc_in_buf[i] = s.enc_in_buf[frames * 4 + i];
    }
    s.enc_carry_len = tail as u8;
    s.enc_unit_len += frames as u16;
    if (s.enc_unit_len as usize) < s.unit_samples() {
        return;
    }

    let mut at = 0;
    if s.enc_opened == 0 {
        at += enc::write_stream(
            &mut s.enc_out_buf,
            s.codec,
            enc::PACKING_RAW,
            1,
            G711_CLOCK,
            &[],
        )
        .unwrap_or(0);
        s.enc_opened = 1;
    }
    let len = s.enc_unit_len as usize;
    at += enc::write_unit(
        &mut s.enc_out_buf[at..],
        enc::FLAG_KEY,
        s.enc_pts,
        0,
        &s.enc_unit[..len],
    )
    .unwrap_or(0);
    s.enc_pts += len as i64;
    s.enc_unit_len = 0;
    let written = (sys.channel_write)(s.encoded_out, s.enc_out_buf.as_ptr(), at);
    track_pending(
        written,
        at,
        &mut s.enc_pending_out,
        &mut s.enc_pending_offset,
    );
}

/// decode: `encoded_in` records → linear samples duplicated L=R → `pcm_out`.
unsafe fn step_decode(s: &mut G711State) {
    let sys = &*s.syscalls;
    if s.encoded_in < 0 || s.pcm_out < 0 {
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
    // Silence owed for lost samples goes out before the unit that revealed
    // the loss, one buffer per step.
    if s.dec_conceal_owed > 0 {
        let n = (s.dec_conceal_owed as usize).min(DEC_OUT_BUF / 4);
        s.dec_out_buf[..n * 4].fill(0);
        s.dec_conceal_owed -= n as u32;
        let written = (sys.channel_write)(s.pcm_out, s.dec_out_buf.as_ptr(), n * 4);
        track_pending(
            written,
            n * 4,
            &mut s.dec_pending_out,
            &mut s.dec_pending_offset,
        );
        return;
    }

    let len = s.dec_carry_len as usize;
    if len < DEC_CARRY {
        let n = (sys.channel_read)(
            s.encoded_in,
            s.dec_carry.as_mut_ptr().add(len),
            DEC_CARRY - len,
        );
        if n > 0 {
            s.dec_carry_len += n as u16;
        }
    }
    let carried = s.dec_carry_len as usize;
    let (record, consumed) = match enc::parse(&s.dec_carry[..carried], DEC_CARRY) {
        enc::Parse::NeedMore => return,
        enc::Parse::Fault(_) => {
            reset_decode_stream(s);
            return;
        }
        enc::Parse::Record { record, consumed } => (record, consumed),
    };
    // A unit arriving after a loss: owe the silence the gap in `pts` implies
    // and leave the unit where it is — it decodes once the silence is out.
    if let enc::Record::Unit(u) = record {
        if s.dec_stream_ok != 0 && u.flags & enc::FLAG_DISCONTINUITY != 0 {
            if let Some(expect) = s.dec_expect_pts {
                let gap = u.pts - expect;
                if gap > 0 && gap <= i64::from(CONCEAL_MAX_SAMPLES) {
                    s.dec_conceal_owed = gap as u32;
                    s.dec_expect_pts = Some(u.pts);
                    return;
                }
            }
        }
    }
    if s.dec_sequence.admit(&record).is_err() {
        reset_decode_stream(s);
        return;
    }
    let out_bytes = match record {
        enc::Record::Stream(st) => {
            let ok = matches!(st.codec, enc::CODEC_PCMU | enc::CODEC_PCMA)
                && st.packing == enc::PACKING_RAW
                && st.clock_rate == G711_CLOCK
                && st.channels == 1;
            s.dec_stream_ok = u8::from(ok);
            s.dec_codec = st.codec;
            s.dec_expect_pts = None;
            if !ok {
                s.dec_refused = s.dec_refused.wrapping_add(1);
            }
            0
        }
        enc::Record::Unit(u) if s.dec_stream_ok != 0 => {
            s.dec_expect_pts = Some(u.pts + u.payload.len() as i64);
            decode_unit(&mut s.dec_out_buf, s.dec_codec, &u)
        }
        enc::Record::Unit(_) => 0,
        enc::Record::End => {
            s.dec_stream_ok = 0;
            s.dec_expect_pts = None;
            0
        }
    };
    s.dec_carry.copy_within(consumed..carried, 0);
    s.dec_carry_len = (carried - consumed) as u16;
    if out_bytes > 0 {
        let written = (sys.channel_write)(s.pcm_out, s.dec_out_buf.as_ptr(), out_bytes);
        track_pending(
            written,
            out_bytes,
            &mut s.dec_pending_out,
            &mut s.dec_pending_offset,
        );
    }
}

/// Expand one unit's G.711 octets into stereo PCM in `out`, returning its
/// bytes. A G.711 unit is one fragment; a fragmented or truncated one is not
/// an access unit and is dropped.
fn decode_unit(out: &mut [u8; DEC_OUT_BUF], codec: u8, u: &enc::Unit<'_>) -> usize {
    if u.flags & (enc::FLAG_TRUNCATED | enc::FLAG_CONTINUES) != 0
        || u.payload.len() > DEC_PAYLOAD_MAX
    {
        return 0;
    }
    let expand = if codec == enc::CODEC_PCMA {
        g711::alaw_decode
    } else {
        g711::ulaw_decode
    };
    for (i, &b) in u.payload.iter().enumerate() {
        let sample = expand(b).to_le_bytes();
        out[i * 4..i * 4 + 4].copy_from_slice(&[sample[0], sample[1], sample[0], sample[1]]);
    }
    u.payload.len() * 4
}

/// Forget the input stream after a fault — the record boundary is lost, or
/// the producer broke the record order; the producer must open a new stream.
fn reset_decode_stream(s: &mut G711State) {
    s.dec_faults = s.dec_faults.wrapping_add(1);
    s.dec_carry_len = 0;
    s.dec_sequence = enc::Sequence::new();
    s.dec_stream_ok = 0;
    s.dec_expect_pts = None;
}

#[cfg_attr(not(feature = "host-test"), no_mangle)]
#[link_section = ".text.module_state_size"]
pub extern "C" fn module_state_size() -> u32 {
    core::mem::size_of::<G711State>() as u32
}

#[cfg_attr(not(feature = "host-test"), no_mangle)]
#[link_section = ".text.module_init"]
pub extern "C" fn module_init(_syscalls: *const c_void) {}

#[cfg_attr(not(feature = "host-test"), no_mangle)]
#[link_section = ".text.module_new"]
pub extern "C" fn module_new(
    in_chan: i32,
    out_chan: i32,
    _ctrl_chan: i32,
    params: *const u8,
    params_len: usize,
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
        s.encoded_out = out_chan; // out[0]
        s.encoded_in = dev_channel_port(sys, 0, 1); // in[1]
        s.pcm_out = dev_channel_port(sys, 1, 1); // out[1]

        let is_tlv =
            !params.is_null() && params_len >= 4 && *params == 0xFE && *params.add(1) == 0x01;
        if is_tlv {
            params_def::parse_tlv(s, params, params_len);
        } else {
            params_def::set_defaults(s);
        }
        if s.codec == CODEC_UNSUPPORTED {
            let m = b"[g711] refusing to construct: codec must be pcmu or pcma";
            dev_log(sys, 1, m.as_ptr(), m.len());
            return -22;
        }
        0
    }
}

#[cfg_attr(not(feature = "host-test"), no_mangle)]
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

//! AAC-LC decoder — a clean-room implementation from the decoder
//! specification in the planning repository
//! (`.context/clean_room/aac/spec/aac_lc_r01_released.md`). Every constant
//! cites the section or table it comes from as `[spec r01 §x.y]`; the tables
//! in `tables.rs` are generated from the specification by
//! `tools/gen/aac_tables.py`.
//!
//! Input is an ADTS stream [spec r01 §2.1] read through the codec's `Input`
//! seam — a file's bytes, or raw units the record pump has wrapped in ADTS
//! headers from their AudioSpecificConfig. Output is 16-bit PCM, interleaved
//! in the stream's channel order [spec r01 §9], one raw data block (1024
//! samples per channel) per step.
//!
//! What this decoder covers: MPEG-2 and MPEG-4 AAC-LC, every syntax element
//! (coupling parsed and discarded, data and fill skipped, PCE parsed), and
//! every LC tool — pulses, M/S, intensity, noise substitution, TNS, all four
//! window sequences with both window shapes. What it bounds: output layouts
//! of one or two channels (channel configurations 1 and 2); a stream with
//! another configuration is refused, with a log line and no output.

mod bits;
// The filterbank is reachable from the host harness, which checks the IMDCT
// against the direct definition.
#[cfg(not(feature = "host-test"))]
mod filterbank;
#[cfg(feature = "host-test")]
pub mod filterbank;
mod syntax;
mod tables;
mod tools;

use super::abi::SyscallTable;
use super::input::Input;
use super::{dev_log, drain_pending, track_pending, POLL_IN};
use bits::BitReader;
use filterbank::Scratch;
use syntax::{
    Channel, MsInfo, Rate, ID_CCE, ID_CPE, ID_DSE, ID_END, ID_FIL, ID_LFE, ID_PCE, ID_SCE,
};
use tools::Noise;

/// Why a frame could not be decoded [spec r01 §10].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The data ended inside a field.
    Truncated,
    /// A field or structure violates a stated limit.
    Malformed,
    /// A feature outside the LC profile is present.
    NotLc,
    /// A channel layout this build does not carry.
    Unsupported,
}

/// ADTS frames are at most 8191 bytes [spec r01 §2.1]; the input carry holds
/// one complete frame plus the start of the next.
const IN_BUF: usize = 16384;
/// Output channels this build decodes.
const MAX_CHANNELS: usize = 2;
/// Channels parsed: the two outputs plus one scratch for elements that are
/// decoded to stay in sync and dropped [spec r01 §3.1, §3.4].
const PARSED_CHANNELS: usize = 3;
/// PCM staging: one block of every output channel, 16-bit.
const PCM_BUF: usize = 1024 * MAX_CHANNELS * 2;
/// Largest input read per step.
const READ_CHUNK: usize = 2048;

/// An ADTS header [spec r01 §2.1].
#[derive(Clone, Copy)]
#[repr(C)]
struct Adts {
    crc: bool,
    sampling_index: u8,
    channel_config: u8,
    blocks: u8,
    /// Whole frame length in bytes, header included.
    frame_length: u16,
    /// Header bytes: 7, or 9 with a CRC.
    header_len: u8,
}

impl Adts {
    const fn zero() -> Self {
        Self {
            crc: false,
            sampling_index: 0,
            channel_config: 0,
            blocks: 1,
            frame_length: 0,
            header_len: 7,
        }
    }
}

fn parse_adts(b: &[u8]) -> Option<Adts> {
    if b.len() < 7 || b[0] != 0xFF || (b[1] & 0xF6) != 0xF0 {
        return None;
    }
    let profile = b[2] >> 6;
    let sampling_index = (b[2] >> 2) & 0x0F;
    let channel_config = ((b[2] & 0x01) << 2) | (b[3] >> 6);
    let frame_length =
        (u16::from(b[3] & 0x03) << 11) | (u16::from(b[4]) << 3) | u16::from(b[5] >> 5);
    let blocks = (b[6] & 0x03) + 1;
    let crc = b[1] & 0x01 == 0;
    let header_len = if crc { 9 } else { 7 };
    if profile != 1 || sampling_index > 12 || frame_length < u16::from(header_len) {
        return None;
    }
    Some(Adts {
        crc,
        sampling_index,
        channel_config,
        blocks,
        frame_length,
        header_len,
    })
}

/// The decoding state proper, separate from the input carry so a frame can
/// be read out of the carry while this is mutated.
#[repr(C)]
struct Decoder {
    rate_index: u8,
    /// Output channels: 0 until the first frame configures the stream.
    channels: u8,
    prev_shape: [u8; MAX_CHANNELS],
    pcm: [u8; PCM_BUF],
    noise: Noise,
    ms: MsInfo,
    chans: [Channel; PARSED_CHANNELS],
    overlap: [[f32; 1024]; MAX_CHANNELS],
    time: [f32; 1024],
    scratch: Scratch,
}

#[repr(C)]
pub struct AacState {
    pub syscalls: *const SyscallTable,
    pub input: Input,
    pub out_chan: i32,

    // ── input carry ──
    in_buf: [u8; IN_BUF],
    in_len: u32,
    /// A frame's header has been accepted and its blocks are being decoded.
    frame_open: u8,
    /// Raw data blocks of the current frame already decoded.
    blocks_done: u8,
    channel_config: u8,
    /// A refusal has been logged for this stream's layout.
    refused_logged: u8,
    /// Byte offset of the next block from the start of the frame.
    block_at: u16,
    header: Adts,
    frames_decoded: u32,
    frames_faulted: u32,

    // ── output ──
    pending_out: u16,
    pending_offset: u16,

    dec: Decoder,
}

pub unsafe fn aac_init(
    s: &mut AacState,
    syscalls: *const SyscallTable,
    input: Input,
    out_chan: i32,
) {
    core::ptr::write_bytes(
        s as *mut AacState as *mut u8,
        0,
        core::mem::size_of::<AacState>(),
    );
    s.syscalls = syscalls;
    s.input = input;
    s.out_chan = out_chan;
    s.header = Adts::zero();
    s.dec.noise = Noise::new();
    s.dec.ms = MsInfo::zero();
    for c in &mut s.dec.chans {
        *c = Channel::zero();
    }
    s.dec.scratch = Scratch::zero();
}

/// Replay the bytes format detection consumed.
pub unsafe fn aac_feed_detect(s: &mut AacState, buf: *const u8, len: usize) {
    let bytes = core::slice::from_raw_parts(buf, len);
    let at = s.in_len as usize;
    let n = len.min(IN_BUF - at);
    s.in_buf[at..at + n].copy_from_slice(&bytes[..n]);
    s.in_len += n as u32;
}

/// Drop `n` bytes from the front of the carry.
fn consume(s: &mut AacState, n: usize) {
    let len = s.in_len as usize;
    let n = n.min(len);
    s.in_buf.copy_within(n..len, 0);
    s.in_len = (len - n) as u32;
}

/// Find the next ADTS header in the carry, dropping what precedes it. The
/// header must parse and, when the following frame's start is in the carry,
/// be followed by another sync [spec r01 §2.1].
fn sync(s: &mut AacState) -> Option<Adts> {
    let mut at = 0usize;
    while at + 7 <= s.in_len as usize {
        let b = &s.in_buf[at..s.in_len as usize];
        if let Some(h) = parse_adts(b) {
            let next = usize::from(h.frame_length);
            let confirmed = b.len() < next + 2 || (b[next] == 0xFF && (b[next + 1] & 0xF6) == 0xF0);
            if confirmed {
                if at > 0 {
                    consume(s, at);
                }
                return Some(h);
            }
        }
        at += 1;
    }
    if at > 0 {
        consume(s, at);
    }
    None
}

/// Configure for a stream's fixed header, or refuse its layout.
unsafe fn configure(s: &mut AacState, h: &Adts) -> Result<(), Fault> {
    if s.dec.channels != 0
        && s.dec.rate_index == h.sampling_index
        && s.channel_config == h.channel_config
    {
        return Ok(());
    }
    s.dec.rate_index = h.sampling_index;
    s.channel_config = h.channel_config;
    s.dec.overlap = [[0.0; 1024]; MAX_CHANNELS];
    s.dec.prev_shape = [0; MAX_CHANNELS];
    s.refused_logged = 0;
    s.dec.channels = match h.channel_config {
        1 => 1,
        2 => 2,
        _ => 0,
    };
    if s.dec.channels == 0 {
        return Err(Fault::Unsupported);
    }
    let hz = tables::SAMPLE_RATES[usize::from(h.sampling_index)];
    let mut msg = *b"[aac] lc 00000 Hz 0 ch";
    for (i, d) in [10000, 1000, 100, 10, 1].iter().enumerate() {
        msg[9 + i] = b'0' + (hz / d % 10) as u8;
    }
    msg[18] = b'0' + s.dec.channels;
    dev_log(&*s.syscalls, 3, msg.as_ptr(), msg.len());
    Ok(())
}

/// Decode one raw data block at `br` into the output channels and produce
/// its PCM [spec r01 §3, §7, §8, §9].
fn decode_block(d: &mut Decoder, br: &mut BitReader<'_>) -> Result<(), Fault> {
    let rate = Rate::for_index(d.rate_index).ok_or(Fault::Malformed)?;
    let want_pair = d.channels == 2;
    let mut got_audio = false;
    let (out, scratch) = d.chans.split_at_mut(MAX_CHANNELS);
    let (left, right) = out.split_at_mut(1);
    let (left, right, scratch) = (&mut left[0], &mut right[0], &mut scratch[0]);
    loop {
        match br.read(3)? {
            ID_SCE | ID_LFE => {
                let _tag = br.read(4)?;
                if !want_pair && !got_audio {
                    syntax::parse_channel_stream(br, &rate, left, None)?;
                    got_audio = true;
                } else {
                    syntax::parse_channel_stream(br, &rate, scratch, None)?;
                }
            }
            ID_CPE => {
                let _tag = br.read(4)?;
                let common = br.bit()?;
                let use_out = want_pair && !got_audio;
                let shared = if common {
                    let info = syntax::parse_window_info(br, &rate)?;
                    if use_out {
                        syntax::parse_ms(br, &info, &mut d.ms)?;
                    } else {
                        let mut dropped = MsInfo::zero();
                        syntax::parse_ms(br, &info, &mut dropped)?;
                    }
                    Some(info)
                } else {
                    if use_out {
                        d.ms.mode = 0;
                    }
                    None
                };
                if use_out {
                    syntax::parse_channel_stream(br, &rate, left, shared.as_ref())?;
                    syntax::parse_channel_stream(br, &rate, right, shared.as_ref())?;
                    got_audio = true;
                } else {
                    // An extra pair is decoded to stay in sync and dropped:
                    // both halves land in the scratch channel in turn.
                    syntax::parse_channel_stream(br, &rate, scratch, shared.as_ref())?;
                    syntax::parse_channel_stream(br, &rate, scratch, shared.as_ref())?;
                }
            }
            ID_CCE => syntax::parse_cce(br, &rate, scratch)?,
            ID_DSE => syntax::skip_dse(br)?,
            ID_PCE => {
                syntax::parse_pce(br)?;
            }
            ID_FIL => syntax::skip_fil(br)?,
            ID_END => break,
            _ => return Err(Fault::Malformed),
        }
    }
    br.align()?;

    let channels = usize::from(d.channels);
    if !got_audio {
        // No audio element for this layout: silence [spec r01 §3.1].
        for c in out.iter_mut().take(channels) {
            c.spectrum = [0.0; 1024];
            c.tns_count = 0;
        }
    } else if want_pair {
        // Reconstruction, in the order of §7.
        tools::dequantise(left, &rate);
        tools::dequantise(right, &rate);
        tools::mid_side(left, right, &d.ms, &rate);
        tools::noise_pair(left, right, &d.ms, &rate, &mut d.noise);
        tools::intensity(left, right, &d.ms, &rate);
        tools::tns(left, &rate);
        tools::tns(right, &rate);
    } else {
        tools::dequantise(left, &rate);
        tools::noise_single(left, &rate, &mut d.noise);
        tools::tns(left, &rate);
    }
    for c in 0..channels {
        filterbank::frame(
            &out[c],
            d.prev_shape[c],
            &mut d.overlap[c],
            &mut d.time,
            &mut d.scratch,
        );
        d.prev_shape[c] = out[c].info.shape;
        for (i, &v) in d.time.iter().enumerate() {
            // Round half away from zero, saturate [spec r01 §9].
            let r = if v >= 0.0 { v + 0.5 } else { v - 0.5 };
            let sample = (r as i32).clamp(-32768, 32767) as i16;
            let at = (i * channels + c) * 2;
            d.pcm[at..at + 2].copy_from_slice(&sample.to_le_bytes());
        }
    }
    Ok(())
}

/// One step: drain owed output, read input, decode at most one raw data
/// block, write its PCM.
pub unsafe fn aac_step(s: &mut AacState) -> i32 {
    let sys = &*s.syscalls;
    if !drain_pending(
        sys,
        s.out_chan,
        s.dec.pcm.as_ptr(),
        &mut s.pending_out,
        &mut s.pending_offset,
    ) {
        return 0;
    }
    let in_len = s.in_len as usize;
    if in_len < IN_BUF {
        let poll = s.input.poll(sys, POLL_IN);
        if poll > 0 && (poll as u32) & POLL_IN != 0 {
            let want = (IN_BUF - in_len).min(READ_CHUNK);
            let n = s.input.read(sys, s.in_buf.as_mut_ptr().add(in_len), want);
            if n > 0 {
                s.in_len += n as u32;
            }
        }
    }

    if s.frame_open == 0 {
        let Some(h) = sync(s) else {
            return 0;
        };
        if (s.in_len as usize) < usize::from(h.frame_length) {
            return 0; // wait for the whole frame
        }
        s.header = h;
        s.frame_open = 1;
        s.blocks_done = 0;
        // Blocks start after the header, and after the block positions and
        // CRC word of a multi-block CRC frame [spec r01 §2.1].
        let mut at = usize::from(h.header_len);
        if h.crc && h.blocks > 1 {
            at += (usize::from(h.blocks) - 1) * 2 + 2;
        }
        s.block_at = at as u16;
    }

    let h = s.header;
    let frame_len = usize::from(h.frame_length);
    let result = configure(s, &h).and_then(|()| {
        let start = usize::from(s.block_at);
        if start > frame_len {
            return Err(Fault::Malformed);
        }
        let mut br = BitReader::new(&s.in_buf[start..frame_len]);
        decode_block(&mut s.dec, &mut br)?;
        // A block ends byte-aligned; in a multi-block CRC frame its own CRC
        // word follows [spec r01 §2.1].
        let mut next = start + br.pos() / 8;
        if h.crc && h.blocks > 1 {
            next += 2;
        }
        s.block_at = next as u16;
        Ok(())
    });
    match result {
        Ok(()) => {
            s.frames_decoded = s.frames_decoded.wrapping_add(1);
            s.blocks_done += 1;
            if s.blocks_done >= h.blocks {
                consume(s, frame_len);
                s.frame_open = 0;
            }
            let bytes = 1024 * usize::from(s.dec.channels) * 2;
            let written = (sys.channel_write)(s.out_chan, s.dec.pcm.as_ptr(), bytes);
            track_pending(written, bytes, &mut s.pending_out, &mut s.pending_offset);
            2
        }
        Err(Fault::Unsupported) => {
            if s.refused_logged == 0 {
                s.refused_logged = 1;
                let m = b"[aac] refused: channel layout is not 1 or 2 channels";
                dev_log(sys, 2, m.as_ptr(), m.len());
            }
            consume(s, frame_len);
            s.frame_open = 0;
            0
        }
        Err(_) => {
            // Discard the frame and resume scanning one byte after its sync
            // [spec r01 §2.1, §10]; the overlap is cleared so the next good
            // frame does not add a stale tail [spec r01 §9].
            s.frames_faulted = s.frames_faulted.wrapping_add(1);
            s.dec.overlap = [[0.0; 1024]; MAX_CHANNELS];
            consume(s, 1);
            s.frame_open = 0;
            0
        }
    }
}

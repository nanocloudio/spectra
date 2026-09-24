//! Unified Audio Decoder PIC Module
//!
//! Detects audio format from stream header and decodes accordingly.
//! Supports WAV (PCM), MP3 (MPEG-1 Layer III), and AAC (ADTS-framed).
//!
//! # Format Detection
//!
//! First bytes of input determine codec:
//! - `RIFF` (4 bytes) → WAV
//! - `0xFF 0xFB/FA/F3/F2` (MPEG sync) → MP3
//! - `ID3` (3 bytes) → MP3 (ID3 tag, then MPEG sync)
//! - `0xFF 0xF0/F1/F8/F9` (ADTS sync) → AAC
//!
//! # Stream Reset
//!
//! When bank switches files, it flushes the data channel. The decoder
//! detects stream end (codec returns done or sync loss) and resets to
//! format detection for the next file.
//!
//! # State Layout
//!
//! DecoderState wraps a codec union (byte array sized to largest codec).
//! Only one codec is active at a time.

#![cfg_attr(not(feature = "host-test"), no_std)]
#![allow(
    dead_code,
    unused_imports,
    unreachable_patterns,
    reason = "PIC build path-mounts modules/sdk/* via include!/mod, so each module's compile sees the full ABI surface; consumers use a subset. unreachable_patterns: defensive `_ => Error` arms in enum state-machine matches are intentional — adding a new variant should not silently bypass the error path"
)]
// ── Lints this module shape trips by construction ─────────────────────────
//
// `fluxor ci` clippies every module source at `-D warnings` (it does so
// directly now that the project has no root manifest), so what is allowed here
// is a claim about the whole module. Each family below is inherent to *a
// bare-metal codec module*, not to any one decoder — the decoder-specific
// exemptions live on the two DSP ports, in `audio/mod.rs`, so the clean-room
// code stays strict.
//
//   missing_safety_doc        The sub-codec kernels expose
//                             `pub unsafe fn <fmt>_init/_step/_feed_detect`.
//                             They are `pub` ONLY so `tests/harness` can drive
//                             a kernel directly; in the PIC build nothing
//                             outside this crate can call them, and the crate
//                             has no public API — the kernel reaches it through
//                             the `#[no_mangle]` ABI symbols. The contract is
//                             one sentence, identical for all of them, and
//                             stated at each family façade. Twenty-seven copies
//                             of it would be noise, not documentation.
//
//   not_unsafe_ptr_arg_deref  `module_new`/`module_step` take `*mut u8` state
//                             and a `*const c_void` syscall table because that
//                             IS the loader ABI. Marking them `unsafe fn` would
//                             not make the boundary any more checked — there is
//                             no Rust caller to check.
//
//   needless_range_loop       The format decoders walk several arrays in
//   too_many_arguments        lockstep by index (`raw[i]`, `prev[i]`,
//                             `out[i]`), and pass a scan's full parameter set
//                             positionally. Both read closer to the format
//                             specifications they implement than the iterator
//                             or struct rewrites would.
#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    reason = "inherent to a bare-metal codec module; see the comment above"
)]
use core::ffi::c_void;

// Host-test builds skip the SDK's PIC EABI intrinsic stubs (which
// are gated to `target_os = "none"` / `wasm32`); provide a host
// fallback for the one intrinsic the audio sub-codecs reach for.
#[cfg(feature = "host-test")]
pub unsafe fn __aeabi_memclr(dest: *mut u8, n: usize) {
    core::ptr::write_bytes(dest, 0, n);
}

// Guard against building with NO codec features — that produces a
// decoder that detects nothing and dispatches nothing. The only way it
// happens in practice is a pre-variant `fluxor` CLI building this
// variant-declaring source without emitting `--cfg feature="…"` flags;
// fail the build loudly instead of shipping a silently-empty codec.
#[cfg(not(any(
    feature = "wav",
    feature = "mp3",
    feature = "aac",
    feature = "image",
    feature = "h264"
)))]
compile_error!(
    "codec built with no codec features — stale `fluxor` CLI without [[variant]] support? \
     Rebuild the CLI, or pass --cfg feature=\"...\" flags"
);

#[path = "../../../target/fluxor/fluxor-abi/sdk/abi.rs"]
mod abi;
use abi::contracts::encoded as enc;
use abi::SyscallTable;

include!("../../../target/fluxor/fluxor-abi/sdk/runtime.rs");
include!("../../../target/fluxor/fluxor-abi/sdk/runtime/params.rs");

// ── Essence families ──────────────────────────────────────────────────────
//
// One directory per family, each with the SAME façade:
//
//   detect(&[u8]) -> Option<Format>   sniff, claiming only what it decodes
//   <fam>_init(..)                    bind state to channels
//   <fam>_feed_detect(..)             replay the bytes detection consumed
//   <fam>_step(..)                    one bounded unit of work per tick
//   <fam>_is_done(..)                 stream finished
//
// so this root dispatches to all of them the same way, and adding a family
// is a directory plus one arm in each of the four `match`es below rather
// than a new shape to learn. Within a family, a format is one file — or one
// directory when it needs more than one file, which today is `audio/aac/`
// and `video/h264/`.
//
// `container` is a peer rather than something inside `video`, because a
// container and a codec are orthogonal: see `container/mod.rs`.
//
// Feature gates (RFC module_variants): a family compiles only when one of
// its formats is enabled. The manifest's [[variant]] table drives which
// features each prebuilt fmod carries — `codec.fmod` (default = full) has
// everything; `codec-audio.fmod` has wav+mp3+aac only, dropping the image
// and video code, their state-union terms, and the large arena request.
//
// PIC builds keep the families private; host-test builds expose them so the
// harness can drive a sub-codec directly.
// Nearest-neighbour resampling, shared by the image and video families —
// both scale a source raster onto the `dst_w` x `dst_h` output surface, and
// before this existed they did it by two different rules.
#[cfg(any(feature = "image", feature = "h264"))]
pub mod scale;

// The decoders' byte source: the `encoded` channel, or the record pump's FIFO.
pub mod input;

#[cfg(any(feature = "wav", feature = "mp3", feature = "aac"))]
pub mod audio;

#[cfg(feature = "image")]
pub mod image;

#[cfg(feature = "h264")]
pub mod container;
#[cfg(feature = "h264")]
pub mod video;

// ============================================================================
// Constants
// ============================================================================

/// Format tags — the root's own flat namespace, grouped by family so a
/// family can be tested for with a range rather than an enumeration.
/// The value is surfaced verbatim in the `[dec] hb fmt=N` heartbeat, so
/// these are a diagnostic wire format: append, never renumber.
const FMT_DETECTING: u8 = 0;
// audio
const FMT_WAV: u8 = 1;
const FMT_MP3: u8 = 2;
const FMT_AAC: u8 = 3;
// image
const FMT_BMP: u8 = 4;
const FMT_GIF: u8 = 5;
const FMT_PNG: u8 = 6;
const FMT_JPEG: u8 = 7;
// video (container-carried)
const FMT_MKV: u8 = 8;
// video (record-fed: `video_in`)
const FMT_ES_H264: u8 = 9;

#[inline(always)]
fn is_audio_format(fmt: u8) -> bool {
    (FMT_WAV..=FMT_AAC).contains(&fmt)
}

#[inline(always)]
fn is_image_format(fmt: u8) -> bool {
    (FMT_BMP..=FMT_JPEG).contains(&fmt)
}

#[inline(always)]
fn is_video_format(fmt: u8) -> bool {
    fmt == FMT_MKV || fmt == FMT_ES_H264
}

/// Formats that own their own EOF / drain / reset cycle.
///
/// The image and video families both hold a decoded frame long after the
/// input has HUP'd, and drain it over hundreds of ticks; the root's
/// HUP-quiesce reset would wipe that mid-drain. Audio has no such
/// buffered artefact, so the root owns its reset. Expressed as "which
/// family owns the cycle" rather than a format list, so a new image or
/// video format inherits the right answer.
#[inline(always)]
fn owns_eof_cycle(fmt: u8) -> bool {
    is_image_format(fmt) || is_video_format(fmt)
}

/// Detection buffer size — enough to identify any format
const DETECT_BUF_SIZE: usize = 16;

/// IO buffer for detection phase reads
const DETECT_IO_SIZE: usize = 256;

/// Largest `UNIT` fragment payload `audio_in` / `video_in` accept — the
/// `max_payload` fact on both. A larger access unit arrives as several.
const REC_FRAGMENT_MAX: usize = 4096;
/// Carry for the record stream: one whole record at most (a `STREAM`'s
/// configuration is bounded the same way).
const REC_CARRY: usize = enc::UNIT_HEADER + REC_FRAGMENT_MAX;
/// Records handled per step.
const REC_BUDGET: u32 = 8;
/// ADTS header the pump writes ahead of each raw AAC unit.
const ADTS_HEADER: usize = 7;
/// Largest raw AAC access unit: 6144 bits per channel (ISO 14496-3
/// §4.5.3.1) for the 7.1 the ADTS channel configuration can name — the most
/// an ADTS frame can hold is 8191 bytes including its header.
const AAC_UNIT_MAX: usize = 8191 - ADTS_HEADER;

/// Number of consecutive scheduler ticks with `POLL_IN` clear and
/// `POLL_HUP` set that the decoder waits before resetting to format-
/// detection mode. Sized to cover the codec emitting at most one
/// full AAC frame's PCM out of its internal output buffer
/// (1024 stereo i16 = 4096 B, emitted in one channel_write per tick
/// in the current AAC IO_BUF_SIZE) plus a small safety margin.
const HUP_QUIESCE_TICKS: u8 = 64;

// Per-feature state-union terms. A disabled feature contributes 0, so
// the union — and therefore every instance's state footprint — shrinks
// to the largest codec actually compiled in (audio-only ≈ 32 KB vs the
// ~multi-MB video term).
#[cfg(feature = "wav")]
const WAV_STATE_SIZE: usize = core::mem::size_of::<audio::wav::WavState>();
#[cfg(not(feature = "wav"))]
const WAV_STATE_SIZE: usize = 0;

#[cfg(feature = "mp3")]
const MP3_STATE_SIZE: usize = core::mem::size_of::<audio::mp3::Mp3State>();
#[cfg(not(feature = "mp3"))]
const MP3_STATE_SIZE: usize = 0;

#[cfg(feature = "aac")]
const AAC_STATE_SIZE: usize = core::mem::size_of::<audio::aac::AacState>();
#[cfg(not(feature = "aac"))]
const AAC_STATE_SIZE: usize = 0;

#[cfg(feature = "image")]
const IMG_STATE_SIZE: usize = core::mem::size_of::<image::ImageState>();
#[cfg(not(feature = "image"))]
const IMG_STATE_SIZE: usize = 0;

#[cfg(feature = "h264")]
const MKV_STATE_SIZE: usize = core::mem::size_of::<video::MkvH264State>();
#[cfg(not(feature = "h264"))]
const MKV_STATE_SIZE: usize = 0;

/// Larger of two sizes. `core::cmp::max` is not `const`, and the alternative —
/// an `if TERM > max` chain over the five constants — reads as an absurd
/// comparison to clippy in every build where a family is disabled: that term is
/// then literally `0`, the minimum of `usize`, so the branch is provably dead.
/// It is dead *for that variant*, which is the entire point of the chain, so the
/// fix is to compare values the lint cannot constant-fold rather than to silence
/// it.
const fn max_size(a: usize, b: usize) -> usize {
    if a > b {
        a
    } else {
        b
    }
}

/// Codec state buffer size — must be >= the largest ENABLED codec state.
///
/// A disabled feature contributes 0, so the union shrinks to the largest codec
/// actually compiled in. That is what takes `codec-audio` from the video
/// variant's multi-MB state down to ~32 KB.
const CODEC_STATE_SIZE: usize = {
    let max = max_size(
        max_size(max_size(WAV_STATE_SIZE, MP3_STATE_SIZE), AAC_STATE_SIZE),
        max_size(IMG_STATE_SIZE, MKV_STATE_SIZE),
    );
    // Never 0, so `CodecBuf` stays a real field; then align up to 4 bytes.
    let max = max_size(max, 4);
    (max + 3) & !3
};

// ============================================================================
// State
// ============================================================================

/// Wrapper guaranteeing 8-byte alignment of the codec state buffer.
///
/// AacState/Mp3State/WavState all start with `*const SyscallTable`,
/// which on aarch64 / wasm32 / x86_64 needs 8-byte alignment for the
/// load. A bare `[u8; N]` field has alignment 1, so reinterpreting it
/// as one of those structs at an arbitrary `[u8]` offset SIGBUSes on
/// strict-alignment targets (aarch64). The `align(8)` newtype forces
/// the buffer to land on an 8-byte boundary inside `DecoderState`.
#[repr(C, align(8))]
struct CodecBuf([u8; CODEC_STATE_SIZE]);

#[repr(C)]
struct DecoderState {
    syscalls: *const SyscallTable,
    in_chan: i32,
    /// `out_chan` is the FIRST output port from `module_new` — the
    /// audio sink for audio formats. `pixels_chan` is looked up
    /// separately at module_new via `dev_channel_port(.., 1, 1)`
    /// because the BMP/image path emits VideoRaster on the second
    /// output port. Unwired ports return `-1`; the dispatch checks
    /// before writing.
    out_chan: i32,
    pixels_chan: i32,
    /// Active format: 0=detecting, 1=wav, 2=mp3, 3=aac, 4=bmp
    format: u8,
    /// Number of bytes in detect_buf
    detect_len: u8,
    /// Consecutive empty reads (for stream-end detection)
    empty_reads: u8,
    /// Consecutive ticks observed with `HUP set & POLL_IN clear` since
    /// the upstream signalled EOF. Reset is deferred until this counter
    /// reaches `HUP_QUIESCE_TICKS` so the codec gets a chance to finish
    /// emitting any PCM that was already decoded but hadn't been
    /// channel_write'd to its consumer yet. Without this delay, the
    /// browser harness lost ~250 ms of audio (last 2 notes of the
    /// 8-note scale) because `host_browser_fetch` HUPs as soon as the
    /// response body is fully drained — even though the channel ring
    /// still had ~10 AAC frames buffered for the codec to decode.
    hup_quiet_ticks: u8,
    _pad_tc: [u8; 1],
    /// Tick counter for the per-codec heartbeat (every 5000 ticks).
    /// Drives the `[img] decoded (heartbeat)` re-emit + the sticky
    /// `last_err` replay, both of which exist because a one-shot
    /// dev_log from inside `decode_buffer` can be lost in the
    /// early-boot log ring before log_net's UDP stream is fully
    /// draining.
    tick_count: u32,
    /// Header accumulation buffer for format detection
    detect_buf: [u8; DETECT_BUF_SIZE],
    /// IO buffer used during detection phase
    detect_io: [u8; DETECT_IO_SIZE],
    /// Codec state — overlay for WavState/Mp3State/AacState/ImageState
    codec: CodecBuf,

    // ── Image-format staging ──
    //
    // BMP format isn't detected until the first two bytes arrive, by
    // which time the ImageState lives inside the `codec` union. We
    // stage the YAML-driven params here at module_new time and copy
    // them into `ImageState` from `init_codec` once the format is
    // known. Default-zero fields use `image`'s own defaults
    // (480×480 stretch, 8 MiB max).
    image_dst_w: u16,
    image_dst_h: u16,
    image_scale_mode: u8,
    _image_pad: u8,
    image_max_bytes: u32,

    // ── Record input (`audio_in` / `video_in`) ──
    //
    // The encoded-media record stream is a second way in. Its STREAM record
    // names the codec, so nothing is sniffed: the pump opens the matching
    // decoder and feeds it — audio through `fifo`, which the decoder reads as
    // its `Input`, video straight into the H.264 accumulator.
    /// The wired record input, or -1 when the module reads `encoded`.
    rec_chan: i32,
    /// `rec_chan` is `video_in`.
    rec_video: u8,
    /// The open stream was accepted and a decoder is bound to it.
    rec_ok: u8,
    /// Inside a fragmented unit.
    rec_in_unit: u8,
    /// Raw AAC: each unit gets an ADTS header built from the stream's
    /// AudioSpecificConfig — `adts` is that header's constant part.
    rec_raw_aac: u8,
    rec_carry_len: u16,
    aac_unit_len: u16,
    /// Video: milliseconds per `pts` tick, Q16 — the PTS pacing clock. A
    /// division per unit would be a 64-bit one the 32-bit PIC targets
    /// cannot link, so the ratio is taken once, in 32 bits, per stream.
    rec_ms_q16: u32,
    /// Streams refused (no decoder for the codec here) and record-stream
    /// faults. Counted, never silent.
    rec_refused: u32,
    rec_faults: u32,
    rec_sequence: enc::Sequence,
    adts: [u8; ADTS_HEADER],
    _rec_pad: u8,
    rec_carry: [u8; REC_CARRY],
    aac_unit: [u8; AAC_UNIT_MAX],
    fifo: input::ByteFifo,
}

impl DecoderState {
    #[inline(always)]
    unsafe fn sys(&self) -> &SyscallTable {
        &*self.syscalls
    }

    #[cfg(feature = "wav")]
    #[inline(always)]
    unsafe fn wav(&mut self) -> &mut audio::wav::WavState {
        &mut *(self.codec.0.as_mut_ptr() as *mut audio::wav::WavState)
    }

    #[cfg(feature = "mp3")]
    #[inline(always)]
    unsafe fn mp3(&mut self) -> &mut audio::mp3::Mp3State {
        &mut *(self.codec.0.as_mut_ptr() as *mut audio::mp3::Mp3State)
    }

    #[cfg(feature = "aac")]
    #[inline(always)]
    unsafe fn aac(&mut self) -> &mut audio::aac::AacState {
        &mut *(self.codec.0.as_mut_ptr() as *mut audio::aac::AacState)
    }

    #[cfg(feature = "image")]
    #[inline(always)]
    unsafe fn img(&mut self) -> &mut image::ImageState {
        &mut *(self.codec.0.as_mut_ptr() as *mut image::ImageState)
    }

    #[cfg(feature = "h264")]
    #[inline(always)]
    unsafe fn mkv(&mut self) -> &mut video::MkvH264State {
        &mut *(self.codec.0.as_mut_ptr() as *mut video::MkvH264State)
    }
}

// ============================================================================
// Parameter Definitions
// ============================================================================

mod params_def {
    use super::DecoderState;
    use super::SCHEMA_MAX;
    use super::{p_u16, p_u32, p_u8};

    // Image params (1-4). Audio formats have no per-format params
    // today (everything is auto-detected from the bitstream); when
    // they do, append at tag 10+.
    define_params! {
        DecoderState;

        1, width, u16, 480
            => |s, d, len| { s.image_dst_w = p_u16(d, len, 0, 480); };

        2, height, u16, 480
            => |s, d, len| { s.image_dst_h = p_u16(d, len, 0, 480); };

        // Only `stretch` is implemented. `fit` and `fill` are accepted and
        // then IGNORED — the raster is stretched regardless — so a graph that
        // asks for them silently gets something else. `init_codec` logs a line
        // naming the substitution rather than leaving it invisible; the enum
        // keeps all three because the values are a declared public surface
        // (`codec_baseline`) and dropping them would be the breaking change,
        // not the honest one.
        3, scale_mode, u8, 0, enum { stretch=0, fit=1, fill=2 }
            => |s, d, len| { s.image_scale_mode = p_u8(d, len, 0, 0); };

        4, max_bytes, u32, 8388608
            => |s, d, len| { s.image_max_bytes = p_u32(d, len, 0, 8388608); };
    }
}

// ============================================================================
// Format Detection
// ============================================================================

/// Identify the stream's format from the bytes accumulated so far.
///
/// Returns an `FMT_*` tag, or `FMT_DETECTING` when no family has claimed
/// the bytes yet and more may help.
///
/// Each family owns its own sniffing (`audio::detect`, `image::detect`,
/// `container::detect`) and claims only formats it can actually decode, so
/// this function is the ORDER policy and nothing else. That order is not
/// arbitrary:
///
/// 1. **Containers first.** A container's payload is an essence stream, and
///    its own header could sit close enough to an essence magic to be
///    mis-sniffed. Claiming the wrapper before looking inside removes the
///    question.
/// 2. **Fixed-magic essences before sync-word ones.** Image magics are
///    long, exact literals; the MPEG/ADTS sync word is 11 set bits, which
///    an arbitrary payload hits far more often. Cheap certainty first.
fn detect_format(buf: &[u8; DETECT_BUF_SIZE], len: u8) -> u8 {
    let n = len as usize;
    if n < 2 {
        return FMT_DETECTING;
    }
    let head = &buf[..n];

    #[cfg(feature = "h264")]
    if let Some(c) = container::detect(head) {
        return match c {
            container::Container::Matroska => FMT_MKV,
        };
    }

    #[cfg(feature = "image")]
    if let Some(f) = image::detect(head) {
        return match f {
            image::ImageFormat::Bmp => FMT_BMP,
            image::ImageFormat::Gif => FMT_GIF,
            image::ImageFormat::Png => FMT_PNG,
            image::ImageFormat::Jpeg => FMT_JPEG,
        };
    }

    #[cfg(any(feature = "wav", feature = "mp3", feature = "aac"))]
    if let Some(f) = audio::detect(head) {
        return match f {
            audio::AudioFormat::Wav => FMT_WAV,
            audio::AudioFormat::Mp3 => FMT_MP3,
            audio::AudioFormat::Aac => FMT_AAC,
        };
    }

    // Nothing claimed 8 bytes. Treat the stream as headerless PCM and let
    // the WAV path pass it through: in a real graph the bank only serves
    // known formats, so this is the "raw samples, no RIFF wrapper" case
    // rather than a genuine unknown. It stays a ROOT policy — no family
    // should claim bytes it did not recognise.
    if n >= 8 {
        return FMT_WAV;
    }

    FMT_DETECTING
}

/// Say so when a `scale_mode` the decoders do not implement was asked for.
///
/// Both raster families stretch unconditionally. Accepting `fit`/`fill` and
/// quietly stretching is the kind of defect that reads as a decoder bug from
/// the outside — the picture is simply the wrong shape — so it is named once,
/// at init, on the same log path as the format commit.
unsafe fn warn_unimplemented_scale_mode(sys: &SyscallTable, mode: u8) {
    if mode != 0 {
        let msg = b"[dec] scale_mode fit/fill unimplemented - using stretch";
        dev_log(sys, 2, msg.as_ptr(), msg.len());
    }
}

/// Reset decoder to format detection state.
unsafe fn reset_to_detect(s: &mut DecoderState) {
    s.format = FMT_DETECTING;
    s.detect_len = 0;
    s.empty_reads = 0;
    s.hup_quiet_ticks = 0;
    // Zero the codec state
    __aeabi_memclr(s.codec.0.as_mut_ptr(), CODEC_STATE_SIZE);
}

/// Initialize the detected codec and feed it the detection bytes.
unsafe fn init_codec(s: &mut DecoderState) {
    let syscalls = s.syscalls;
    let in_chan = s.in_chan;
    let out_chan = s.out_chan;
    let detect_len = s.detect_len as usize;
    // Take raw pointer to detect_buf before borrowing codec via &mut
    let detect_ptr = s.detect_buf.as_ptr();
    let codec_ptr = s.codec.0.as_mut_ptr();

    match s.format {
        #[cfg(feature = "wav")]
        FMT_WAV => {
            let ws = &mut *(codec_ptr as *mut audio::wav::WavState);
            audio::wav::wav_init(ws, syscalls, input::Input::channel(in_chan), out_chan);
            audio::wav::wav_feed_detect(ws, detect_ptr, detect_len);
            dev_log(&*syscalls, 3, b"[dec] wav".as_ptr(), 9);
        }
        #[cfg(feature = "mp3")]
        FMT_MP3 => {
            let ms = &mut *(codec_ptr as *mut audio::mp3::Mp3State);
            audio::mp3::mp3_init(ms, syscalls, input::Input::channel(in_chan), out_chan);
            audio::mp3::mp3_feed_detect(ms, detect_ptr, detect_len);
            dev_log(&*syscalls, 3, b"[dec] mp3".as_ptr(), 9);
        }
        #[cfg(feature = "aac")]
        FMT_AAC => {
            let a = &mut *(codec_ptr as *mut audio::aac::AacState);
            audio::aac::aac_init(a, syscalls, input::Input::channel(in_chan), out_chan);
            audio::aac::aac_feed_detect(a, detect_ptr, detect_len);
            dev_log(&*syscalls, 3, b"[dec] aac".as_ptr(), 9);
        }
        #[cfg(feature = "image")]
        FMT_BMP | FMT_GIF | FMT_PNG | FMT_JPEG => {
            // Image path emits on `pixels` (output port 1), not the
            // audio `out_chan` (output port 0). The parent looks
            // `pixels_chan` up via `dev_channel_port(.., 1, 1)` in
            // module_new and stashes it on `DecoderState`.
            let pix_chan = s.pixels_chan;
            let (dw, dh, sm, mb) = (
                s.image_dst_w,
                s.image_dst_h,
                s.image_scale_mode,
                s.image_max_bytes,
            );
            warn_unimplemented_scale_mode(&*syscalls, sm);
            let img = &mut *(codec_ptr as *mut image::ImageState);
            // Stage params + format discriminant before image_init.
            img.dst_w = if dw == 0 { 480 } else { dw };
            img.dst_h = if dh == 0 { 480 } else { dh };
            img.scale_mode = sm;
            img.max_bytes = if mb == 0 { 8 * 1024 * 1024 } else { mb };
            img.image_format = match s.format {
                FMT_GIF => image::ImageFormat::Gif,
                FMT_PNG => image::ImageFormat::Png,
                FMT_JPEG => image::ImageFormat::Jpeg,
                _ => image::ImageFormat::Bmp,
            };
            image::image_init(img, syscalls, in_chan, pix_chan);
            image::image_feed_detect(img, detect_ptr, detect_len);
            let tag: &[u8] = match s.format {
                FMT_GIF => b"[dec] gif",
                FMT_PNG => b"[dec] png",
                FMT_JPEG => b"[dec] jpeg",
                _ => b"[dec] bmp",
            };
            dev_log(&*syscalls, 3, tag.as_ptr(), tag.len());
        }
        #[cfg(feature = "h264")]
        FMT_MKV => {
            let pix_chan = s.pixels_chan;
            let mkv = stage_video(s);
            video::mkv_init(mkv, syscalls, in_chan, pix_chan);
            video::mkv_feed_detect(mkv, detect_ptr, detect_len);
        }
        _ => {}
    }
}

/// Stage the video decoder's parameters in the codec union. Video emits on
/// `pixels` (output port 1), like the image path, and shares its params.
#[cfg(feature = "h264")]
unsafe fn stage_video(s: &mut DecoderState) -> &mut video::MkvH264State {
    let (dw, dh, sm, mb) = (
        s.image_dst_w,
        s.image_dst_h,
        s.image_scale_mode,
        s.image_max_bytes,
    );
    warn_unimplemented_scale_mode(s.sys(), sm);
    let mkv = s.mkv();
    mkv.dst_w = dw;
    mkv.dst_h = dh;
    mkv.scale_mode = sm;
    // `max_bytes` is staged from the shared image params whose DEFAULT
    // (8 MiB) is sized for whole-image accumulation. The video ES buffer holds
    // at most a burst of coded frames (~200 KB each at SD), so the image
    // default reads as unset and becomes 2 MiB; explicit values pass through.
    mkv.max_bytes = if mb == 0 || mb == 8_388_608 {
        2 * 1024 * 1024
    } else {
        mb
    };
    mkv
}

// ============================================================================
// Record input
// ============================================================================

/// The constant part of an ADTS header for a raw AAC stream, from its
/// AudioSpecificConfig: object type 1–4 (ADTS carries the profile as two
/// bits), an indexed sample rate, and a channel configuration 1–7. `None`
/// for anything ADTS cannot express.
fn adts_template(asc: &[u8]) -> Option<[u8; ADTS_HEADER]> {
    let (&b0, &b1) = (asc.first()?, asc.get(1)?);
    let object_type = b0 >> 3;
    let sfi = ((b0 & 0x07) << 1) | (b1 >> 7);
    let channels = (b1 >> 3) & 0x0F;
    if !(1..=4).contains(&object_type) || sfi > 12 || !(1..=7).contains(&channels) {
        return None;
    }
    // MPEG-4, layer 0, no CRC; frame length and fullness filled per unit.
    Some([
        0xFF,
        0xF1,
        ((object_type - 1) << 6) | (sfi << 2) | (channels >> 2),
        (channels & 0x03) << 6,
        0,
        0x1F,
        0xFC,
    ])
}

/// Forget the record stream after a fault — the boundary is lost, or the
/// producer broke the record order — and wait for a new one.
unsafe fn record_fault(s: &mut DecoderState) {
    s.rec_faults = s.rec_faults.wrapping_add(1);
    s.rec_carry_len = 0;
    s.rec_sequence = enc::Sequence::new();
    close_record_stream(s);
    let m = b"[dec] record stream fault";
    dev_log(s.sys(), 2, m.as_ptr(), m.len());
}

/// Release the decoder bound to the current record stream.
unsafe fn close_record_stream(s: &mut DecoderState) {
    s.rec_ok = 0;
    s.rec_in_unit = 0;
    s.aac_unit_len = 0;
    s.fifo.clear();
    reset_to_detect(s);
}

/// Bind a decoder to a new record stream, or refuse it.
unsafe fn open_record_stream(s: &mut DecoderState, st: &enc::Stream<'_>) {
    close_record_stream(s);
    let syscalls = s.syscalls;
    let out_chan = s.out_chan;
    let fifo = input::Input::fifo(&mut s.fifo);
    let audio = s.rec_video == 0;
    s.rec_ok = 1;
    match (st.codec, st.packing) {
        #[cfg(feature = "aac")]
        (enc::CODEC_AAC, enc::PACKING_RAW | enc::PACKING_FRAMED) if audio => {
            s.rec_raw_aac = u8::from(st.packing == enc::PACKING_RAW);
            if s.rec_raw_aac != 0 {
                match adts_template(st.config) {
                    Some(t) => s.adts = t,
                    None => s.rec_ok = 0,
                }
            }
            if s.rec_ok != 0 {
                s.format = FMT_AAC;
                audio::aac::aac_init(s.aac(), syscalls, fifo, out_chan);
                dev_log(&*syscalls, 3, b"[dec] es/aac".as_ptr(), 12);
            }
        }
        #[cfg(feature = "mp3")]
        (enc::CODEC_MP3, enc::PACKING_FRAMED) if audio => {
            s.format = FMT_MP3;
            audio::mp3::mp3_init(s.mp3(), syscalls, fifo, out_chan);
            dev_log(&*syscalls, 3, b"[dec] es/mp3".as_ptr(), 12);
        }
        #[cfg(feature = "h264")]
        (enc::CODEC_H264, enc::PACKING_ANNEXB | enc::PACKING_LENGTH_PREFIXED) if !audio => {
            s.format = FMT_ES_H264;
            // `stream_is_valid` refused a zero clock; `NonZeroU32` says so to
            // the compiler, which then emits no divide-by-zero panic path.
            s.rec_ms_q16 = core::num::NonZeroU32::new(st.clock_rate)
                .map_or(0, |clock| (1000u32 << 16) / clock);
            let pix_chan = s.pixels_chan;
            let mkv = stage_video(s);
            video::es_init(mkv, syscalls, pix_chan);
            if !video::es_stream(mkv, st.packing == enc::PACKING_ANNEXB, st.config) {
                s.rec_ok = 0;
            }
        }
        _ => s.rec_ok = 0,
    }
    if s.rec_ok == 0 {
        reset_to_detect(s);
        s.rec_refused = s.rec_refused.wrapping_add(1);
        let m = b"[dec] record stream refused: no decoder for its codec here";
        dev_log(&*syscalls, 2, m.as_ptr(), m.len());
    }
}

/// Hand one `UNIT` fragment to the bound video decoder. `false` is
/// backpressure: nothing was taken, and the record is offered again next step.
#[cfg(feature = "h264")]
unsafe fn feed_video_unit(s: &mut DecoderState, flags: u8, pts: i64, payload: &[u8]) -> bool {
    if !video::es_room(s.mkv(), payload.len()) {
        return false;
    }
    let first = s.rec_in_unit == 0;
    let last = flags & enc::FLAG_CONTINUES == 0;
    let truncated = flags & enc::FLAG_TRUNCATED != 0;
    let pts_ms = ((pts.max(0) as u64) * u64::from(s.rec_ms_q16)) >> 16;
    video::es_unit(s.mkv(), first, last, truncated, pts_ms, payload);
    s.rec_in_unit = u8::from(!last);
    true
}

/// Hand one `UNIT` fragment to the bound audio decoder, through the FIFO.
/// `false` is backpressure, as for video.
unsafe fn feed_audio_unit(s: &mut DecoderState, flags: u8, payload: &[u8]) -> bool {
    let last = flags & enc::FLAG_CONTINUES == 0;
    let truncated = flags & enc::FLAG_TRUNCATED != 0;
    if s.rec_raw_aac != 0 {
        let held = s.aac_unit_len as usize;
        if last && !truncated && s.fifo.free() < ADTS_HEADER + held + payload.len() {
            return false;
        }
        if held + payload.len() > AAC_UNIT_MAX {
            // Not an AAC access unit; drop what was gathered of it.
            s.aac_unit_len = 0;
            s.rec_in_unit = u8::from(!last);
            return true;
        }
        s.aac_unit[held..held + payload.len()].copy_from_slice(payload);
        s.aac_unit_len += payload.len() as u16;
        if last {
            let len = s.aac_unit_len as usize;
            s.aac_unit_len = 0;
            if !truncated {
                let frame = (ADTS_HEADER + len) as u32;
                let mut header = s.adts;
                header[3] |= (frame >> 11) as u8;
                header[4] = (frame >> 3) as u8;
                header[5] |= ((frame & 0x07) << 5) as u8;
                s.fifo.push(&header);
                s.fifo.push(&s.aac_unit[..len]);
            }
        }
    } else if !truncated && !s.fifo.push(payload) {
        // Self-delimiting frames (ADTS, MPEG audio) go through as they arrive;
        // a truncated tail is skipped and the decoder resyncs on the next frame.
        return false;
    }
    s.rec_in_unit = u8::from(!last);
    true
}

/// Read the record stream and feed it to the bound decoder.
unsafe fn pump_records(s: &mut DecoderState) {
    let len = s.rec_carry_len as usize;
    if len < REC_CARRY {
        let n = (s.sys().channel_read)(
            s.rec_chan,
            s.rec_carry.as_mut_ptr().add(len),
            REC_CARRY - len,
        );
        if n > 0 {
            s.rec_carry_len += n as u16;
        }
    }
    let mut budget = REC_BUDGET;
    while budget > 0 {
        budget -= 1;
        let carried = s.rec_carry_len as usize;
        // Read through a raw view: the handlers below mutate other fields of
        // `s`, and the carry is not touched until the record is consumed.
        let carry = core::slice::from_raw_parts(s.rec_carry.as_ptr(), carried);
        let (record, consumed) = match enc::parse(carry, REC_CARRY) {
            enc::Parse::NeedMore => return,
            enc::Parse::Fault(_) => return record_fault(s),
            enc::Parse::Record { record, consumed } => (record, consumed),
        };
        let taken = match record {
            enc::Record::Unit(u) if s.rec_ok != 0 => {
                // Admit before feeding, so an out-of-order fragment is never
                // half-applied; a refused one stays for the next step.
                let mut probe = s.rec_sequence;
                if probe.admit(&record).is_err() {
                    return record_fault(s);
                }
                let fed = match s.format {
                    #[cfg(feature = "h264")]
                    FMT_ES_H264 => feed_video_unit(s, u.flags, u.pts, u.payload),
                    _ => feed_audio_unit(s, u.flags, u.payload),
                };
                if !fed {
                    return;
                }
                s.rec_sequence = probe;
                true
            }
            _ => {
                if s.rec_sequence.admit(&record).is_err() {
                    return record_fault(s);
                }
                match record {
                    enc::Record::Stream(st) => open_record_stream(s, &st),
                    enc::Record::End => match s.format {
                        #[cfg(feature = "h264")]
                        FMT_ES_H264 => video::es_end(s.mkv()),
                        _ => s.fifo.end(),
                    },
                    enc::Record::Unit(_) => {}
                }
                true
            }
        };
        if taken {
            s.rec_carry.copy_within(consumed..carried, 0);
            s.rec_carry_len = (carried - consumed) as u16;
        }
    }
}

/// One step of a record-fed module: pump records, then run the bound decoder
/// and release it once its stream has ended and drained.
unsafe fn step_records(s: &mut DecoderState) -> i32 {
    pump_records(s);
    match s.format {
        #[cfg(feature = "h264")]
        FMT_ES_H264 => {
            let r = video::mkv_step(s.mkv());
            if video::mkv_is_done(s.mkv()) {
                dev_log(s.sys(), 3, b"[dec] es done".as_ptr(), 13);
                close_record_stream(s);
            }
            r
        }
        #[cfg(feature = "mp3")]
        FMT_MP3 => {
            let r = audio::mp3::mp3_step(s.mp3());
            finish_audio_record_stream(s);
            r
        }
        #[cfg(feature = "aac")]
        FMT_AAC => {
            let r = audio::aac::aac_step(s.aac());
            finish_audio_record_stream(s);
            r
        }
        _ => 0,
    }
}

/// An audio stream that ended and drained is given the same quiesce window
/// as a hung-up channel, so the decoder emits the PCM it still holds.
unsafe fn finish_audio_record_stream(s: &mut DecoderState) {
    if !s.fifo.ended_and_drained() {
        s.hup_quiet_ticks = 0;
        return;
    }
    s.hup_quiet_ticks = s.hup_quiet_ticks.saturating_add(1);
    if s.hup_quiet_ticks >= HUP_QUIESCE_TICKS {
        dev_log(s.sys(), 3, b"[dec] es done".as_ptr(), 13);
        close_record_stream(s);
    }
}

// ============================================================================
// Module API
// ============================================================================

#[cfg_attr(not(feature = "host-test"), no_mangle)]
#[link_section = ".text.module_state_size"]
pub extern "C" fn module_state_size() -> u32 {
    core::mem::size_of::<DecoderState>() as u32
}

/// Per-module heap budget, derived from the ENABLED feature set (RFC
/// module_variants — this is the primary constrained-target win).
///
/// These values are MEASURED, not estimated. `spectra-bench --probe-arena`
/// binary-searches the smallest arena at which each corpus tier decodes; the
/// numbers below are that search, rounded up for headroom. The previous
/// estimate ("≈ 12.5 MiB at 1080p → 16 MiB") under-called the real
/// requirement by 2–3.5x, which is why no still or video above DVD raster
/// could be decoded — see `docs/testing/perf-benchmarks.md` §7.
///
/// Measured minimum arena, MiB (2026-07-26):
///
/// | Raster | BMP | GIF | JPEG | PNG | H.264 |
/// | --- | --- | --- | --- | --- | --- |
/// | DVD 720x480/576 | 2 | 4 | 3 | 5 | 6–11 |
/// | 1080p | 10 | 20 | 17 | 26 | 24–44 |
/// | 4K | 40 | — | 65 | 97 | 82–116 |
///
/// Bitrate matters as much as raster on the video path: `bluray_base`
/// (crf 23) needs 27 MiB and `bluray_hibitrate` (crf 18) needs 44, because
/// the elementary-stream accumulator scales with coded frame size rather than
/// pixel count.
///
/// - video (`h264`): **48 MiB** — covers every measured 1080p tier including
///   the crf 18 worst case, with ~9% headroom.
/// - image (no video): **32 MiB** — covers every measured 1080p still (PNG at
///   26 MiB is the worst; inflate window plus full RGB intermediate plus
///   RGB565 output).
/// - audio only: 64 KiB. The audio sub-codecs run entirely out of the state
///   union and allocate nothing — measured 0 bytes high-water across all five
///   audio tiers — so this is pure margin for incidental allocation.
///
/// # Why 47 and not a round number — this is a measured ceiling
///
/// 64 MiB was asked for and tried. It does not load, and neither does 48:
///
/// ```text
/// STATE ARENA EXHAUSTED — need=67108872 used=67191136 cap=100663296
/// ```
///
/// Two facts combine, and the second is the surprising one.
///
/// 1. `STATE_ARENA_SIZE` is 96 MiB on aarch64 and wasm
///    (`deps/fluxor/modules/sdk/abi/config.rs`), shared by EVERY module in the
///    graph and carved eagerly by `loader::alloc_state`.
/// 2. **The arena is charged twice per load.** Measured, not inferred: at
///    24 MiB a three-module graph reports `state=50413928` — two 24 MiB
///    arenas plus one 82 KB module state. At 47 it reports `98648424`, which
///    is 2 x 47 MiB + state and 98% of the pool. At 48 and 64 the `used`
///    figure is always exactly one arena plus one state at the moment the
///    second is requested.
///
/// So the usable ceiling is `(96 MiB - state) / 2`, i.e. **47 MiB**, and this
/// constant sits exactly on it. 47 is not a safety margin — it is the largest
/// value that loads. It also happens to cover every measured 1080p tier
/// (worst case 44 MiB, `bluray_hibitrate`), so nothing is lost by stopping
/// here.
///
/// Verified at 47 on all three runtimes:
///   * linux — `examples/video_player/linux.yaml` decodes 100 PPM frames;
///   * wasm — `tests/host/video_player_wasm.pw.py` passes all five checks in
///     a real Firefox (canvas non-blank, frames advancing);
///   * bcm2712 — the `audio` variant loads and heartbeats on the pi5 board
///     (its 64 KiB arena is unaffected either way).
///
/// **Going higher needs a platform change, not a bigger number here.** 64 MiB
/// wants `STATE_ARENA_SIZE` at ~132 MiB minimum (realistically 160); 4K wants
/// ~232 MiB once doubled. That constant lives in Fluxor and is shared by every
/// project on the platform, and raising it means rebuilding the hash-pinned
/// kernel and wasm firmware that wave/lattice/quantum also resolve — so it is
/// a platform decision, deliberately not made from this repo.
///
/// **The doubling looks like a Fluxor loader defect.** It halves every
/// module's usable arena on every project and nothing documents it. Worth
/// raising independently of the codec; if it is fixed, this constant can
/// roughly double with no other change.
///
/// At 47 MiB the codec consumes 98% of the pool on its own, so a graph that
/// loads any OTHER arena-requesting module will not fit. That is the real
/// argument for fixing the doubling rather than living with it.
///
/// On rp2350 the whole pool is 256 KiB, so only the audio variant can ever
/// load there; true before any of this and unchanged by it.
#[cfg_attr(not(feature = "host-test"), no_mangle)]
#[link_section = ".text.module_arena_size"]
pub extern "C" fn module_arena_size() -> u32 {
    if cfg!(feature = "h264") {
        47 * 1024 * 1024
    } else if cfg!(feature = "image") {
        32 * 1024 * 1024
    } else {
        64 * 1024
    }
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
        if syscalls.is_null() || state.is_null() {
            return -1;
        }
        if state_size < core::mem::size_of::<DecoderState>() {
            return -2;
        }

        // Zero-init entire state
        __aeabi_memclr(state, core::mem::size_of::<DecoderState>());

        let s = &mut *(state as *mut DecoderState);
        s.syscalls = syscalls as *const SyscallTable;
        s.in_chan = in_chan;
        s.out_chan = out_chan;
        // pixels port: output index 1 in the manifest (audio is
        // output index 0 = `out_chan` above). Unwired = -1; the
        // image path checks before writing.
        s.pixels_chan = dev_channel_port(&*s.syscalls, 1, 1);
        s.format = FMT_DETECTING;
        s.detect_len = 0;
        s.empty_reads = 0;

        // Exactly one way in: `encoded` bytes, or a record stream on
        // `audio_in` (in[1]) or `video_in` (in[2]).
        let audio_in = dev_channel_port(&*s.syscalls, 0, 1);
        let video_in = dev_channel_port(&*s.syscalls, 0, 2);
        let wired = [in_chan, audio_in, video_in]
            .iter()
            .filter(|&&c| c >= 0)
            .count();
        if wired > 1 {
            let m = b"[dec] refusing to construct: wire one of encoded, audio_in, video_in";
            dev_log(&*s.syscalls, 1, m.as_ptr(), m.len());
            return -22;
        }
        s.rec_chan = if audio_in >= 0 { audio_in } else { video_in };
        s.rec_video = u8::from(video_in >= 0);
        s.rec_sequence = enc::Sequence::new();

        // Parse params
        let is_tlv =
            !params.is_null() && params_len >= 4 && *params == 0xFE && *params.add(1) == 0x01;

        if is_tlv {
            params_def::parse_tlv(s, params, params_len);
        } else {
            params_def::set_defaults(s);
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
        let s = &mut *(state as *mut DecoderState);
        if s.syscalls.is_null() {
            return -1;
        }

        // Once-per-heartbeat re-emit of `[img] decoded` while the
        // image path holds a successfully-decoded frame
        // (Phase::Decoded or Phase::Draining — NOT Phase::Error
        // which would be a false positive). The one-shot log fired
        // inside `decode_buffer` can be lost in the early-boot log
        // ring if it lands before log_net's UDP stream is fully
        // draining — same pattern fat32/nvme use for their `init=`
        // / `st=` heartbeats so a viewer connecting mid-run still
        // sees the proof line.
        s.tick_count = s.tick_count.wrapping_add(1);
        // Liveness heartbeat, unconditional on format: the image/video
        // heartbeats below only speak once a format is committed, so a
        // codec stuck in DETECTING — or one whose one-shot "[dec] xxx"
        // line was lost in the pre-net boot window — is indistinguishable
        // from a dead module in UDP telemetry. One log line per ~5000
        // ticks names the detected format byte.
        if s.tick_count.is_multiple_of(5000) {
            let mut msg = *b"[dec] hb fmt=0";
            msg[13] = b'0' + (s.format % 10);
            dev_log(s.sys(), 3, msg.as_ptr(), msg.len());
        }
        // Video-path heartbeat: frames out + ES fill + phase, so a
        // wedged hop is identifiable from one log line without
        // instrumenting the peer modules.
        #[cfg(feature = "h264")]
        if s.tick_count.is_multiple_of(5000) && is_video_format(s.format) {
            let mkv = &*(s.codec.0.as_ptr() as *const video::MkvH264State);
            video::mkv_heartbeat(mkv, s.sys());
        }
        #[cfg(feature = "image")]
        if s.tick_count.is_multiple_of(5000) && is_image_format(s.format) {
            let img = &*(s.codec.0.as_ptr() as *const image::ImageState);
            let ph = img.phase as u32;
            if ph == image::Phase::Decoded as u32 || ph == image::Phase::Draining as u32 {
                let msg = b"[img] decoded (heartbeat)";
                dev_log(s.sys(), 3, msg.as_ptr(), msg.len());
            }
            // Sticky error replay: format decoders write the reason
            // into `last_err` on every failure path. Surface it once
            // per heartbeat so the rig telemetry sees the diagnosis
            // even when the one-shot dev_log from inside
            // `decode_buffer` was dropped by log_net at boot.
            if img.last_err_len > 0 {
                dev_log(s.sys(), 3, img.last_err.as_ptr(), img.last_err_len as usize);
            }
        }

        if s.rec_chan >= 0 {
            return step_records(s);
        }

        // ----------------------------------------------------------------
        // Format detection phase
        // ----------------------------------------------------------------
        if s.format == FMT_DETECTING {
            let sys = s.sys();
            // Ack a pending stream-boundary HUP iff the ring is
            // empty. Bank waits on HUP-clear before writing the next
            // file, so the consumer side must release it here.
            // Flushing while bytes are queued would drop the start
            // of the new stream — those bytes are what we need to
            // run detection against.
            let poll = (sys.channel_poll)(s.in_chan, POLL_IN | POLL_HUP);
            let has_hup = (poll as u32) & POLL_HUP != 0;
            let has_in = (poll as u32) & POLL_IN != 0;
            if has_hup && !has_in {
                dev_channel_ioctl(sys, s.in_chan, IOCTL_FLUSH, core::ptr::null_mut(), 0);
                return 0;
            }
            if !has_in {
                return 0;
            }

            // Read bytes into detect_buf
            let space = DETECT_BUF_SIZE - (s.detect_len as usize);
            if space == 0 {
                // Shouldn't happen — detect_format returns at 8 bytes
                // Fallback to WAV
                s.format = FMT_WAV;
                init_codec(s);
                return 0;
            }

            let to_read = if space > DETECT_IO_SIZE {
                DETECT_IO_SIZE
            } else {
                space
            };
            let read = (sys.channel_read)(s.in_chan, s.detect_io.as_mut_ptr(), to_read);

            if read <= 0 {
                return 0;
            }

            // Copy read bytes into detect_buf
            let n = read as usize;
            let offset = s.detect_len as usize;
            let mut i: usize = 0;
            while i < n && (offset + i) < DETECT_BUF_SIZE {
                s.detect_buf[offset + i] = *s.detect_io.as_ptr().add(i);
                i += 1;
            }
            s.detect_len += i as u8;

            let fmt = detect_format(&s.detect_buf, s.detect_len);
            if fmt != FMT_DETECTING {
                s.format = fmt;
                init_codec(s);
                return 2; // Burst — start decoding immediately
            }

            return 0;
        }

        // ----------------------------------------------------------------
        // Active codec phase
        // ----------------------------------------------------------------

        // Audio sub-codecs: reset on upstream HUP. Image formats
        // own their own EOF / drain / reset cycle inside
        // the image family's phase machine — wiping their state here
        // would tear down a 1 MiB RGB565 frame mid-drain (≈ 1000
        // ticks at WRITE_CHUNK = 1024 B/tick).
        // MKV: sub-codec signals Done (HUP + decoder flushed + all
        // frames drained) — flush the boundary and re-arm detection
        // so the next file can start.
        #[cfg(feature = "h264")]
        if s.format == FMT_MKV {
            let mkv = &*(s.codec.0.as_ptr() as *const video::MkvH264State);
            if video::mkv_is_done(mkv) {
                let in_poll = ((*s.syscalls).channel_poll)(s.in_chan, POLL_IN);
                let has_new = (in_poll as u32) & POLL_IN != 0;
                let errored = mkv.phase == video::VPhase::Error;
                // Done: reset immediately. Error: hold (heartbeat
                // replays last_err) until fresh bytes arrive.
                if !errored || has_new {
                    dev_channel_ioctl(s.sys(), s.in_chan, IOCTL_FLUSH, core::ptr::null_mut(), 0);
                    dev_log(s.sys(), 3, b"[dec] rst-mkv".as_ptr(), 13);
                    reset_to_detect(s);
                    return 0;
                }
            }
        }

        if !owns_eof_cycle(s.format) {
            let sys_ptr = s.syscalls;
            let in_chan = s.in_chan;
            let in_poll = ((*sys_ptr).channel_poll)(in_chan, POLL_IN | POLL_HUP);
            let has_hup = (in_poll as u32) & POLL_HUP != 0;
            let has_in = (in_poll as u32) & POLL_IN != 0;

            // Fast path: WAV has reached the header-declared
            // `data_size` and upstream signalled HUP. The stream is
            // over — reset and FLUSH. The FLUSH both drops any
            // trailing junk (some WAV files carry metadata after
            // the data chunk) and clears HUP so a HUP-gated producer
            // can resume. MP3 / AAC don't expose a `sub_done`
            // predicate and fall through to the quiesce path.
            let sub_done = match s.format {
                #[cfg(feature = "wav")]
                FMT_WAV => audio::wav::wav_is_done(s.wav()),
                _ => false,
            };
            if has_hup && sub_done {
                dev_log(&*sys_ptr, 3, b"[dec] rst-adv".as_ptr(), 13);
                dev_channel_ioctl(&*sys_ptr, in_chan, IOCTL_FLUSH, core::ptr::null_mut(), 0);
                reset_to_detect(s);
                return 0;
            }

            // Quiesce path: HUP is set and the ring has drained.
            // Wait `HUP_QUIESCE_TICKS` consecutive ticks to give the
            // sub-codec a chance to flush its internal output (the
            // wasm fetch consumer drops the last 250 ms of audio
            // without it), then FLUSH + reset.
            if has_hup && !has_in {
                s.hup_quiet_ticks = s.hup_quiet_ticks.saturating_add(1);
                if s.hup_quiet_ticks >= HUP_QUIESCE_TICKS {
                    dev_channel_ioctl(&*sys_ptr, in_chan, IOCTL_FLUSH, core::ptr::null_mut(), 0);
                    dev_log(&*sys_ptr, 3, b"[dec] rst".as_ptr(), 9);
                    reset_to_detect(s);
                    return 0;
                }
            } else {
                // Fresh input or HUP-cleared — restart the window.
                s.hup_quiet_ticks = 0;
            }
        }

        // Dispatch to detected codec's step function
        match s.format {
            #[cfg(feature = "wav")]
            FMT_WAV => audio::wav::wav_step(s.wav()),
            #[cfg(feature = "mp3")]
            FMT_MP3 => audio::mp3::mp3_step(s.mp3()),
            #[cfg(feature = "aac")]
            FMT_AAC => audio::aac::aac_step(s.aac()),
            #[cfg(feature = "image")]
            FMT_BMP | FMT_GIF | FMT_PNG | FMT_JPEG => image::image_step(s.img()),
            #[cfg(feature = "h264")]
            FMT_MKV => video::mkv_step(s.mkv()),
            _ => 0,
        }
    }
}

// ============================================================================
// Panic Handler
// ============================================================================

// Wasm entry-point wrappers — no-op on non-wasm targets. See
// `modules/sdk/runtime/wasm_entry.rs` for the wasm32 module_init_wasm /
// module_step_wasm definitions.
include!("../../../target/fluxor/fluxor-abi/sdk/runtime/wasm_entry.rs");

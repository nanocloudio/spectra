//! Audio essence family — the façade the codec root talks to.
//!
//! Exposes the same `detect` / `init` / `feed_detect` / `step` / `is_done`
//! contract as the [`image`](super::image) and [`video`](super::video)
//! families, so the root dispatches to all three the same way.
//!
//! One file per format beside this one — [`wav`] (PCM passthrough),
//! [`mp3`] (an f32 port of `minimp3`) — or one directory when a format
//! needs more than one file, which is [`aac`] and its six generated table
//! files. That is the same rule [`super::video`] follows for `h264/`.
//!
//! # Why this family has no shared chassis
//!
//! [`image`](super::image) hands every format ONE `ImageState`: the four
//! image formats differ only in how they turn accumulated bytes into a
//! raster, and share the accumulator, the scaler and the output path. The
//! audio formats share nothing comparable — WAV is a header parse and a
//! byte passthrough, MP3 and AAC are streaming transform decoders with
//! their own frame machines, bit reservoirs and overlap state. Forcing a
//! common state struct on them would mean a union of three unrelated
//! machines, which is precisely the `CODEC_STATE_SIZE` overlay the ROOT
//! already provides.
//!
//! So the shared surface here is the format enum, the sniffing, and the
//! seam below — not a state type. The families are symmetric in their
//! contract, not forced to be identical in their internals.
//!
//! # The seam
//!
//! The `use super::…` re-exports below are what let every file in this
//! subtree keep saying `super::dev_log` regardless of how deep it sits:
//! Rust resolves a private import from any descendant module, so `aac/`
//! two levels down reaches the root through the same one line as `wav.rs`
//! one level down. It also means the root's surface area to this family is
//! visible in one place rather than scattered across the format files.

use super::abi;
use super::input;
#[allow(
    unused_imports,
    reason = "the seam re-exports the root surface the whole subtree draws on; which subset is live depends on the enabled format features"
)]
use super::{
    __aeabi_memclr, dev_channel_ioctl, dev_log, drain_pending, fmt_i16_raw, fmt_u32_raw, p_u32,
    track_pending, E_AGAIN, IOCTL_NOTIFY, POLL_IN, POLL_OUT,
};

// `mp3.rs` is an f32 port of CC0 `minimp3`. Its value is being diffable
// against the C original — a reviewer must be able to put the two side by
// side and see the same constants and the same loop shapes. Restyling it to
// satisfy these lints would destroy exactly that, for code already proven
// byte-exact against the reference decoder.
//
// The exemption sits HERE, on the port, rather than at the crate root, so the
// clean-room families (`aac/`, `image/`, `video/`, and this file) stay fully
// strict. `aac/` is a clean-room implementation from a specification and
// needs none of it.
//
//   excessive_precision / approx_constant  DSP constants and window tables
//   identity_op / erasing_op               minimp3 writes strides out in full
//                                          (`z[0*64]`, `y[0*18]`); each was
//                                          checked individually — none masks
//                                          a bug
//   needless_range_loop / too_many_arguments / manual_memcpy /
//   assign_op_pattern / unnecessary_cast / collapsible_if /
//   manual_range_contains / implicit_saturating_sub /
//   field_reassign_with_default
//                                          1:1 with the C control flow
#[cfg(feature = "aac")]
pub mod aac;
#[allow(
    clippy::excessive_precision,
    clippy::approx_constant,
    clippy::identity_op,
    clippy::erasing_op,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::manual_memcpy,
    clippy::assign_op_pattern,
    clippy::unnecessary_cast,
    clippy::collapsible_if,
    clippy::manual_range_contains,
    clippy::implicit_saturating_sub,
    clippy::field_reassign_with_default,
    reason = "inherent to the minimp3 port; see the comment above"
)]
#[cfg(feature = "mp3")]
pub mod mp3;
#[cfg(feature = "wav")]
pub mod wav;

/// Audio formats this build can decode.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum AudioFormat {
    Wav = 0,
    Mp3 = 1,
    Aac = 2,
}

/// Identify an audio format from the first bytes of a stream, or `None`
/// when this family does not claim them.
///
/// Same contract as [`super::image::detect`]: `None` means "not mine", not
/// "not yet". Unlike the image and container families these are not fixed
/// magic strings — MP3 and AAC are identified by their sync words, which
/// is why the checks are spelled out rather than table-driven.
pub fn detect(buf: &[u8]) -> Option<AudioFormat> {
    // RIFF — the WAV container's own magic.
    if buf.len() >= 4 && &buf[..4] == b"RIFF" {
        return Some(AudioFormat::Wav);
    }
    // ID3v2 tag: an MP3 whose first frame is preceded by metadata.
    if buf.len() >= 3 && &buf[..3] == b"ID3" {
        return Some(AudioFormat::Mp3);
    }
    if buf.len() < 2 || buf[0] != 0xFF {
        return None;
    }
    let b1 = buf[1];
    // MPEG audio sync: 11 set bits. Layer field (bits 2:1) == 01 is
    // Layer III, i.e. MP3. Layer I/II are not decoded here, so they are
    // deliberately NOT claimed — better an unclaimed stream than an MP3
    // decoder fed Layer II frames.
    if (b1 & 0xE0) == 0xE0 && (b1 >> 1) & 0x03 == 0x01 {
        return Some(AudioFormat::Mp3);
    }
    // ADTS sync: 0xFFF, then the layer field must be 00 (bits 2:1) —
    // which is what separates ADTS from the MPEG-audio sync above.
    if (b1 & 0xF0) == 0xF0 && (b1 & 0x06) == 0x00 {
        return Some(AudioFormat::Aac);
    }
    None
}

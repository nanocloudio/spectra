//! The elementary-stream boundary — where containers meet decoders.
//!
//! Design and rationale: `docs/reference/elementary-stream.md`. The short
//! version is that gluing each container to each decoder directly is a cross
//! product (~20 files for the target matrix); putting a seam in the middle
//! makes it a sum (6 parsers + 9 decoders), and every new container then works
//! with every existing codec for free.
//!
//! Three properties of this contract are load-bearing and easy to get wrong:
//!
//! 1. **Every event names its track.** Matroska's existing sink finds *the*
//!    video track and discards the rest, which is why no audio in a Matroska
//!    file currently reaches a decoder at all. MP4, TS and PS routinely carry
//!    both, and M4A is audio-only.
//!
//! 2. **Timestamps stay in track ticks with a timescale** — never normalised
//!    to microseconds here. Normalising costs a 64-bit division, and the
//!    32-bit PIC targets cannot link `__aeabi_uldivmod`; the codec module's video family already
//!    documents working around exactly that. Containers also genuinely differ:
//!    Matroska scales by `TimestampScale`, MP4 has a per-track timescale, TS
//!    counts at 90 kHz.
//!
//! 3. **Framing is declared, never silently converted.** See [`EsFraming`].
//!
//! This module is data and traits only: no I/O, no allocation, no Fluxor ABI.
//! That is what lets the same parser feed a PIC decoder, a host test, and a
//! browser adapter.

#![allow(
    dead_code,
    reason = "the contract is defined in full; each consumer uses a subset, \
              and a partially-defined boundary is worse than an unused variant"
)]

/// Which port a track's output eventually reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EsKind {
    Video,
    Audio,
    /// Surfaced rather than dropped so a caller can report "this file has
    /// subtitles we do not decode" instead of silently ignoring the track.
    Subtitle,
}

/// Codec identity, independent of the container that carried it.
///
/// Deliberately not `#[non_exhaustive]`: this lives in the same repository as
/// every consumer, and an exhaustive match is how a new codec is guaranteed to
/// be considered at each decision point rather than falling into a
/// catch-all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EsCodec {
    // Video
    H264,
    H265,
    Mpeg2Video,
    Mpeg1Video,
    // Audio
    AacLc,
    Mp3,
    Mp2,
    Ac3,
    Eac3,
    Dts,
    TrueHd,
    Flac,
    Opus,
    Vorbis,
    PcmS16Le,
    PcmS16Be,
    /// Recognised as a track, not as something we can decode. Carries the
    /// container's own identifier where one exists, so a diagnostic can say
    /// which codec was refused.
    Unsupported,
}

impl EsCodec {
    /// Which port this codec's output belongs on, or `None` when the codec is
    /// not something we decode.
    #[must_use]
    pub const fn kind(self) -> Option<EsKind> {
        match self {
            Self::H264 | Self::H265 | Self::Mpeg2Video | Self::Mpeg1Video => Some(EsKind::Video),
            Self::AacLc
            | Self::Mp3
            | Self::Mp2
            | Self::Ac3
            | Self::Eac3
            | Self::Dts
            | Self::TrueHd
            | Self::Flac
            | Self::Opus
            | Self::Vorbis
            | Self::PcmS16Le
            | Self::PcmS16Be => Some(EsKind::Audio),
            Self::Unsupported => None,
        }
    }
}

/// How access units are delimited in the byte stream.
///
/// **This is declared, not normalised, and that is deliberate.** The obvious
/// design is to convert everything to Annex B at the boundary so decoders see
/// one shape. It would break the browser path: Fluxor's WASM video adapter
/// states that WebCodecs "consumes length-prefixed samples as stored in
/// Matroska blocks … so no Annex B re-framing happens anywhere on this path".
/// Forcing conversion here would make that consumer convert back.
///
/// So the parser reports what it has and the consumer decides. Software
/// decoders that want Annex B call [`annexb_len`] and [`to_annexb`]; a
/// hardware or browser decoder takes the bytes as stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EsFraming {
    /// NAL units each prefixed with a big-endian length of `bytes` octets.
    /// avcC / hvcC framing — Matroska and ISOBMFF.
    LengthPrefixed { bytes: u8 },
    /// Annex B start codes, parameter sets in band. MPEG-TS, MPEG-PS, raw
    /// elementary streams.
    AnnexB,
    /// Self-delimiting frames; one access unit per `on_au_*` cycle. ADTS,
    /// MP3, AC-3.
    Framed,
    /// No framing at all. PCM.
    Raw,
}

/// A track the container declared.
///
/// Fields that do not apply to the track's kind are zero rather than
/// `Option`-wrapped: this crosses into a `no_std` PIC module where the
/// discriminants cost more than they explain, and "0 pixels wide" is not a
/// value a real video track takes.
#[derive(Clone, Copy, Debug)]
pub struct EsTrack<'a> {
    /// Container-assigned track number. Stable for the life of the stream.
    pub id: u16,
    pub kind: EsKind,
    pub codec: EsCodec,
    pub framing: EsFraming,
    /// avcC, hvcC, AudioSpecificConfig, … Empty when the container carries
    /// parameter sets in band, which is the normal case for MPEG-TS.
    pub codec_private: &'a [u8],
    /// Ticks per second for this track's timestamps. See the module docs on
    /// why this is not normalised.
    pub timescale_hz: u32,
    /// Video only.
    pub width: u32,
    pub height: u32,
    /// Audio only.
    pub sample_rate_hz: u32,
    pub channels: u8,
}

impl EsTrack<'_> {
    /// A minimal track, for parsers to fill in field by field.
    #[must_use]
    pub const fn new(id: u16, kind: EsKind, codec: EsCodec, framing: EsFraming) -> Self {
        Self {
            id,
            kind,
            codec,
            framing,
            codec_private: &[],
            timescale_hz: 0,
            width: 0,
            height: 0,
            sample_rate_hz: 0,
            channels: 0,
        }
    }
}

/// One access unit — a coded frame for video, a coded frame or block for
/// audio.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EsAccessUnit {
    pub track: u16,
    /// Presentation time in this track's ticks. `None` when the container
    /// omits it, which Matroska does for anything but the first block of a
    /// lace.
    pub pts: Option<i64>,
    /// Decode time in this track's ticks.
    ///
    /// Equal to `pts` where there is no reordering, which is every stream the
    /// decoder currently handles — Constrained Baseline has no B-frames. It
    /// stops being redundant the moment Main or High profile arrives, which
    /// is what MP4 and TS actually carry, so the field exists now rather than
    /// being retrofitted through every parser later.
    pub dts: Option<i64>,
    pub keyframe: bool,
}

impl EsAccessUnit {
    #[must_use]
    pub const fn new(track: u16) -> Self {
        Self {
            track,
            pts: None,
            dts: None,
            keyframe: false,
        }
    }
}

/// Container-level failures. Every variant is fatal for the current stream;
/// the caller resets the parser to recover.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EsError {
    /// Malformed container structure.
    Structure,
    /// Nesting deeper than the parser's fixed stack.
    TooDeep,
    /// A construct the parser does not implement yet.
    Unsupported,
    /// A retained field exceeded its fixed buffer.
    TooBig,
}

/// What a container parser pushes into.
///
/// Streaming is part of the contract: [`on_au_data`](EsSink::on_au_data) may
/// be called any number of times per access unit with whatever chunks the
/// transport produced, so parsers stay O(1) in memory and the module's input
/// port can remain a pipe.
///
/// There is deliberately no `Demux` trait to pair with this. A parser exposes
/// an inherent `feed<S: EsSink>(&mut self, data: &[u8], sink: &mut S)`; a
/// trait with a generic method would not be object-safe, the dispatcher picks
/// the parser by format sniff anyway, and static dispatch is what a PIC module
/// wants.
pub trait EsSink {
    /// A track has been fully described. May fire for tracks the consumer has
    /// no decoder for — inspect [`EsTrack::codec`] before committing to one.
    fn on_track(&mut self, track: &EsTrack<'_>);

    /// Every track in the container is now known.
    ///
    /// The point at which selection can happen: a consumer that wants "the
    /// first audio track that is not commentary" cannot decide on the first
    /// `on_track` alone. Parsers that discover tracks lazily (MPEG-TS learns
    /// them from the PMT, and a stream can add one mid-flight) may call this
    /// more than once.
    fn on_tracks_complete(&mut self) {}

    fn on_au_begin(&mut self, au: &EsAccessUnit);
    fn on_au_data(&mut self, data: &[u8]);
    fn on_au_end(&mut self);

    /// End of stream. Distinct from the input simply going quiet — it is what
    /// lets a decoder flush frames it is holding for reordering.
    fn on_eos(&mut self) {}

    /// Fatal; no further events fire for this stream.
    fn on_error(&mut self, err: EsError);
}

// ── Framing helpers ─────────────────────────────────────────────────────────
//
// Opt-in, per the `EsFraming` contract: a consumer that wants Annex B calls
// these, and one that wants the bytes as stored does not.

/// Length of the Annex B form of a length-prefixed access unit.
///
/// Each `bytes`-octet length prefix becomes a 4-byte start code, so the answer
/// is only the same as the input when `bytes == 4`. Returns `None` if the
/// input is malformed — a prefix that overruns the buffer.
#[must_use]
pub fn annexb_len(data: &[u8], bytes: u8) -> Option<usize> {
    let n = bytes as usize;
    if n == 0 || n > 4 {
        return None;
    }
    let mut off = 0usize;
    let mut out = 0usize;
    while off < data.len() {
        if off + n > data.len() {
            return None;
        }
        let mut len = 0usize;
        for k in 0..n {
            len = (len << 8) | data[off + k] as usize;
        }
        off += n;
        if off + len > data.len() {
            return None;
        }
        off += len;
        out += 4 + len;
    }
    Some(out)
}

/// Rewrite length-prefixed NAL units as Annex B start codes.
///
/// Writes into `out` and returns the number of bytes written, or `None` if
/// `out` is too small or the input is malformed. Call [`annexb_len`] first to
/// size the buffer.
///
/// The 4-byte start code is used unconditionally rather than the 3-byte short
/// form: h264bsd accepts both, and a fixed width means the output length is
/// computable in advance, which a bounded-memory decoder needs.
pub fn to_annexb(data: &[u8], bytes: u8, out: &mut [u8]) -> Option<usize> {
    let n = bytes as usize;
    if n == 0 || n > 4 {
        return None;
    }
    let mut off = 0usize;
    let mut w = 0usize;
    while off < data.len() {
        if off + n > data.len() {
            return None;
        }
        let mut len = 0usize;
        for k in 0..n {
            len = (len << 8) | data[off + k] as usize;
        }
        off += n;
        if off + len > data.len() || w + 4 + len > out.len() {
            return None;
        }
        out[w] = 0;
        out[w + 1] = 0;
        out[w + 2] = 0;
        out[w + 3] = 1;
        w += 4;
        out[w..w + len].copy_from_slice(&data[off..off + len]);
        w += len;
        off += len;
    }
    Some(w)
}

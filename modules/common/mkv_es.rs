//! Matroska → elementary-stream adapter (T3.1.2).
//!
//! Presents [`MkvDemux`](crate::mkv_demux::MkvDemux) output through the
//! container-independent [`EsSink`] contract, so a decoder binds to the
//! boundary rather than to Matroska.
//!
//! ## Why an adapter and not a rewrite
//!
//! The obvious move is to change `MkvDemux` to emit `EsSink` events directly
//! and delete `MkvSink`. That was rejected: the demuxer is a working,
//! conformance-tested parser with 64 tests and two shipping consumers bound to
//! its current shape, and reworking its callback sites would put all of that
//! at risk to gain nothing the adapter does not already provide. Translating
//! at the edge keeps the parser untouched and both contracts live.
//!
//! The cost is honest and worth stating: `MkvSink`'s two callback families
//! (video and audio) mean this adapter can only ever surface the *selected*
//! video and audio track. That is the demuxer's own first-supported-wins
//! policy, not a limitation introduced here — but a caller wanting every track
//! in a multi-track file needs T3.1b.4, and will not get it by using this
//! type.
//!
//! ## Usage
//!
//! ```ignore
//! let mut sink = MyDecoder::new();
//! let mut adapter = MkvEs::new(&mut sink);
//! demux.feed(bytes, &mut adapter);
//! ```

use crate::es::{EsAccessUnit, EsCodec, EsError, EsFraming, EsKind, EsSink, EsTrack};
use crate::mkv_demux::{AudioCodec, AudioTrackInfo, MkvError, MkvSink, VideoCodec, VideoTrackInfo};

/// Nanoseconds in a second — the numerator when converting Matroska's
/// nanoseconds-per-tick into the boundary's ticks-per-second.
const NANOS_PER_SEC: u32 = 1_000_000_000;

/// Wraps an [`EsSink`] so a Matroska demuxer can drive it.
pub struct MkvEs<'s, S: EsSink> {
    sink: &'s mut S,
    /// Track number of the selected video track, learned from `on_video_track`.
    /// Zero until then — Matroska track numbers are positive, so zero is
    /// unambiguously "none".
    video_id: u16,
    audio_id: u16,
    /// Which track the access unit currently open belongs to.
    open_id: u16,
    /// `on_tracks_complete` has fired. See [`Self::tracks_done`].
    completed: bool,
    /// A track was described but its number did not fit the boundary's `u16`.
    /// Latched so the resulting frames are dropped rather than mislabelled.
    overflowed: bool,
}

impl<'s, S: EsSink> MkvEs<'s, S> {
    pub fn new(sink: &'s mut S) -> Self {
        Self {
            sink,
            video_id: 0,
            audio_id: 0,
            open_id: 0,
            completed: false,
            overflowed: false,
        }
    }

    /// Matroska requires Tracks to precede Clusters, so by the time any frame
    /// arrives the track list is final. That makes the first access unit the
    /// correct — and only available — moment to fire `on_tracks_complete`:
    /// `MkvSink` has no end-of-Tracks event to translate.
    ///
    /// A consumer that selects among tracks therefore learns the full set
    /// before it must decide anything, which is the property the callback
    /// exists to provide.
    fn tracks_done(&mut self) {
        if !self.completed {
            self.completed = true;
            self.sink.on_tracks_complete();
        }
    }

    fn begin(&mut self, id: u16, ts: i64, keyframe: bool) {
        if id == 0 {
            return;
        }
        self.tracks_done();
        self.open_id = id;
        let mut au = EsAccessUnit::new(id);
        au.pts = Some(ts);
        // Matroska stores no separate decode timestamp. For streams without
        // reordering these coincide; where B-frames reorder, the container
        // simply does not carry DTS and a consumer that needs it derives it
        // from the bitstream. Asserting pts == dts here would be a claim the
        // container never made.
        au.dts = None;
        au.keyframe = keyframe;
        self.sink.on_au_begin(&au);
    }
}

/// Matroska's nanoseconds-per-tick to the boundary's ticks-per-second.
///
/// **Deliberately a 32-bit division.** The natural expression is
/// `1_000_000_000u64 / scale`, and on the 32-bit PIC targets that emits
/// `__aeabi_uldivmod`, which does not link — a trap this codebase has hit
/// three times, and one that only the rp2350 build catches. Both operands fit
/// `u32` (the numerator is 1e9 against a 4.29e9 ceiling; the default scale is
/// 1e6, i.e. millisecond ticks), so narrowing first is exact for every value
/// the format realistically carries.
///
/// A scale of zero, or one too large for `u32`, is not representable and
/// yields zero — reported as an unusable timescale rather than a wrong one.
#[must_use]
fn timescale_hz(scale_ns: u64) -> u32 {
    if scale_ns == 0 || scale_ns > u64::from(u32::MAX) {
        return 0;
    }
    NANOS_PER_SEC / (scale_ns as u32)
}

/// NAL length-prefix size from an avcC or hvcC configuration record.
///
/// Both records store it in the low two bits of a fixed byte, biased by one:
/// avcC at offset 4, hvcC at offset 21. Absent or truncated codec private
/// data falls back to 4, the value every real muxer writes.
#[must_use]
fn nal_length_size(codec: VideoCodec, private: &[u8]) -> u8 {
    let offset = match codec {
        VideoCodec::H264 => 4,
        VideoCodec::H265 => 21,
        VideoCodec::Unknown => return 4,
    };
    match private.get(offset) {
        Some(b) => (b & 0x03) + 1,
        None => 4,
    }
}

#[must_use]
const fn map_video(codec: VideoCodec) -> EsCodec {
    match codec {
        VideoCodec::H264 => EsCodec::H264,
        VideoCodec::H265 => EsCodec::H265,
        VideoCodec::Unknown => EsCodec::Unsupported,
    }
}

#[must_use]
const fn map_audio(codec: AudioCodec) -> EsCodec {
    match codec {
        AudioCodec::AacLc => EsCodec::AacLc,
        AudioCodec::Mp3 => EsCodec::Mp3,
        AudioCodec::Mp2 => EsCodec::Mp2,
        AudioCodec::Ac3 => EsCodec::Ac3,
        AudioCodec::Eac3 => EsCodec::Eac3,
        AudioCodec::Dts => EsCodec::Dts,
        AudioCodec::TrueHd => EsCodec::TrueHd,
        AudioCodec::Flac => EsCodec::Flac,
        AudioCodec::Opus => EsCodec::Opus,
        AudioCodec::Vorbis => EsCodec::Vorbis,
        AudioCodec::PcmS16Le => EsCodec::PcmS16Le,
        AudioCodec::PcmS16Be => EsCodec::PcmS16Be,
        AudioCodec::Other => EsCodec::Unsupported,
    }
}

#[must_use]
const fn map_error(err: MkvError) -> EsError {
    match err {
        MkvError::Structure => EsError::Structure,
        MkvError::TooDeep => EsError::TooDeep,
        MkvError::Lacing => EsError::Unsupported,
        MkvError::PrivateTooBig => EsError::TooBig,
    }
}

/// Matroska track number narrowed to the boundary's `u16`.
///
/// The format permits an EBML unsigned integer, so the two types genuinely
/// differ. In practice muxers assign small numbers (the spec's own guidance is
/// 1..=127) and nothing above 65535 has ever been observed. Rather than
/// saturate — which would silently merge two tracks into one id, the worst
/// available outcome — an unrepresentable number is refused.
#[must_use]
fn narrow_id(number: u64) -> Option<u16> {
    u16::try_from(number).ok().filter(|n| *n != 0)
}

impl<S: EsSink> MkvSink for MkvEs<'_, S> {
    fn on_video_track(&mut self, info: &VideoTrackInfo<'_>) {
        let Some(id) = narrow_id(info.number) else {
            self.overflowed = true;
            self.sink.on_error(EsError::Unsupported);
            return;
        };
        self.video_id = id;

        let mut track = EsTrack::new(
            id,
            EsKind::Video,
            map_video(info.codec),
            EsFraming::LengthPrefixed {
                bytes: nal_length_size(info.codec, info.codec_private),
            },
        );
        track.codec_private = info.codec_private;
        track.timescale_hz = timescale_hz(info.timestamp_scale);
        track.width = info.pixel_width;
        track.height = info.pixel_height;
        self.sink.on_track(&track);
    }

    fn on_audio_track(&mut self, info: &AudioTrackInfo<'_>) {
        let Some(id) = narrow_id(info.number) else {
            self.overflowed = true;
            self.sink.on_error(EsError::Unsupported);
            return;
        };
        self.audio_id = id;

        // Every Matroska block is one complete audio frame — lacing is
        // unpacked by the demuxer before it reaches here — so compressed
        // audio is `Framed`. PCM has no frames to delimit at all.
        let framing = match info.codec {
            AudioCodec::PcmS16Le | AudioCodec::PcmS16Be => EsFraming::Raw,
            _ => EsFraming::Framed,
        };

        let mut track = EsTrack::new(id, EsKind::Audio, map_audio(info.codec), framing);
        track.codec_private = info.codec_private;
        track.timescale_hz = timescale_hz(info.timestamp_scale);
        track.sample_rate_hz = info.sample_rate_hz;
        track.channels = info.channels;
        self.sink.on_track(&track);
    }

    fn on_frame_begin(&mut self, timestamp_ticks: i64, keyframe: bool) {
        let id = self.video_id;
        self.begin(id, timestamp_ticks, keyframe);
    }

    fn on_frame_data(&mut self, data: &[u8]) {
        if self.open_id != 0 {
            self.sink.on_au_data(data);
        }
    }

    fn on_frame_end(&mut self) {
        if self.open_id != 0 {
            self.open_id = 0;
            self.sink.on_au_end();
        }
    }

    fn on_audio_frame_begin(&mut self, timestamp_ticks: i64, keyframe: bool) {
        let id = self.audio_id;
        self.begin(id, timestamp_ticks, keyframe);
    }

    fn on_audio_frame_data(&mut self, data: &[u8]) {
        if self.open_id != 0 {
            self.sink.on_au_data(data);
        }
    }

    fn on_audio_frame_end(&mut self) {
        if self.open_id != 0 {
            self.open_id = 0;
            self.sink.on_au_end();
        }
    }

    fn on_error(&mut self, err: MkvError) {
        self.sink.on_error(map_error(err));
    }
}

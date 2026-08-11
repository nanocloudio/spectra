# The elementary-stream boundary

Design for the internal contract between container parsers and decoders.
Written 2026-07-27, before implementation, because it changes a core that
Fluxor's WASM adapter also consumes.

Status: **proposed**. Nothing below is implemented yet.

## 1. Why

Today one file glues one container to one decoder. `codec/video/mod.rs` demuxes
Matroska, re-frames avcC NALs to Annex B, drives h264bsd, converts YUV to
RGB565 and writes the pixels port — all in 744 lines with no seam between the
container and the codec.

That is fine for one container and one codec. The target list is MKV, MP4,
M4A, MOV, MPEG-TS and MPEG-PS against H.264, H.265, MPEG-2 video, AAC, MP3,
MP2, AC-3 and PCM. Pairing those the current way is a **cross product** —
`mp4_h264.rs`, `ts_mpeg2.rs`, `ps_ac3.rs` … around twenty files, each with its
own buffering and timestamp handling, each a place for the same bug to be
fixed differently.

With a boundary in the middle it is a **sum**: six demuxers plus nine decoders,
and every new container works with every existing codec for free.

This is not the module split that was considered and rejected. Nothing here
changes the graph, the ports, the manifest, the variants, or the module's
public identity. It is entirely internal.

## 2. The seam already half-exists

`mkv_demux.rs` does not call the decoder. It pushes into an `MkvSink` trait —
`on_video_track`, `on_frame_begin`, `on_frame_data`, `on_frame_end`,
`on_error` — and the struct in `codec/video/mod.rs` that implements it is already
called `EsSink`. The boundary is there; it is just Matroska-shaped and
video-only.

So this is a generalisation, not a new architecture.

## 3. Three decisions

Everything hard about the design is in these three. Get them wrong and the
work is redone when MP4 or TS lands.

### 3.1 Tracks — events carry an id, and audio is a first-class citizen

`MkvSink::on_video_track` finds *the* video track and ignores the rest. MP4,
TS and PS routinely carry audio and video together, and M4A is audio-only, so
the sink must describe any number of tracks of either kind and every
access-unit event must say which track it belongs to.

The module still decodes one video and one audio track at a time — the
`pixels` and `audio` ports admit exactly that. Track selection becomes a
policy the dispatcher applies to a full track list, rather than a rule baked
into each parser.

### 3.2 Timestamps — track ticks and a timescale, never normalised units

Tempting to normalise everything to microseconds at the boundary. Two reasons
not to.

**It would cost a 64-bit division on the 32-bit targets.** `codec/video/mod.rs`
already documents avoiding exactly this: *"a u64 division would pull in
`__aeabi_uldivmod`, which the 32-bit PIC targets can't link"*, and it keeps its
timestamp arithmetic in u32 as a result. Normalising at the boundary would
reintroduce the division for every container.

**Different containers have genuinely different clocks** — MKV ticks with a
`TimestampScale`, MP4 has a per-track timescale, TS uses a 90 kHz PTS. Passing
ticks plus the timescale keeps the parser honest and lets the consumer pick
its own arithmetic.

**DTS becomes real.** MKV Constrained Baseline has no reordering, so one
timestamp suffices and the current code carries only PTS. Main and High
profile with B-frames — which is what MP4 and TS actually contain — need
decode and presentation order separately. Both are optional, because some
containers omit them.

### 3.3 Framing — declared, never silently converted

This is the one that would be easiest to get wrong.

H.264 appears in two shapes. MP4 and MKV store length-prefixed NALs with
parameter sets out of band in an avcC record. MPEG-TS and PS store Annex B
start codes with parameter sets in band. `codec/video/mod.rs` currently converts the
first to the second on the way to h264bsd.

The obvious move is to normalise to Annex B at the boundary so decoders see
one shape. **That would break the WebCodecs path.** Fluxor's WASM adapter says
so explicitly: *"WebCodecs consumes length-prefixed samples as stored in
Matroska blocks … so no Annex B re-framing happens anywhere on this path."*
A boundary that forces conversion makes the browser consumer strictly worse —
it would have to convert back.

So framing is **declared on the track** and the consumer decides. The software
decoder converts when it needs Annex B; the WebCodecs adapter takes the bytes
as stored. Conversion becomes an opt-in helper, not a property of the seam.

## 4. The contract

```rust
pub enum EsKind { Video, Audio }

pub enum EsCodec {
    H264, H265, Mpeg2Video, Mpeg1Video,
    AacLc, Mp3, Mp2, Ac3, PcmS16Le, PcmS16Be,
    Unknown,
}

pub enum EsFraming {
    /// NALs prefixed with an N-byte length (avcC/hvcC — MP4, MKV).
    LengthPrefixed { bytes: u8 },
    /// Start codes, parameter sets in band (MPEG-TS, MPEG-PS, raw .264).
    AnnexB,
    /// Self-delimiting frames, one access unit per callback (ADTS, MP3, AC-3).
    Framed,
    /// No framing (PCM).
    Raw,
}

pub struct EsTrack<'a> {
    pub id: u16,
    pub kind: EsKind,
    pub codec: EsCodec,
    pub framing: EsFraming,
    /// avcC / hvcC / AudioSpecificConfig. Empty for in-band containers.
    pub codec_private: &'a [u8],
    /// Ticks per second for this track's timestamps (§3.2).
    pub timescale_hz: u32,
    pub width: u32,          // video, 0 otherwise
    pub height: u32,
    pub sample_rate_hz: u32, // audio, 0 otherwise
    pub channels: u8,
}

pub struct EsAccessUnit {
    pub track: u16,
    /// Presentation time in track ticks. `None` when the container omits it.
    pub pts: Option<i64>,
    /// Decode time. Equals `pts` where there is no reordering.
    pub dts: Option<i64>,
    pub keyframe: bool,
}

pub trait EsSink {
    fn on_track(&mut self, track: &EsTrack<'_>);
    /// Every track is now known — the point at which selection can happen.
    fn on_tracks_complete(&mut self) {}
    fn on_au_begin(&mut self, au: &EsAccessUnit);
    fn on_au_data(&mut self, data: &[u8]);
    fn on_au_end(&mut self);
    fn on_eos(&mut self) {}
    fn on_error(&mut self, err: EsError);
}
```

Deliberately **no `Demux` trait.** Each parser keeps an inherent
`feed<S: EsSink>(&mut self, data: &[u8], sink: &mut S)`, matching
`MkvDemux::feed` today. A trait with a generic method is not object-safe, the
dispatcher in `mod.rs` selects the parser by format sniff anyway, and static
dispatch is what a PIC module wants. A trait here would buy nothing and cost
monomorphisation control.

Streaming is preserved exactly: `on_au_data` may be called many times per
access unit with whatever chunks arrive, so parsers stay O(1) in memory and
the input port can remain a pipe.

## 5. What each parser owes

| Container | Framing | Timescale | Notes |
| --- | --- | --- | --- |
| Matroska / WebM | `LengthPrefixed` | `TimestampScale` | exists; needs audio tracks + track ids |
| ISOBMFF (MP4/M4A/MOV) | `LengthPrefixed` | per-track `mdhd` | one parser covers all three |
| MPEG-TS | `AnnexB` / `Framed` | 90 kHz | PAT/PMT for tracks; PES for AUs |
| MPEG-PS (VOB) | `AnnexB` / `Framed` | 90 kHz | DVD; shares the PES layer with TS |
| ADTS / MP3 / AC-3 | `Framed` | sample rate | "containers" that are just framing |
| RIFF/WAV | `Raw` | sample rate | exists |

## 6. Order of work

1. **Land the contract** in `modules/common/es.rs` and move MKV onto
   it, with no new containers. The differential parity suite is the safety
   net: it compares against Fluxor's untouched original, so any behavioural
   drift during the refactor fails immediately and names the fixture.
2. **Split `codec/video/mod.rs`** into an H.264 decoder that consumes `EsSink` events
   and a thin RGB565 emit path. This is where the cross product collapses.
3. **Add ISOBMFF**, which then reaches H.264 with no new glue. First proof the
   boundary pays for itself.
4. **Add the PES layer, then TS and PS on top of it.** Both DVD and broadcast.
5. **Extend H.264 to Main/High** — CABAC, B-frames, 8x8 transform, interlace.
   Independent of all the above, and larger than any of it. Nothing from MP4 or
   TS decodes without it, so it gates the value of steps 3 and 4 even though
   it does not gate their code.

## 7. Cost to Fluxor

Fluxor's WASM adapter path-imports **Fluxor's own** copy of `mkv_demux.rs`, not
Spectra's, so none of this breaks it today. T2.3.2 — repointing that adapter at
the shared core — will have to adopt `EsSink`, which is more work than
repointing an import.

That is the right trade: the adapter currently re-demuxes in the browser
because no elementary-stream surface existed to hand it. §3.3 exists precisely
so that adapter can consume this boundary without a conversion it does not
want.

## 8. Measured context — what the target library needs

The stated library is Blu-ray and 4K, all Matroska. On this Pi 5:

| Content | Decoder | Realtime |
| --- | --- | --- |
| 4K HEVC Main10 | `rpi-hevc-dec` hardware (`/dev/video19`) | **0.98x** |
| 4K HEVC Main10 | ffmpeg software, 4 threads | 0.34x |
| 1080p H.264 CBP | ffmpeg software, 4 threads | 8.34x |
| 1080p H.264 CBP | Spectra, single thread | 0.75x |

ffmpeg's HEVC decoder is mature, NEON-optimised and multithreaded, and it is
still 3x short of realtime at 4K Main10. A portable single-threaded decoder
does not close that gap; software 4K HEVC is not a tuning problem on this
class of hardware.

So the boundary designed here matters *more* for that library, not less: it is
what lets the same demux, track selection and timestamp handling feed either a
software decoder or a hardware provider. `docs/specification.md` already draws
that line — "hardware-accelerated decoders may satisfy Spectra codec
capabilities through Fluxor providers" — and §3.3's decision to declare framing
rather than convert it is exactly what a provider needs, for the same reason
WebCodecs needs it.

## 9. What this does not do

- No new graph nodes, ports, manifest entries or variants.
- No change to the `OctetStream` in, `AudioSample` / `VideoRaster` out shape.
- No sniffing change: `mod.rs` still detects format and selects a parser.
- No decoder rewrite. Step 2 moves code across a seam; it does not touch the
  h264bsd port's internals.

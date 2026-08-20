# The elementary-stream boundary

The internal contract between container parsers and decoders.

Source: `modules/common/es.rs` (the contract),
`modules/common/mkv_es.rs` (the Matroska adapter).

## 1. Why a boundary

Gluing each container directly to each decoder is a cross product:
every container/codec pair becomes its own file with its own buffering
and timestamp handling. With a seam in the middle it is a sum —
parsers on one side, decoders on the other — and every new container
works with every existing codec.

The contract is data and traits only: no I/O, no allocation, no
fluxor ABI. That is what lets the same parser feed a PIC decoder, a
host binary, and a browser adapter.

## 2. The contract

A container parser pushes events into an `EsSink`:

- `on_track` describes a track the container declared (`EsTrack`:
  id, kind, codec, framing, codec-private bytes, timescale, and the
  video or audio geometry). It may fire for tracks the consumer has no
  decoder for.
- `on_tracks_complete` marks the point at which every track is known
  and selection can happen. Parsers that discover tracks lazily may
  call it more than once.
- `on_au_begin` / `on_au_data` / `on_au_end` deliver one access unit.
  `on_au_data` may be called any number of times per access unit with
  whatever chunks the transport produced, so parsers stay O(1) in
  memory and the module's input port can remain a pipe.
- `on_eos` is distinct from the input going quiet: it is what lets a
  decoder flush frames held for reordering.
- `on_error` is fatal for the current stream; the caller resets the
  parser to recover. `EsError` distinguishes malformed structure,
  over-deep nesting, unimplemented constructs, and oversized retained
  fields.

There is deliberately no `Demux` trait to pair with the sink. Each
parser exposes an inherent
`feed<S: EsSink>(&mut self, data: &[u8], sink: &mut S)`: a trait with
a generic method is not object-safe, the dispatcher selects the parser
by format sniff anyway, and static dispatch is what a PIC module
wants.

## 3. The three load-bearing decisions

### 3.1 Tracks — every event names its id

`EsTrack` carries a container-assigned id and every access unit names
its track, so the sink can describe any number of tracks of either
kind. Track selection is a policy the consumer applies to the full
track list, not a rule baked into each parser. `EsKind` surfaces
subtitle tracks rather than dropping them, so a caller can report
"this file has subtitles we do not decode" instead of silently
ignoring them.

### 3.2 Timestamps — track ticks and a timescale, never normalised

Timestamps stay in the track's own ticks with `timescale_hz` alongside
(`pts` and `dts`, each optional because some containers omit them).
Normalising to microseconds at the boundary would cost a 64-bit
division that the 32-bit PIC targets cannot link (`__aeabi_uldivmod`),
and containers genuinely differ: Matroska scales by `TimestampScale`,
ISOBMFF has a per-track timescale, MPEG-TS counts at 90 kHz. Passing
ticks plus the timescale keeps the parser honest and lets the consumer
pick its own arithmetic. `dts` exists separately from `pts` because
profiles with frame reordering need decode and presentation order
distinguished; for streams without reordering the two are equal.

### 3.3 Framing — declared, never silently converted

H.264 appears in two shapes: length-prefixed NAL units with parameter
sets out of band in an avcC record (Matroska, ISOBMFF), and Annex B
start codes with parameter sets in band (MPEG-TS, MPEG-PS).
Normalising to Annex B at the seam would force a consumer that wants
the bytes as stored — a WebCodecs-backed or hardware decoder taking
length-prefixed samples directly — to convert back. So `EsFraming` is
declared on the track (`LengthPrefixed { bytes }`, `AnnexB`, `Framed`
for self-delimiting frames such as ADTS, `Raw` for PCM) and the
consumer decides: a software decoder that wants Annex B calls the
opt-in helpers `annexb_len` and `to_annexb`.

## 4. What is wired today

The Matroska demuxer (`modules/common/mkv_demux.rs`) parses into its
own `MkvSink` callback interface, and `modules/common/mkv_es.rs`
adapts that interface to `EsSink`, so a decoder can bind to the
container-independent boundary. The adapter translates at the edge
rather than rewriting the demuxer's callback sites; it surfaces the
demuxer's selected video and audio track, which is the demuxer's own
first-supported-wins selection policy.

The `codec` module's video path currently binds to `MkvSink`
directly (`modules/app/codec/video/mod.rs`).

## 5. The wider container matrix

Status: design target, not wired. Everything in this section is
unimplemented; the contract in §2 is shaped by it, but Matroska is the
only container parser that exists.

| Container | Framing | Timescale | Notes |
| --- | --- | --- | --- |
| ISOBMFF (MP4/M4A/MOV) | `LengthPrefixed` | per-track | one parser covers all three |
| MPEG-TS | `AnnexB` / `Framed` | 90 kHz | PAT/PMT for tracks; PES for access units |
| MPEG-PS (VOB) | `AnnexB` / `Framed` | 90 kHz | shares the PES layer with TS |
| ADTS / MP3 / AC-3 | `Framed` | sample rate | containers that are just framing |

The intended order of work: move the video decoder onto `EsSink`
events, add ISOBMFF (which then reaches H.264 with no new glue), add
the PES layer with TS and PS on top, and extend the H.264 decoder
beyond Constrained Baseline — Main and High profile material is what
ISOBMFF and TS files actually carry, so decoder reach gates the value
of those parsers even though it does not gate their code.

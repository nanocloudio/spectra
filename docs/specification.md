# Spectra specification

## Product definition

Spectra is the shared home for portable audio, image, video, and
media-container algorithms in the nanocloud suite, packaged as modules
for the fluxor runtime. It lets Truffle, Grove, Zedex, and other
applications consume the same bounded codec modules without placing
application-level media code inside fluxor.

Spectra is a capability provider, not a media product, playback
service, session planner, emulator, storage system, or hardware
abstraction layer.

## Ownership

Spectra owns:

- portable demultiplexing and elementary-stream parsing;
- audio, image, and video codec algorithms;
- bounded sample, packet, frame, timestamp, and format descriptors
  needed by those algorithms;
- deterministic format conversion directly required at codec
  boundaries;
- codec capability declarations and target constraints; and
- `.fmod` packaging and OCI publication for Spectra modules.

Spectra does not own:

- the fluxor ABI, graph model, scheduler, channels, content-type
  registry, capability resolver, target descriptors, or
  platform-provider contracts;
- camera acquisition, display scanout, audio-device output, browser
  host shims, or board drivers;
- Truffle catalogues, media identity, playback intent, routing, or
  activity;
- Grove sessions, instruments, effects graphs, mixing policy, or
  distributed performance;
- Zedex emulation cores or the production of emulator audio/video
  events;
- Loam objects, Quantum delivery, or Lattice state; or
- general rendering, perception, inference, or application workflow
  semantics.

## Fluxor contract

Fluxor is authoritative for the wire-stable media surfaces, including
`OctetStream`, `AudioSample`, `AudioEncoded`, `VideoEncoded`,
`VideoRaster`, `VideoDraw`, `VideoScanout`, `MediaMuxed`,
`EventTimelineAudio`, and `EventTimelineVideo`, plus their rate
classes and capability vocabulary. Codec identity is deliberately not
part of that vocabulary: encoded surfaces are generic (`AudioEncoded`
carries any audio codec's access units, `VideoEncoded` any video
codec's), and codec identity travels in-band, in the record stream's
`STREAM` record (`abi::contracts::encoded`), with the codecs a port can
carry declared as `[ports.facts]`. [`docs/reference/media-surfaces.md`](reference/media-surfaces.md)
tabulates Spectra's role per surface.

Spectra manifests consume those names without assigning replacement
numeric identifiers. A media-contract change begins in fluxor and is
adopted by Spectra only after compatibility validation.

Spectra modules are ordinary position-independent fluxor modules. They
use bounded channels, explicit backpressure, declared memory and
record limits, target capability matching, and normal graph
activation. They must not add a second runtime or plugin ABI.

## The codec module

Source: `modules/app/codec/mod.rs`, `modules/app/codec/manifest.toml`.

The unified `codec` module has three inputs, of which a graph wires
exactly one. A file's bytes are sniffed; a record stream names its codec:

```text
encoded: OctetStream     (a file: sniffed)
    -> WAV / MP3 / AAC-LC (ADTS)   -> audio:  AudioSample
    -> BMP / GIF / PNG / JPEG      -> pixels: VideoRaster
    -> Matroska + H.264 CBP        -> pixels: VideoRaster
audio_in: AudioEncoded   (records: STREAM names the codec)
    -> AAC (raw + ASC, or ADTS) / MP3 -> audio:  AudioSample
video_in: VideoEncoded
    -> H.264 (Annex B, or length-prefixed + avcC) -> pixels: VideoRaster
```

It declares two variants: `full` (every family) and `audio` (WAV, MP3,
AAC only — the `pixels` and `video_in` ports are omitted, so wiring
them in an image or video graph fails at config build, and the module
arena drops to 64 KiB). Declared targets are rp2350, bcm2712, and wasm;
each declared target is built by `fluxor modules build --all`.

Alongside it sit `g711` (the PCM ↔ G.711 bridge, µ-law or A-law, on the record stream) and the two BCM2712
HEVC bring-up modules, `hevc_probe` and `hevc_decode`, which drive the
SoC's hardware decode block directly. The bring-up modules answer
hardware questions; they are not deployable media decoders.

## Provider boundary

Portable algorithms may be shared with platform adapters, but platform
calls remain outside Spectra. For example, Matroska parsing belongs in
a reusable Spectra core while WebCodecs lifecycle, browser promises,
canvas presentation, and host shim calls remain fluxor WASM provider
responsibilities.

Hardware-accelerated decoders may satisfy Spectra codec capabilities
through fluxor providers. Provider selection and fallback are
deployment decisions and must not change the semantic media contract
silently.

## Correctness and bounds

Every codec declares:

- accepted containers, profiles, levels, channel layouts, pixel
  formats, and target families;
- maximum input record, frame, working-set, and output-write sizes;
- incremental parsing and end-of-stream behaviour;
- malformed-input and unsupported-feature errors;
- reset, seek, discontinuity, flush, and backpressure semantics; and
- its licensing provenance (see below).

Malformed or adversarial input must produce a bounded error, never
panic, overrun, allocate without a declared ceiling, or continue with
ambiguous state. A target declaration in a manifest is a claim that
the module builds and runs there, not an aspiration.

## Provenance and licensing

The repository licence is Apache-2.0. Each decoder's lineage, from its
source headers:

| Decoder | Lineage |
| --- | --- |
| H.264 (Constrained Baseline) | Mechanical Rust port of the h264bsd decoder (Apache-2.0), C names kept verbatim. |
| MP3 | f32 implementation mirroring minimp3 (CC0). |
| AAC-LC | Clean-room implementation from a written decoder specification; no decoder source consulted by its author. Tables generated from the specification's formulas and codebook rows by `tools/gen/aac_tables.py`. |
| WAV, BMP, GIF, PNG, JPEG | Implemented in-tree against the published format specifications; no external decoder lineage. |
| Matroska demux | Clean-room implementation from RFC 8794 (EBML) and the Matroska specification; no reference code consulted. |
| G.711 | Companding per ITU-T Recommendation G.711. |
| BCM2712 HEVC bring-up | In-tree register model and command-list assembler for the SoC's decode block; no third-party code. |

The AAC-LC decoder is held to a stricter standard than a licence
header, because every open-source AAC decoder is copyleft or carries
ISO reference-code terms. It was produced under a two-team clean-room
protocol: a specification of the decoding process was written and
reviewed by people who had read existing decoders and the standard,
and the decoder was written from that specification alone by an author
who had read neither. The specification, the review record and the
audit of third-party decoders live in the private planning repository
(`.context/clean_room/aac/`); the tree carries the outcome — a decoder
whose every constant cites a section of that specification, tables
generated from it by `tools/gen/aac_tables.py`, and two proofs:
`tests/harness/tests/aac_decode.rs` (conformance against ffmpeg) and
`tests/harness/tests/aac_table_provenance.rs` (every table recomputed
or re-walked from committed data).

## Consumers

- Truffle resolves media objects and playback intent into compatible
  Spectra decode capabilities.
- Grove incorporates decoded streams into portable sessions and DSP
  graphs.
- Zedex emits its native A/V event surfaces and may use Spectra for
  recording, streaming, or encoded asset paths.

Consumers depend on public fluxor media contracts and Spectra
capability/module identity, never Spectra-private state or codec
internals.

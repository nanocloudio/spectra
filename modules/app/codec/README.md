# codec Module

Unified essence decoder. Sniffs the encoded payload and dispatches at
runtime to one of four families — audio, image, video, or the container
that carries a video essence.

## Layout

Four peer families, one directory each, all with the same façade. Adding a
format is a file in the right directory; adding a family is a directory.

```
mod.rs           PIC entry points, format detection order, family dispatch
audio/           essence: sample-domain
  mod.rs           façade + sniffing + the single seam to the root
  wav.rs           PCM passthrough (header parse only)
  mp3.rs           MPEG-1 Layer III (f32 port of minimp3)
  aac/             a codec needing more than one file gets a directory
    mod.rs           AAC-LC decoder
    cos_lut.rs hcb.rs iq.rs kbd.rs pow.rs sine.rs   generated tables
image/           essence: still raster
  mod.rs           façade + the chassis all four formats share
  bmp.rs gif.rs jpeg.rs png.rs
  deflate.rs       RFC 1951 inflater (PNG's decompressor)
video/           essence: moving raster
  mod.rs           façade + the demux → decode → colour-convert pipeline
  h264/            Constrained Baseline decoder (h264bsd port), 14 files
container/       muxed streams, kept orthogonal to the codecs they carry
  mod.rs           façade + sniffing
  matroska.rs      mounted from modules/common (see below)
tests/
  harness.rs       hermetic module-test lane (`fluxor test`)
```

Two rules hold everywhere, which is the whole point of the shape:

1. **A format is one file; a format that needs more than one file is one
   directory.** `audio/aac/` and `video/h264/` are the two that qualify.
2. **A family's directory name is its namespace.** No file repeats it —
   `image/png.rs`, not `image/image_png.rs`; `video/h264/cavlc.rs`, not
   `video/h264/h264_cavlc.rs`.

### Why `container/` is a peer of `video/`, not inside it

A container and a codec are orthogonal. Matroska carries H.264, HEVC and
AAC; H.264 arrives in Matroska, MP4 and raw Annex B. Folding the two
together is how a tree grows an `mkv_h264.rs` beside an `mp4_h264.rs`
beside an `mkv_hevc.rs`, each a near-copy of the others. Keeping them
apart means a new container is one file in `container/` and a new video
codec is one directory in `video/`, with the pipeline that joins a chosen
pair living once, in `video/mod.rs`.

### Why `audio/` has no shared state type

`image/mod.rs` hands every format one `ImageState`, because the four image
formats differ only in how they turn accumulated bytes into a raster and
genuinely share the accumulator, the scaler and the output path. The audio
formats share nothing comparable — WAV is a header parse and a byte
passthrough; MP3 and AAC are streaming transform decoders with their own
frame machines, bit reservoirs and overlap state. The families are
symmetric in their **contract**, not forced to be identical inside.

## The family contract

Each family façade exposes the same five things, so `mod.rs` dispatches to
all of them the same way:

| | purpose |
| --- | --- |
| `detect(&[u8]) -> Option<Format>` | claim the stream, or don't |
| `<fam>_init(..)` | bind state to channels |
| `<fam>_feed_detect(..)` | replay the bytes detection consumed |
| `<fam>_step(..)` | one bounded unit of work per scheduler tick |
| `<fam>_is_done(..)` | stream finished |

`detect` returning `None` means **"not mine"**, never "not yet": the root
asks each family in turn and commits on the first claim, so a family that
cannot tell must not claim. Every magic is length-checked before it is
compared, for exactly that reason.

The root owns only the **order** of those questions — containers first (a
container's payload is itself an essence stream), then fixed-magic
essences, then sync-word ones. See `detect_format` in `mod.rs`.

## Surface contract

Output ports follow the canonical AV surface family:

- `audio` — `AudioSample` (decoded sample-domain audio)
- `pixels` — `VideoRaster` (decoded pixel-domain frames)

The input port is `OctetStream` because format detection happens inside
the module. A graph that only needs one output side leaves the other
unwired.

## Variants

`manifest.toml` declares two (RFC `module_variants`):

- **`full`** (default, ships as `codec.fmod`) — every family.
- **`audio`** (`codec-audio.fmod`) — `wav`+`mp3`+`aac` only. Drops the
  image and video code, their terms in the state union, and the large
  arena request: `module_arena_size()` falls from 47 MiB to 64 KiB, which
  is what makes the module loadable on rp2350 at all.

Wiring `codec.pixels` against the audio variant fails at config-build time
because the embedded manifest omits the port.

## Mounted, not copied

`container/matroska.rs` is `#[path]`-mounted from
`modules/common/mkv_demux.rs` — the shared source tree that publishes as
`spectra-common`, so the demuxer has one home rather than a copy per
consumer. `#[path]` rather than `include!` keeps the file byte-identical:
it carries its own inner `#![allow]` attributes, which rustc rejects when
macro-spliced into a `mod` body.

The SDK (`abi.rs`, `runtime.rs`, `params.rs`) is mounted the same way from
`target/fluxor/fluxor-abi/sdk/`, materialised by `fluxor sync`. It is
gitignored, so `make lint` / `make test` / `make bench` declare it as a
prerequisite; a bare `cargo` invocation in a freshly-cleaned tree will
fail to resolve `mod abi` until you have synced.

## Testing

Two lanes:

- **Hermetic** (`tests/harness.rs`, run by `fluxor test`) — the AAC table
  provenance proof, which recomputes the tables from their defining
  formulas and touches no syscall table.
- **Channel-driven** (`tests/harness/tests/`, run by `make test`) — drives
  the module against mock channels: dispatcher, WAV, image frames, MKV
  video, DEFLATE, and the bank↔codec file-cycling suite. Every host suite
  in the project lives there; it is the only test crate.

## Porting or validating a new sub-codec

Follow `docs/guides/codec_porting.md`. The short version:

1. Get a byte-exact upstream reference building locally with intermediate
   spectrum dumps.
2. Build a standalone Rust replica of the DSP under `/tmp/` driving
   `decode_frame()` from a `std` `main()` — same code, no Fluxor runtime.
3. Diff per-layer probes (post-huffman, post-stereo, post-IMDCT, overlap
   state) against the C reference until correlation is 1.0000.
4. Only then bring up the end-to-end pipeline against `ffmpeg` reference
   output.

## Provenance

Relocated from `fluxor/modules/app/codec` on 2026-07-26 (T2.2.1).
Per-area licence provenance is inventoried in
`.context/planning/codec-inventory.md` §2. WAV, the image decoders, and the
Matroska demuxer are original/clean-room; MP3 mirrors CC0 `minimp3`; the
H.264 decoder is an Apache-2.0 port of h264bsd.

> **Open question — AAC.** `audio/aac/mod.rs` describes itself as a port of
> **faad2**'s decode path, and faad2 is GPLv2. Five of the six table files
> are recomputed from their defining formulas by the provenance test and
> are clean; `hcb.rs` (99 KB of Huffman codebooks — tabulated, not
> computed) remains unresolved. This gates publication (T2.3.1), not
> relocation — see inventory §2.1.

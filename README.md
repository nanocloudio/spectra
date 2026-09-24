# Spectra

Spectra is the shared media capability project of the nanocloud suite:
portable audio, image, video, and container codecs, packaged as
position-independent `no_std` modules for the [fluxor](../fluxor/)
runtime. Applications such as Truffle, Grove, and Zedex wire Spectra's
modules into their graphs instead of embedding codec implementations of
their own.

```text
files and containers (OctetStream), or
encoded access units (AudioEncoded / VideoEncoded record streams)
        |
        v
Spectra demux and decode
        |
        +-- audio  -> AudioSample
        `-- pixels -> VideoRaster
```

Fluxor remains authoritative for the module ABI, content-type
identifiers, channels, scheduling, capability matching, target
descriptors, and the platform audio/display/camera adapters. Spectra
supplies modules that use those contracts.

## Quick start

```sh
fluxor modules build --all
```

builds every module for every silicon target it declares — `.fmod`
artefacts land under `target/fluxor/<target>/modules/`:

```text
Modules (rp2350/rp2350): built 3 of 3, up-to-date 0, skipped 0, failed 0
Modules (bcm2712/bcm2712): built 5 of 5, up-to-date 0, skipped 0, failed 0
Modules (wasm/wasm): built 2 of 2, up-to-date 0, skipped 0, failed 0
```

To see a decode running on a Linux host — one shell command, config on
stdin, decoded frames on disk — follow
[`docs/guides/running.md`](docs/guides/running.md).

## Setup

Spectra consumes fluxor through the local OCI store (`$FLUXOR_STORE`,
default `~/.local/share/fluxor/store`):

```sh
# one-time, per developer machine
git clone git@github.com:nanocloudio/fluxor.git ../fluxor
make -C ../fluxor install    # put the fluxor CLI launcher on PATH
make -C ../fluxor publish    # publish SDK, module palette, runtime into the store
```

Spectra's manifest also pins module artefacts published by wave;
publish wave into the same store from its own checkout before the
first build. `fluxor modules build` then materialises everything
`fluxor.lock` pins. To pick up newly published dependencies, run
`fluxor update` and commit the advanced lockfile. When iterating on
several repos at once, add them to `~/.fluxor/workspace.toml`;
workspace members resolve `:latest` automatically.

`make publish` publishes Spectra's own artefacts — the
`spectra-common` source tree and one module artefact per module —
into the store for downstream consumers.

## Modules

| Module | Targets | What it does |
| --- | --- | --- |
| `codec` | rp2350, bcm2712, wasm | Unified decoder: sniffs a file and decodes WAV, MP3, and AAC-LC audio to `AudioSample`; BMP, GIF, PNG, and JPEG stills to `VideoRaster`; and Matroska-contained H.264 Constrained Baseline video to `VideoRaster`. Also decodes AAC and MP3 record streams (`audio_in`) and H.264 record streams (`video_in`). |
| `g711` | rp2350, bcm2712 | Bidirectional PCM ↔ G.711 bridge (µ-law or A-law) for a two-party voice path, on the `AudioEncoded` record stream. |
| `hevc_probe` | bcm2712 | BCM2712 HEVC block bring-up: reads the hardware version register and exercises a DMA round-trip. |
| `hevc_decode` | bcm2712 | BCM2712 HEVC phase-1 execution: assembles a command list for an embedded intra slice and drives the entropy engine. |

The `codec` module builds in two variants: `full` (the default,
`codec.fmod`) and `audio` (`codec-audio.fmod`), which omits the image
and video paths and the `pixels` port and shrinks the module arena to
64 KiB, small enough for an RP2350. A graph selects a variant with
`variant: audio` on the module entry. The two HEVC modules are
hardware bring-up stages, not decoders a graph would deploy for media
playback.

## Repository layout

| Path | Contents |
| --- | --- |
| `modules/app/` | The PIC modules: `codec`, `g711`, `hevc_probe`, `hevc_decode`. `fluxor modules build` packs each into a `.fmod`, plus one per declared variant. |
| `modules/common/` | Shared format cores, mounted into modules by `#[path]` and published as the `spectra-common` source tree: the Matroska demuxer, the elementary-stream contract, the G.711 companding maths, and the HEVC command-list assembler and register model. |
| `tools/` | Host tooling: the AAC table generator (`tools/gen/`) and the HEVC bitstream and command-list host tools (`tools/hevc/`). |
| `docs/` | Reference documentation, indexed by [`docs/overview.md`](docs/overview.md). |
| `fluxor.toml` | Project manifest for the `fluxor` CLI: identity, dependencies, silicon targets. |
| `Makefile` | Thin alias layer over the `fluxor` CLI; `make help` lists the targets. |

There is no root crate and no `crates/` directory: `modules/common/`
is a source tree, not a package, and the `modules/**` trees are
`no_std` PIC objects built by `fluxor modules build`, which invokes
`rustc` directly against each `mod.rs`.

## Documentation

- [`docs/overview.md`](docs/overview.md) — index of the doc set.
- [`docs/specification.md`](docs/specification.md) — what Spectra owns,
  what it must never own, the Fluxor contract, and codec provenance.
- [`docs/guides/running.md`](docs/guides/running.md) — the validated
  Linux bring-up: one command, one decoded frame.
- [`docs/reference/media-surfaces.md`](docs/reference/media-surfaces.md)
  — the Fluxor-owned content types Spectra produces and consumes.
- [`docs/reference/elementary-stream.md`](docs/reference/elementary-stream.md)
  — the internal boundary between container parsers and decoders.

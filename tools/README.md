# Tools

A file is named for what it is, its directory for whose it is — the same
convention `modules/app/codec/image/png.rs` uses, and one shared with wave.

| Directory | Role |
| --- | --- |
| `gen/` | Code generators whose output is committed source |
| `hevc/` | Host-side driver tools for the BCM2712 HEVC bring-up |

The language split follows the work. Python holds the generators, because
they are numerical and their reference is a formula rather than a program. Rust
holds the HEVC tools, because they must compile the *same* `modules/common`
sources the bare-metal driver does — deriving a driver constant with a second
implementation would defeat the point of deriving it.

## `gen/`

`aac_tables.py` generates `modules/app/codec/audio/aac/tables.rs` — every
table the AAC-LC decoder uses — from the clean-room decoder specification
(`.context/clean_room/aac/spec/aac_lc_r01_released.md`): the standard's data
tables and Huffman codebooks are parsed from it, the KBD windows, cosine,
inverse-quantisation and power tables are computed from the formulas it
states. Every run self-checks (Kraft sums, prefix-freeness, band monotonicity,
the IMDCT decomposition against the direct definition) and writes
`aac_codebooks.txt`, the committed copy of the codebook rows that
`tests/harness/tests/aac_table_provenance.rs` checks the tries against, so
the check needs no access to `.context/`.

The corpus generators are deliberately NOT here — `fixtures/tools/` sits next to
the media it produces and the digest manifest it invalidates, which is where a
reader checking provenance will look.

## `hevc/`

One crate, `spectra-hevc-tools`, three binaries. Each mounts `modules/common`
by `#[path]` at its crate root, exactly as the PIC module does, so the constants
they derive are produced by the same parser the silicon path runs.

| Binary | Does |
| --- | --- |
| `precompute` | Derives a clip's driver constants and emits `modules/app/hevc_decode/clip.rs`. `--expect-certified` re-derives the silicon-certified 64x64 constants and fails on any drift |
| `cmdlist` | Decodes and prints the phase-1 command list the driver will submit, so a `P1_LIST_DONE` value read off the rig points at a specific command |
| `inspect` | Dumps parameter sets and slice headers, to decide what a clip asks of the hardware before turning it into constants |

`precompute` writes a **tracked source file**, which is why it is a binary and
not an example: `clip.rs` is compiled into the bare-metal driver, and
regenerating it must be reproducible byte-for-byte.

## Not here, deliberately

- **`fixtures/tools/`** — the corpus and vector generators, next to the media
  they produce.

## Still to build

- MP3 and AAC parity drivers that compare decoded PCM against ffmpeg by
  correlation. They would also produce the evidence the AAC licence question
  needs.
- An RGB565-to-yuv420p converter with pinned rounding, so the corpus
  `.framemd5` references can be consumed and H.264 parity stops resting on a
  single 64x48 clip.

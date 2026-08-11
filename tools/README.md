# Tools

Three roles, one directory each. A file is named for what it is, its directory
for whose it is — the same convention `modules/app/codec/image/png.rs` uses, and
the one `wave/tools/` is organised by.

| Directory | Role |
| --- | --- |
| `ci/` | Gates `fluxor ci` runs as `[ci.test]` scripts |
| `gen/` | Code generators whose output is committed source |
| `hevc/` | Host-side driver tools for the BCM2712 HEVC bring-up |

The language split follows the work. Shell orchestrates processes and greps
their output, which is what a CI gate is. Python holds the generators, because
they are numerical and their reference is a formula rather than a program. Rust
holds the HEVC tools, because they must compile the *same* `modules/common`
sources the bare-metal driver does — deriving a driver constant with a second
implementation would defeat the point of deriving it.

## `ci/`

| Script | Gate |
| --- | --- |
| `shadow_guard.sh` | Hard-fails when the shadow checkout is missing, so CI cannot run zero tests and report green (`../standards/test-tracking.md` §7) |
| `host_crates.sh` | fmt, clippy and build for the three host crates, which no `fluxor ci` phase reaches now that there is no root workspace |
| `pic_link_check.sh` | Compiles `modules/common` for `thumbv8m.main-none-eabi` and rejects `__aeabi` helpers that do not link on rp2350; also asserts the cores stay `no_std` and `unsafe`-free |

`make lint` runs `host_crates.sh` too, so the Makefile and CI cannot disagree
about what "linted" means.

## `gen/`

`aac_kbd_tables.py` regenerates `modules/app/codec/audio/aac/kbd.rs` from the
ISO 13818-7 KBD window formula. Run with no arguments it only *compares*,
reporting per-entry ULP differences against what is committed; `--write`
regenerates. It runs `rustfmt` on its output, because `fluxor ci` fmt-checks
`modules/**` directly and a generator that emits unformatted Rust breaks the
gate for whoever regenerates next rather than for whoever wrote the generator.

The corpus generators are deliberately NOT here — `fixtures/tools/` sits next to
the media it produces and the digest manifest it invalidates, which is where a
reader checking provenance will look.

## `hevc/`

One crate, `spectra-hevc-tools`, three binaries. Each mounts `modules/common`
by `#[path]` at its crate root, exactly as the PIC module does, so the constants
they derive are produced by the same parser the silicon path runs.

| Binary | Does |
| --- | --- |
| `precompute` | Derives a clip's driver constants and emits `modules/app/hevc_decode/clip.rs`. `--expect-certified` re-derives the silicon-certified 64x64 constants and fails on any drift — it runs on every `make test` |
| `cmdlist` | Decodes and prints the phase-1 command list the driver will submit, so a `P1_LIST_DONE` value read off the rig points at a specific command |
| `inspect` | Dumps parameter sets and slice headers, to decide what a clip asks of the hardware before turning it into constants |

`precompute` writes a **tracked source file**, which is why it is a binary and
not an example: `clip.rs` is compiled into the bare-metal driver, and
regenerating it must be reproducible byte-for-byte.

## Not here, deliberately

- **`benches/spectra-bench`** — the decode-throughput harness. It links
  `spectra-test-harness` for the mock syscall table and arena model, and that
  crate is shadow-tracked; a crate in the primary repo cannot depend on one a
  primary-only clone does not have. `standards/tests.md` §1 already treats
  `benches/**` as a shadow tier. Run it with `make bench`.

  Note the contrast with Wave, whose `tools/load/wave-bench` deliberately shares
  no code with Wave: that rule is about two protocol peers not agreeing via a
  shared bug. Spectra's benchmark measures Spectra's decoder, so it must link
  it — the independent oracle here is ffmpeg, used for correctness rather than
  throughput.

- **`fixtures/tools/`** — the corpus and vector generators, next to the media
  they produce.

## Still to build

- MP3 and AAC parity drivers. Fluxor's `examples/test_harness/diff_mp3.py` and
  `diff_aac.py` compare decoded PCM against ffmpeg by correlation; they were not
  relocated, and their absence is the largest hole in Spectra's coverage
  (`docs/testing/test-strategy.md` §3). They would also produce the evidence the
  AAC licence question needs.
- An RGB565-to-yuv420p converter with pinned rounding, so the corpus
  `.framemd5` references can be consumed and H.264 parity stops resting on a
  single 64x48 clip.

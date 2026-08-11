# Codec migration readiness

- [x] Existing Fluxor codec source, manifest, README, and licences are inventoried.
      See `.context/planning/codec-inventory.md` (T2.1.1, 2026-07-25). Two items
      escalated: AAC/faad2 licence provenance (§2.1, gates publication) and the
      Fluxor wasm `#[path]` import of `mkv_demux.rs` (§4.1, gates source deletion).
      **AAC provenance largely resolved 2026-07-27** — see below.

- [ ] AAC table provenance is established (gates T2.3.1, not relocation).
      **5 of 6 files resolved, 599 KB of 698 KB.** The question for a numeric
      table is not where it was copied from but whether it could have been
      anything else: a table wholly determined by a published formula has one
      correct value per entry and so carries no authored expression.
      `tests/harness/tests/aac_table_provenance.rs` recomputes them
      independently and proves it — `iq_table` is `i^(4/3)` to within 1 ulp
      across 8192 entries, `cos_lut` is `cos(i·2π/N)` to within 1 ulp across
      16384, and `sine`/`pow` are trivially formulaic.
      `aac_kbd_tables.rs` was the one with a "Extracted from
      /tmp/faad2-src/libfaad/kbd_win.h" header. It is now **regenerated** from
      the ISO 13818-7 KBD formula by `tools/gen/aac_kbd_tables.py`: 92% of the
      entries came out bit-identical to faad2's and the rest 1 ulp apart,
      which is what proved the values were formula-determined all along. Cost
      on decoded output, measured: 3 samples of 284672 differ by 1 LSB, 90 dB
      below full scale.
      **`aac_hcb_tables.rs` (99 KB) remains open and still gates T2.3.1.** The
      AAC Huffman codebooks are *tabulated* in ISO/IEC 13818-7 rather than
      derived from it, so unlike every other table they cannot be recomputed
      to prove independence. Resolving it needs the spec document, a
      clean-room reimplementation, or shipping AAC as a separately-licensed
      artefact. Separately, `audio/aac/mod.rs` describing itself as a port of
      faad2's decode path is a distinct question this does not touch.
- [x] Reference fixtures are small, redistributable, and digest-pinned.
      13 conformance fixtures relocated byte-identical to `fixtures/` (T2.2.2,
      2026-07-26), 8.9 MB total. Every one is self-generated — synthetic
      `testsrc2` video, `gen_spiral.py` images, a synth-rendered audio scale —
      so none carries a third-party licence; provenance is tabulated per file
      in `fixtures/README.md`. `DIGESTS.sha256` pins all 17 committed paths and
      `regen.sh --verify` checks them. The H.264 golden corpus was previously
      gitignored in Fluxor with no second history; it is now versioned.
      The derived DVD/Blu-ray/UHD benchmark corpus is deliberately NOT
      versioned — recipe and digests are, per the same file.
- [x] Current output and error behaviour are frozen before relocation.
      **T2.2.5 closed differentially** (2026-07-26). Rather than freeze golden
      output, `tests/harness/tests/codec_relocation_parity.rs` links Fluxor's
      original codec and Spectra's relocated one into one binary and drives
      both through identical input: 11 fixtures spanning WAV, MP3, AAC, BMP,
      PNG, JPEG, GIF and MKV/H.264, **19.0 MiB of decoded output compared
      byte-for-byte**, plus the dispatcher's own log so two builds cannot
      reach the same bytes by different routes.
      This beats a golden freeze because a golden cannot distinguish "the move
      preserved behaviour" from "the move changed behaviour and the golden was
      regenerated" — and it covers what the retired textual provenance check
      deliberately excluded (`mod.rs`, the relocated `mkv_demux.rs`,
      `Cargo.toml`), which is
      exactly where a change could hide.
      Verified to have teeth: mis-routing PNG to the JPEG decoder in Spectra's
      copy is caught immediately ("Fluxor 4147200 vs Spectra 0").
      The one intentional divergence, the arena constant, is pinned by its own
      test, and a companion test shows both builds failing *identically* at the
      old 16 MiB — which is what makes "the arena was the constraint, not a
      code change" a measurement rather than a claim.
      Still outstanding, and separate from this checkpoint: corpus-scale
      parity against ffmpeg at 1080p/4K (`docs/testing/test-strategy.md` §7).
- [x] Module name, ports, content types, parameters, and targets remain compatible.
      Frozen in `src/codec_baseline.rs` and enforced by
      `tests/codec_manifest_contract.rs` (T2.1.2, 2026-07-25). The manifest
      assertions activate automatically when the module lands under
      `modules/app/codec/`; verified against Fluxor's real manifest, and
      verified to fail on injected drift. All 9 now run against the relocated
      manifest and pass.
- [ ] Every declared target builds and has proportionate runtime validation.
      **T2.2.4, mostly done (2026-07-26).** Builds: `fluxor modules build
      --all` produces both variants for rp2350, bcm2712 and wasm — 6 of 6
      fmods, non-zero. Runtime, now actually executed rather than merely
      written:
      * **linux** — `examples/video_player/linux.yaml` decodes `sample.mkv`
        to 100 PPM frames through the live scheduler.
      * **wasm** — `tests/host/video_player_wasm.pw.py` run for the first
        time: all five checks pass in real Firefox (mkv/h264 activated,
        SPS/PPS decoded, canvas non-blank at sum=124144474, frames advancing).
      * **bcm2712** — `pi5_codec_audio_variant` deployed to the pi5 board over
        TFTP after a Kasa power-cycle. The `codec-audio` fmod loads and
        heartbeats on silicon (`[dec] hb fmt=0`), no panic, no fault, no arena
        exhaustion, `soc_temp_mC=61700`. The scenario still reports TimedOut
        because `[bank] path_count=0` — no `.mp3` is staged on the NVMe, and
        Fluxor's `pi5_stage_gallery` writes `/SPIRAL.JPG`, not the
        `/CMAJOR.MP3` its header claims. Loading is proven; decode on silicon
        is not, and needs an audio stager.
      * **rp2350** — build-only still, no execution test.
      Also added `.fluxor-rig.toml`, without which no rig scenario could run
      from this repo at all.
- [ ] Malformed input, EOF, reset, starvation, and backpressure remain bounded.
      **Partial.** Proven for the Matroska parser: every truncation offset,
      every single-byte corruption at three bit masks, illegal nesting, and
      noise all produce a bounded error and never panic
      (`tests/mkv_core_conformance.rs`). Reset is proven both at a boundary and
      mid-stream, and across a file switch (`bank_codec_cycling.rs`).
      Backpressure and arena bounds are exercised by `codec_image_frame.rs`.
      The audio and image decoders have no equivalent malformed-input sweep.
- [x] Matroska demux has one portable implementation shared by PIC and WASM paths.
      `mkv_demux.rs` moved unchanged to `modules/common/` (T2.2.3,
      2026-07-26). The PIC module mounts it by `#[path]`; the host crate
      exposes it for L0 tests. Spectra's half is done — repointing Fluxor's
      WASM adapter at the shared file is the Fluxor-side change and belongs to
      T2.3.2. Until that lands, Fluxor's copy must not be deleted (T2.3.4).
- [x] Platform shims, sinks, stacks, and drivers remain owned by Fluxor.
      Confirmed at inventory §4.2 and honoured in the move: the host and
      browser codec builtins stayed, `fluxor-mod-bank` is consumed under its
      Fluxor name through `deps/fluxor`, and `video_player/wasm-direct.yaml`
      was deliberately not relocated because it drives Fluxor's WebCodecs
      builtin rather than Spectra's decoder.
- [ ] Fluxor examples resolve the locked Spectra artefact without source duplication.
      Not started (T2.3.1 / T2.3.2). Note the live hazard: `fluxor sync`
      symlinks Fluxor's prebuilt `codec.fmod` and `fluxor modules build`
      overwrites it locally, so a rig or graph run that skipped the build
      silently exercises Fluxor's copy and passes. Digest-pinning closes this.
- [ ] Truffle, Grove, and Zedex consume public contracts rather than codec internals.
      Not started (T2.3.3).

## Beyond the checklist

Two findings from the relocation that no checklist row covers, recorded so they
are not lost:

- **The arena was sized from an estimate 2–3.5x below the real requirement**,
  which made everything above DVD raster undecodable. Measured with
  `spectra-bench --probe-arena` and raised to 48 MiB; all 1080p stills and all
  Blu-ray-raster video now decode. The remaining limits are honest ones:
  1080p H.264 is CPU-bound at 0.38–0.83x realtime, and 4K needs more memory
  than the graph's whole 96 MiB pool. See `docs/testing/perf-benchmarks.md`
  §7 and §9.
- **Two bugs in the relocated mock allocator** made a working decoder look
  broken and are fixed here with a regression suite; both belong upstream in
  Fluxor. See `tests/harness/tests/harness_heap_contract.rs`.

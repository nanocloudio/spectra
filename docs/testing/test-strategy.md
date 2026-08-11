# Spectra test strategy — codec suite

Covers the audio (WAV / MP3 / AAC-LC), image (BMP / GIF / PNG / JPEG), and
video (Matroska + H.264 Constrained Baseline) paths across host, browser, and
hardware-rig layers. Rig rules derive from `standards/rig.md`; test placement
from `standards/tests.md` and `standards/test-tracking.md`.

Written 2026-07-26, after the S2.2 relocation landed.

Kept in `docs/` rather than `.context/planning/` deliberately: `.context/` is
gitignored, and a strategy that describes what is and is not covered is exactly
the kind of document that must not live only on one machine. The same reasoning
`standards/test-tracking.md` §1 applies to test sources applies to this.

## 1. Baseline — what exists today

| Source | Count | Covers |
| --- | --- | --- |
| `tests/project_contract.rs` | 2 | Project identity and media-domain names |
| `tests/codec_manifest_contract.rs` | 9 | Frozen manifest surface (T2.1.2) vs the relocated `manifest.toml` |
| `tests/mkv_core_conformance.rs` | 11 | Matroska parser: fixture parse, chunk invariance, reset, bounded failure |
| `tests/harness/tests/codec_*.rs` | 12 | Dispatcher, WAV, image frame, MKV/H.264 — relocated from Fluxor |
| `tests/harness/tests/bank_codec_cycling.rs` | 4 | File-to-file cycling across a HUP boundary |
| `tests/harness/tests/harness_heap_contract.rs` | 6 | The mock allocator's own contract |
| `tests/host/video_player_wasm.pw.py` | 3 checks | Real-browser wasm decode |
| `tests/hardware/pi5_codec_*.toml` | 2 + 2 diag | Silicon: audio variant, image decode |

**44 host tests against ~57k lines of relocated codec.** That ratio is the
headline gap. The audio decoders are the weakest: MP3 and AAC have no
Spectra-side test at all beyond format detection, despite being 178 KB of
source and carrying the migration's one open licence question.

## 2. Six layers

Each layer proves something the one below it cannot. A format is "covered"
only when it has a row in every layer that applies to it.

| Layer | Runs | Proves | Cannot prove |
| --- | --- | --- | --- |
| **L0 codec** | `cargo test`, no I/O | Byte-exact parse against format vectors; malformed input is a bounded error | Anything stateful, scheduled, or concurrent |
| **L1 module** | `cargo test`, mock syscalls | Dispatch, port discipline, backpressure, arena limits, reset paths | Real timing, real memory pressure |
| **L2 graph** | `fluxor run --target linux` | Real runtime, real scheduling, real sinks (PPM capture) | Browser and bare-metal reality |
| **L3 parity** | host, ffmpeg as oracle | Decoded output matches an independent decoder | Our own bugs ffmpeg happens to share |
| **L4 load** | host, `spectra-bench` | Renders/sec, sustained streams, arena ceilings | Bare-metal thermal and scheduling reality |
| **L5 target** | Playwright (wasm), `fluxor rig test` (pi5) | It runs on the actual target | Fast iteration |

**Oracle independence, restated for a codec.** Wave's rule — the load driver
must share no code with the thing it drives — does not transfer directly,
because `spectra-bench` measures Spectra's decoder and therefore must link it.
What transfers is the *correctness* oracle: L3 compares against ffmpeg, which
shares nothing with us. A benchmark that ran fast and decoded garbage would be
caught there, not by the bench.

## 3. Format × layer matrix

Legend: ✅ done · 🔨 to build · ⚠️ partial · ⛔ blocked

| Format | L0 | L1 | L2 | L3 parity | L4 load | L5 target |
| --- | --- | --- | --- | --- | --- | --- |
| **WAV** | 🔨 header vectors | ✅ `codec_wav.rs` (byte-exact ramp) | 🔨 | ✅ passthrough is its own oracle | ✅ 757x realtime | ⚠️ variant only |
| **MP3** | 🔨 | ⚠️ detection only | 🔨 | 🔨 `diff_mp3.py` not relocated | ✅ 361x | ✅ `pi5_codec_audio_variant` |
| **AAC-LC** | 🔨 | ⚠️ detection only | 🔨 | 🔨 `diff_aac.py` not relocated | ✅ 97x | 🔨 |
| **BMP** | 🔨 | ✅ `codec_image_frame.rs` | 🔨 | 🔨 | ✅ to 1080p | ✅ `pi5_codec_image_viewer` |
| **PNG** | 🔨 | ⚠️ shares the image path | 🔨 | 🔨 | ⚠️ DVD only — see §6 | ⚠️ |
| **JPEG** | 🔨 | ⚠️ shares the image path | 🔨 | 🔨 | ⚠️ DVD only — see §6 | ⚠️ |
| **GIF** | 🔨 | ⚠️ shares the image path | 🔨 | 🔨 | ⚠️ DVD only — see §6 | ⚠️ |
| **Matroska** | ✅ 11 tests | ✅ `codec_mkv_video.rs` | 🔨 | ✅ via `tiny.yuv` | ✅ all tiers | ✅ wasm Playwright |
| **H.264 CBP** | 🔨 bitstream vectors | ✅ byte-exact vs `tiny.yuv` | 🔨 `video_player/linux.yaml` | ⚠️ one 64x48 clip only | ✅ DVD→UHD | ✅ wasm Playwright |

The two largest gaps: **MP3 and AAC have no decode test on the Spectra side**
(the parity scripts `diff_mp3.py` / `diff_aac.py` stayed in Fluxor), and
**H.264 parity rests on a single 64x48 10-frame clip** — the corpus now has
1080p and 4K material but nothing consumes its `.framemd5` references yet.

## 4. Rig suite (per `standards/rig.md`)

`tests/hardware/`, run with `fluxor rig test --scenario <f>.toml`. Board `pi5`
/ rig `pi5-a`, netboot TFTP at `/srv/tftp/fluxor`.

| Scenario | Graph | Pass signal |
| --- | --- | --- |
| `pi5_codec_audio_variant.toml` | `nvme → fat32 → bank → codec(variant: audio)` | Recurring `[dec] hb fmt=2` |
| `pi5_codec_image_viewer.toml` | `nvme → fat32 → bank → codec → ws/http` | `[img] decoded` |
| `*_diag.toml` twins | identical graphs | Nothing — observes the full window |

### Rules encoded from `rig.md`

1. **Diag twin per scenario** (§5). The pass rules above match within a few
   lines of the monitor attaching, ending the capture almost immediately. The
   twins use a regex that matches nothing so the whole `timeout_s` is observed
   and `MON_HEAVY_STEP` / `MON_FAULT` / `[therm]` land in the log.
2. **Pass rules key on recurring steady-state lines, never boot banners** (§5).
   Both scenarios already do; `[dec] hb fmt=2` was chosen over the one-shot
   `[dec] mp3` for exactly this reason.
3. **Verify the staged fmod is Spectra's** (§4). `fluxor sync` symlinks
   Fluxor's prebuilt `codec.fmod`; `fluxor modules build` overwrites it. A run
   that skipped the build tests Fluxor's copy and passes. Both scenarios carry
   this warning; digest-pinning at T2.3.1 closes it properly.
4. **Pre-flight the 0-byte fmod check** (§4). Not hypothetical: 50 of Fluxor's
   `bcm2712` fmods were 0 bytes on 2026-07-26 and blocked every graph run until
   rebuilt. `stat -c '%s %n' target/fluxor/<target>/modules/*.fmod`.
5. **Single-domain only** (§7). No `execution.domains`, no `edge_class:
   cross_core` — the Pi 5 cross-core handoff stalls and then crashes the kernel.
6. **Linux proxy first** (§7). Every graph runs at L2 on `target: linux` before
   consuming a power-cycle.
7. **Cool boot, watch `soc_temp_mC`** (§6). The Pi 5 throttles ~2x hot. A 4K
   decode is a sustained thermal load, so this matters more here than for a
   protocol bench.
8. **Nothing on the rig is quotable without a same-board baseline** (§6). For
   decode that means ffmpeg on the SD-booted Pi 5, not a published figure.

### Not yet covered on silicon

No rig scenario exercises **video** (the codec module's `video/` family) or the **4K path**, and none
measures throughput — the two existing scenarios are functional proofs.
`observe.https_load` has no decode equivalent in `rig/vocab.rs`, so a rig
throughput number needs an upstream capability, the same blocker Wave hit.

## 5. wasm

`manifest.toml` declares `wasm`, and `docs/specification.md` requires that
every declared target have "an actual build and an appropriate execution or
cross-target conformance test". `fluxor modules build --all` proves it
compiles; `tests/host/video_player_wasm.pw.py` proves it runs — a real Firefox
against the real `fluxor.wasm`, asserting the container is detected, the canvas
goes non-blank, and successive frames differ.

Two prerequisites are not Spectra's to fix and are worth knowing before
debugging a failure:

- **The wasm kernel firmware is not a lockfile artefact.** `fluxor sync`
  resolves the SDK, the fmods and the linux runtime, but not
  `target/wasm/firmware.wasm`, so a downstream repo can build wasm modules and
  still not run a wasm graph. Link it out of `deps/fluxor` as a stopgap
  (`mkdir -p target/wasm && ln -sf ../../deps/fluxor/target/wasm/firmware.wasm
  target/wasm/firmware.wasm` — see `make help`).
- **The scenario's synthesised host graph needs Fluxor's own fmods**, which is
  where the 0-byte issue above bites.

## 6. Findings that came out of building this

Recorded here because each is a real property of the system, not a to-do:

1. **The arena was sized from an estimate that was 2–3.5x low, and it was the
   binding constraint above DVD raster.** Every 1080p still and Blu-ray-raster
   video tier was refused by the module itself against a 16 MiB
   `module_arena_size()`. `spectra-bench --probe-arena` measured the real
   requirement (24–44 MiB for 1080p video, up to 26 MiB for a 1080p still);
   the constant is now 48 MiB and all of those tiers decode. **The new binding
   limit is CPU, not memory:** 1080p H.264 runs at 0.38–0.83x realtime, so it
   decodes but cannot be sustained. 4K needs 82–116 MiB against a 96 MiB pool
   shared by the whole graph, so it is a Fluxor-side pool decision.
2. **`codec_image_frame.rs` names a stale 10 MiB arena** in a constant while
   the module now returns 48 MiB. Harmless to that test, which sets its own
   cap deliberately, but a trap for the next reader.
3. **Two mock-allocator bugs presented as decoder faults** — see
   `tests/harness/tests/harness_heap_contract.rs`. Both belong upstream.
4. **The video example referenced commercial films by symlink.** Dropped in
   relocation; the corpus tier replaces it.

## 7. Sequencing

**Phase 1 — audio decode coverage (largest gap, lowest risk).** Relocate
`diff_mp3.py` / `diff_aac.py` and the WS-capture harness from Fluxor's
`examples/test_harness/`, and turn the correlation method in
`docs/guides/codec_porting.md` into an L3 test against the corpus `.ref.pcm`
files. This also produces the evidence the AAC licence question needs.

**Phase 2 — corpus-scale H.264 parity.** Consume the `.framemd5` references.
Needs an RGB565→yuv420p conversion with pinned rounding; until then video
parity rests on one 64x48 clip.

**Phase 3 — L0 vectors for the formats that have none.** The image and audio
decoders are reachable as plain host code the same way `mkv_demux` is; the
malformed-input sweep in `mkv_core_conformance.rs` is the pattern to copy, and
`docs/specification.md`'s bounded-failure requirement applies to all of them.

**Phase 4 — L2 graph tests.** `video_player/linux.yaml` already captures PPMs;
script it and diff against the reference.

**Phase 4b — the elementary-stream boundary.** `docs/reference/elementary-stream.md`
and backlog E3. Every format on the target list depends on it, and the
differential parity suite is what makes refactoring a working decoder safe:
it compares against Fluxor's untouched original, so drift fails immediately
and names the fixture.

**Phase 5 — arena work.** Finding §6.1 is a product decision, not a bug: either
the module's arena grows, the image path streams instead of buffering whole
frames, or Spectra documents DVD raster as the supported still ceiling.
Measurements exist now to make that call.


## Rig scenarios prove what their pass rule asserts — and nothing else

Two findings from the 2026-08-10 silicon session, both of which presented as a
decoder bug and were neither.

### A green scenario can be green over a broken stage

`pi5_hevc_probe` passed for an unknown number of runs while the module's DMA
round-trip was failing. The module does two things — a version register read
and a DMA round-trip — and the pass rule matched only `ver=…ok=1`. When the
kernel split the `dma` permission out of `platform_raw`, every
`DMA_ALLOC_CONTIG` began being refused, the stage reported `ok=0`, and the
scenario kept reporting **Passed**.

`pi5_hevc_dma` is the rung that checks it and had simply not been run.

The scenario now carries a `[[fail]]` on `dma … ok=0`, verified in both
directions on hardware. The general rule it encodes: **a scenario that can
observe a downstream stage failing should refuse to certify the run**, even
when the narrow thing it was asked about is fine.

### An unobserved decode looks identical to a failed one

`pi5_codec_image_viewer` observes exactly one decode, and whether it observes
it is a race — see the header of that scenario for the full account. The short
version: with no browser attached there is no consumer for decoded frames, so
the codec decodes file 0, parks (measured: ~50,500 steps, ~5 s at
`tick_us: 100`), and everything it logged went into the boot ring before
`log_net` was draining. A large first file decodes late enough to be seen; a
small one does not.

A PNG-only gallery therefore produces no frame, no error and no heartbeat while
bank reports the whole file delivered — indistinguishable from "PNG is broken
on silicon", which it is not.

**Confirmed on silicon, 2026-08-11.** With the gallery reduced to one matching
file — a 6.2 MB PNG, every other image renamed to a non-matching extension —
the board reported `[dec] hb fmt=6` (6 = PNG) and
`[img] decoded 1920x1080 -> 960x540`, verdict Passed. The same scenario with a
1.2 MB PNG, and again with a 35 KB one, produced nothing at all. Size decides
observability; the decoder was never at fault. `fixtures/image/large_noise.png`
exists to make that first file large.

It improves the odds without fixing them: measured over six consecutive runs of
that gallery, five Passed and one TimedOut, the failure showing bank finished
and the codec frozen and silent — the decode landed inside the boot window that
time anyway. **A TimedOut on this scenario is not by itself evidence of a
decoder fault.** Attribution needs more than one run: three consecutive passes
are what distinguished "my change broke it" from "this scenario is flaky", and
on the first data point it looked exactly like a regression I had introduced.

**What to reach for before concluding a module is broken on hardware:**

| signal | tells you |
| --- | --- |
| `MON_HIST mod=N b0=…` | whether the module is still being stepped; a count frozen early means it parked |
| `MON_FAULT mod=N …` | whether it actually faulted — absence rules that out |
| `MON_FLOW_STALL edge=… from=… port=…` | which edge is backed up, and for how long |
| `[bank] hb … bytes=…` | delivery, independently of the consumer |

None of these depend on the module under test being able to speak, which is the
property that matters when the thing you are debugging is its silence.

### Corollary: the host suite is the faster instrument

Both cases were settled on the host in minutes after several rig runs each. The
1080p PNG decodes byte-for-byte in `tests/harness`, and
`a_starved_decode_reports_its_reason_through_the_heartbeat` shows the module
reporting `[img] png: full alloc fail` and replaying it on every heartbeat.
Reach for the rig to answer questions only silicon can answer — MMIO, DMA,
permissions, real timing — and for everything else prove it on the host first.

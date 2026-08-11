# Spectra decode benchmarks — method and measured numbers

Machinery: `benches/spectra-bench` over the corpus in `fixtures/bench/corpus.tsv`.
Rules inherited from `standards/rig.md` §6. First run 2026-07-26, immediately
after the S2.2 relocation.

> **Read §6 before quoting anything here.** These numbers were taken on a
> thermally saturated board and several tiers are limited by the module's arena
> rather than by the CPU. Both facts change what the figures mean.

## 1. Method

**Images are measured in renders per second, with a fresh decoder per render.**
A viewer decodes a whole still and presents it; there is no rate to sustain.
Reusing one warm decoder would drop the setup cost a real viewer pays on every
image.

**Audio and video are measured in sustained simultaneous streams.** The useful
capacity of a media codec is not frames-per-second in a tight loop, it is how
many streams run at once without any of them falling behind. The harness runs N
decoders concurrently — one thread each, own arena, own channels, released from
a common start gate — and reports the largest N at which **every** stream still
decodes at 1.0x realtime or better. The ramp doubles, so the reported figure is
the last level that held, and the first level that failed is reported next to
it as a caveat rather than averaged in.

**Aggregate throughput is not the answer.** A total that looks healthy while one
stream starves describes a system nobody can ship, which is why the gate is the
slowest stream and not the mean.

**Memory-bound and CPU-bound are distinguished, not assumed.** Each run records
the per-instance arena high-water and classifies its own verdict:

| Verdict | Meaning |
| --- | --- |
| `CPU_BOUND` | Streams fell below realtime with memory headroom left |
| `MEMORY_BOUND` | The declared ceiling was hit first — the stream count is a floor |
| `ARENA_LIMIT` | The **module** refused the work inside its own arena; a codec limit |
| `HARNESS_BOUND` | The driver could not keep the decoders fed — not quotable |

`ARENA_LIMIT` exists because a real capability limit and a broken benchmark are
different findings, and collapsing them into one verdict loses both.

**This is not a parity check.** Every tier must produce output, but the bytes
are not compared against a reference — see `benches/spectra-bench/src/lib.rs`
for what corpus-scale parity would still need. A green run means fast, not
correct.

## 2. Environment

| | |
|---|---|
| Host | Raspberry Pi 5 Model B Rev 1.1, 4 cores aarch64, 15 GiB, Linux 6.18 |
| Build | `cargo build --release` (LTO thin, codegen-units 1) |
| Module | `spectra-mod-codec`, `full` variant, host-rlib via `host-test` |
| Arena | 48 MiB per instance when these numbers were taken; now **47 MiB** — see §9 |
| Window | 6 s per measurement, ramp capped at 16 streams |
| Corpus | Big Buck Bunny (CC-BY 3.0) derivatives, 10 s clips from a 30 s offset |
| **SoC temp** | **77.4 °C at start, 82.9 °C at end** — see §5 |
| Raw | `--ndjson`, one JSON object per tier, appendable across runs |

All 26 tiers below come from a single run, so cross-tier comparisons are
internally consistent even though the absolute values are throttled.

## 3. Measured — images

`spectra-bench --kind image --duration 6`

| Tier | Raster | Format | Renders/s | Arena KiB | Verdict |
|---|---|---|---|---|---|
| `bmp_dvd` | 720x480 | BMP | **133.9** | 1 690 | CPU_BOUND |
| `gif_dvd` | 720x480 | GIF | **114.7** | 3 338 | CPU_BOUND |
| `jpeg_dvd` | 720x480 | JPEG | **91.6** | 2 800 | CPU_BOUND |
| `png_dvd` | 720x480 | PNG | **30.6** | 4 391 | CPU_BOUND |
| `bmp_bluray` | 1920x1080 | BMP | **17.9** | 10 132 | CPU_BOUND |
| `gif_bluray` | 1920x1080 | GIF | **14.8** | 19 818 | CPU_BOUND |
| `jpeg_bluray` | 1920x1080 | JPEG | **14.8** | 16 754 | CPU_BOUND |
| `png_bluray` | 1920x1080 | PNG | **4.4** | 25 735 | CPU_BOUND |
| `bmp_uhd` | 3840x2160 | BMP | **4.8** | 40 515 | CPU_BOUND |
| `jpeg_uhd` | 3840x2160 | JPEG | — | (65 MiB needed) | ARENA_LIMIT |
| `png_uhd` | 3840x2160 | PNG | — | (97 MiB needed) | ARENA_LIMIT |

Every 1080p still and 4K BMP decode as of the §9 arena change; they were all
`ARENA_LIMIT` before it. The two remaining refusals need more than the whole
graph's memory pool, not more of the codec's share.

Ordering is what you would expect from the work each format does: BMP is a
raster copy, GIF is LZW plus a palette expansion, JPEG pays an IDCT and chroma
upsample, PNG pays inflate plus per-scanline filter reconstruction and is ~4x
the cost of BMP at the same raster.

## 4. Measured — audio

`spectra-bench --kind audio --duration 6 --max-streams 16`

| Tier | Codec | Single-stream realtime | Streams held | Slowest at that level | Arena |
|---|---|---|---|---|---|
| `wav_44k_s16` | PCM | **747x** | 16+ | 96.5x | 0 |
| `mp3_128k` | MP3 | **369x** | 16+ | 47.8x | 0 |
| `mp3_320k` | MP3 | **315x** | 16+ | 43.1x | 0 |
| `aac_128k` | AAC-LC | **98x** | 16+ | 13.2x | 0 |
| `aac_192k` | AAC-LC | **89x** | 16+ | 13.1x | 0 |

**Every audio tier saturated the ramp cap, not the machine.** 16 concurrent
streams still ran at 12–98x realtime, so the real ceiling is far higher; these
rows say "at least 16", not "16". Raising `--max-streams` past ~4x the core
count stops being meaningful on a 4-core box, so the single-stream factor is the
figure to reason from.

**Audio allocates nothing from the arena** — 0 bytes high-water across all five
tiers. The decoders live entirely in the module's state union, which is exactly
why the `audio` variant can declare a 64 KiB arena and fit an RP2350.

AAC costs roughly 4x MP3 per stream, consistent with its filterbank and the
698 KB of lookup tables.

## 5. Measured — video

`spectra-bench --kind video --duration 6 --max-streams 16`

| Tier | Raster / cadence | Single-stream realtime | Streams held | Slowest at that level | Arena KiB | Verdict |
|---|---|---|---|---|---|---|
| `dvd_pal_base` | 720x576p25, 1 s GOP | **3.71x** | **8** | 1.14x | 6 146 | CPU_BOUND |
| `dvd_ntsc_base` | 720x480p30, 1 s GOP | **3.26x** | **4** | 1.05x | 5 363 | CPU_BOUND |
| `dvd_ntsc_allintra` | 720x480p30, intra only | **2.22x** | **4** | 1.02x | 10 678 | CPU_BOUND |
| `bluray_longgop` | 1920x1080p24, 10 s GOP | **0.83x** | **0** | — | 23 995 | CPU_BOUND |
| `bluray_base` | 1920x1080p24, 1 s GOP | **0.75x** | **0** | — | 26 725 | CPU_BOUND |
| `bluray_hibitrate` | 1920x1080p24, crf 18 | **0.71x** | **0** | — | 44 679 | CPU_BOUND |
| `bluray_60` | 1920x1080p60 | **0.38x** | **0** | — | 28 864 | CPU_BOUND |
| `uhd_base` / `_hibitrate` / `_60` | 3840x2160 | — | — | — | (82–116 MiB needed) | ARENA_LIMIT |

**All four Blu-ray tiers now decode** — they were `ARENA_LIMIT` before §9 — but
at **0.38–0.83x realtime**, so zero of them can be sustained. That is the
honest new limit: software H.264 Constrained Baseline at 1080p is not
real-time on one Pi 5 core. It is also why the wasm path exists — the browser
hands that work to a hardware decoder through WebCodecs.

The measured arena high-water reproduces the §9 probe exactly (24–45 MiB),
which is a useful cross-check that the probe measures what it claims to.

**The DVD numbers dropped ~25% against the pre-change run** (`dvd_ntsc_base`
was 4.42x / 8 streams, now 3.26x / 4). That is not the arena change — a larger
cap cannot slow allocation — it is thermal. See §6.

Both DVD rasters sustain 8 concurrent streams on 4 cores with the slowest still
above realtime; 16 dropped them to ~0.6x, so the true ceiling sits between 8
and 16. All-intra costs about twice the arena and holds half the streams — no
motion compensation, but a keyframe every frame means the full
transform-and-deblock path runs on every macroblock and nothing is skipped.

Nothing at 1080p or above decodes: `[mkv] decode memalloc error` and
`[mkv] es alloc failed`. At 1920x1080 the DPB reference frames plus the RGB565
output exceed the 16 MiB arena before decoding starts.

## 6. Caveats — read before quoting any number above

- **The board throttled, confirmed by hardware.** `vcgencmd get_throttled`
  returns `0xe0000` — the sticky bits for *soft temperature limit reached*,
  *ARM frequency capped*, and *throttling has occurred*. Temperature ran
  77.4 -> 82.9 °C during the run, after hours of ffmpeg encoding beforehand.
  This is not an inference from the numbers; the SoC is reporting it.
  `standards/rig.md` §6 puts pi5-class throttling at roughly 2x, so **every
  figure here is a depressed floor**, and the tiers measured late in the run are penalised more
  than the early ones. A cool-boot re-run is required before any of this is
  treated as a baseline. It was not done because the corpus build and the
  measurement shared one sitting.
- **Co-tenant harness.** The generator and the decoders share the same 4 cores.
  Anything else on the machine lands in the result.
- **Run-to-run spread is ~25% at this thermal state.** The DVD tiers moved
  from 4.42x to 3.26x between two runs with no code path in common changing.
  Treat differences below ~30% between runs here as noise, and re-baseline
  cool before trusting a regression.
- **Two runs, not one.** The image table came from a 3 s window and the video
  table from a 5 s window, after the §9 change. Cross-medium comparison is
  therefore indicative only; the §8 cool-boot re-baseline should be a single
  run.
- **Oversubscribed above 4 streams.** Every stream count past the core count is
  a throughput statement, not a latency one; per-stream latency at 16 streams
  on 4 cores is not representative of anything deployable.
- **Not a parity check.** See §1.
- **Host, not silicon.** No rig scenario measures decode throughput yet — the
  two pi5 scenarios are functional proofs, and there is no decode equivalent of
  `observe.https_load` in `rig/vocab.rs`. The quotable number is the one from a
  cool Pi 5 over a real link, and it does not exist yet.
- **No same-hardware baseline.** `rig.md` §6 requires comparing against a
  reference decoder on the same board — ffmpeg on this Pi — before calling any
  of this good or bad. Not yet done.

## 7. Findings

1. **The arena was the binding constraint above DVD raster, and it was set
   from an estimate that was 2–3.5x low.** Every 1080p still and every
   Blu-ray-raster video tier was refused by the codec itself
   (`png: full alloc fail`, `jpeg: coef alloc fail`,
   `[mkv] decode memalloc error`) against a 16 MiB `module_arena_size()` whose
   comment justified it as "≈ 12.5 MiB at 1080p". Measurement says 24–44 MiB
   for 1080p video and up to 26 MiB for a 1080p still. §9 fixes it; all of
   those tiers now decode.
2. **Bitrate drives the arena as hard as raster does.** `bluray_base` (crf 23)
   needs 27 MiB and `bluray_hibitrate` (crf 18) needs 44 MiB at the *same*
   1920x1080 raster, because the elementary-stream accumulator scales with
   coded frame size rather than pixel count. Any future arena sizing that
   reasons only about raster will under-provision high-bitrate input.
3. **1080p H.264 decodes but is not real-time on this host** — 0.38–0.83x
   across the four Blu-ray tiers, so zero sustained streams. This is now the
   binding limit, and it is a CPU limit rather than a memory one. It is the
   expected result for a software Constrained-Baseline decoder on one Pi 5
   core, and it is the reason the wasm path routes video to WebCodecs.
4. **4K is out of reach of this knob.** It needs 82–116 MiB against a
   `STATE_ARENA_SIZE` of 96 MiB that the *entire graph* shares. Serving 4K is
   a Fluxor-side pool decision (`modules/sdk/config.rs`), not a codec one.
   4K BMP (40 MiB) does now decode, at 4.8 renders/s.
5. **`codec_image_frame.rs` still names a stale 10 MiB arena** in a constant
   while the module returns 48 MiB. It does not affect the test's assertion
   (it sets its own cap deliberately) but it is a trap for the next reader.
6. **Two mock-allocator bugs made a working decoder look broken.** Both are
   fixed with a regression suite and both belong upstream — see
   `tests/harness/tests/harness_heap_contract.rs`. Before the fix, no image
   over 256 KiB could decode at all.

## 8. Not yet measured

| Gap | Why it matters |
| --- | --- |
| Cool-boot re-run | Everything here is a throttled floor |
| Same-board ffmpeg baseline | Without it no figure can be called good or bad |
| Rig throughput | Needs a decode-aware observe capability in Fluxor |
| The `audio` variant | Only `full` is measured; the 64 KiB RP2350 build is not |
| Audio ceiling | Every tier saturated the ramp cap, not the hardware |
| Corpus-scale parity | Fast is measured; correct is not |
| Soak | Longest run is 6 s per tier — leaks and drift invisible |
| Scaling behaviour | `scale_mode` 0 throughout; scaled output is unmeasured |

## 9. Arena sizing — how the numbers were chosen

`spectra-bench --probe-arena` binary-searches the smallest arena at which each
tier decodes, stopping at first output (every large allocation — DPB, output
raster, encoded accumulator — happens before the first frame emits, so a full
decode answers the same question at ~100x the cost).

Measured minimum arena, MiB:

| Raster | BMP | GIF | JPEG | PNG | H.264 |
| --- | --- | --- | --- | --- | --- |
| DVD 720x480 / 720x576 | 2 | 4 | 3 | 5 | 6–11 |
| 1080p | 10 | 20 | 17 | 26 | 24–44 |
| 4K | 40 | — | 65 | 97 | 82–116 |

`module_arena_size()` returns **47 MiB** for the `full`/`h264` variant,
**32 MiB** for image-only, and **64 KiB** for audio-only (measured 0 bytes
high-water — the audio decoders allocate nothing and run entirely out of the
state union, which is what lets that variant fit an RP2350).

### 47 MiB is a measured ceiling, not a preference

A real graph fails at instantiation:

```text
STATE ARENA EXHAUSTED — need=67108872 used=67191136 cap=100663296
```

`STATE_ARENA_SIZE` is 96 MiB, shared by every module in the graph and carved
eagerly. But the codec is nowhere near 96 MiB on its own, and the reason it
still exhausts is that **the loader charges the arena twice per load**.

That is measured, not inferred. With the constant at 24 MiB, a three-module
graph reports `state=50413928` — two 24 MiB arenas plus one 82 KB module
state — and loads 3 of 3, decoding 100 frames. At 48 and at 64 MiB, `used` is
always exactly one arena plus one state at the moment the second is requested.

| Arena | Charged | Loads? |
| --- | --- | --- |
| 24 MiB | 48.1 MiB | yes — 100 frames decoded |
| 47 MiB | 94.1 MiB | yes — 98% of pool, 100 frames, wasm 5/5 |
| 48 MiB | 96.4 MiB | no, by 0.4 MiB |
| 64 MiB | 128.1 MiB | no |

So the usable ceiling is `(96 MiB − state) / 2` = **47 MiB**, not 96, and the
constant sits exactly on it. At 47 the pool reads `98648424/100663296` — 98%
consumed by the codec alone, which is also why a graph carrying any other
arena-requesting module will not fit. 47 covers every measured 1080p tier
(worst case 44 MiB), so nothing is lost by stopping there.

Verified at 47 on all three runtimes: linux decodes 100 frames, the wasm
Playwright suite passes 5/5 in real Firefox, and the Pi 5 `audio` variant loads
and heartbeats on silicon.

Making 64 MiB work needs `STATE_ARENA_SIZE` raised to ~132 MiB minimum
(realistically 160) in `deps/fluxor/modules/sdk/config.rs` — a constant shared
by every project on the platform, so not a Spectra-side decision. 4K would need
~232 MiB of pool once doubled.

**The doubling looks like a Fluxor loader defect.** It halves every module's
usable arena, on every project, and nothing documents it. Worth raising
independently of the codec.


---

## Run: 2026-08-10, after the module restructure

`make bench` equivalent (`--duration 6 --max-streams 16`), cool machine, Pi 5,
4 cores, arena 47 MiB/instance. Taken after `crates/spectra-cores` moved to
`modules/common/**`, the codec split into audio/image/video/container families,
the DEFLATE sliding window was removed, and the five hand-rolled nearest-
neighbour scalers were unified on `codec/scale.rs`.

| tier | this run | table above | note |
| --- | --- | --- | --- |
| `dvd_ntsc_base` | **4.47x / 8 streams** | 3.26x / 4 | matches the 4.42x/8 pre-thermal figure §6 records |
| `bluray_base` | **0.86x** | 0.75x | see the caveat below |
| `wav_44k_s16` | **756x** | 747x | |
| `mp3_128k` | **372x** | 369x | |
| `aac_128k` | **99x** | 98x | |
| `png_dvd` | **30.7 renders/s**, arena 4356 KiB | 30.6, arena 4391 KiB | |
| `jpeg_dvd` | **92.4** | 91.6 | |
| `gif_dvd` | **106.5** | — | |
| `bmp_dvd` | **124–127** | 133.9 | reproducible; see below |

**Most of these deltas are NOT attributable to the code changes.** §6 records
that the table's video numbers came from a thermally degraded run, and the
audio/video figures here simply match the pre-degradation ones. A cool machine
explains them at least as well as anything in the diff, and the honest reading
is "no regression", not "an improvement".

Two figures ARE structural:

1. **`png_dvd` arena fell 4391 → 4356 KiB, a 35 KiB drop.** That is the 32 KiB
   DEFLATE sliding window removed from `image/deflate.rs`, plus the per-decode
   `xmap` table. It is a size, not a timing, so thermals cannot explain it.

2. **`bmp_dvd` is ~6% down (133.9 → 124–127), reproducible across three runs**,
   while PNG, JPEG and GIF sit at or above their recorded values. This is the
   scaler unification, and the asymmetry is the explanation: all four formats
   used to precompute a `dst_w`-entry column table per frame and index it per
   pixel; they now walk `scale::Nearest` (an add and a compare per pixel). For
   PNG/JPEG/GIF the decode work dominates and the change is invisible. BMP has
   almost no decode work — scaling IS its inner loop — so it feels the whole
   difference.

   Accepted deliberately, and now settled rather than open: the same change
   removed two software divisions per pixel from the video path (which has a
   realtime deadline that BMP does not), dropped four heap allocations per
   image decode, and replaced five hand-rolled copies of the mapping — two of
   which disagreed with each other — with one rule. BMP still renders a
   DVD-raster frame in 8 ms.

   The obvious way to claw the 6% back is to materialise `Nearest` into a
   per-row table inside the image chassis. That is deliberately NOT done: it
   reintroduces exactly the per-decode heap allocation the unification removed,
   on behalf of the least-used format, to speed up the one path with no
   deadline. Revisit only if BMP throughput becomes a real constraint — the
   mapping already has a single source of truth, so it would be a local change
   to the chassis rather than a return to five copies.

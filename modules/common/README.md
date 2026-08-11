# `modules/common` — shared format cores

Pure, `no_std`, I/O-free container and codec cores, free of `unsafe`. One home
per algorithm, mounted — never linked — by everything that needs one: Spectra's
`.fmod` modules, the test harness, and the driver tools.

This is a **source tree**, not a crate: it publishes as `spectra-common`
(`../../../standards/dependencies.md` §1, which names `modules/common/**` as the
downstream equivalent of Fluxor's SDK). Sibling projects do not define crates,
so the `spectra-cores` crate that used to hold these files is gone, along with
the root workspace it anchored.

Same shape as [`wave/modules/common`](../../../wave/modules/common/README.md),
with one deliberate difference: Wave's cores are `include!`d, so they may carry
no inner attributes and no test module. Spectra's are **`#[path]`-mounted**
instead, which keeps each file byte-identical across every build *and* lets a
core keep its own `#![allow(...)]`. The cost is that `include!` is not available
for a core that has inner attributes — see [Mount rules](#mount-rules).

## The `crate::` idiom

Cores reference each other as `crate::hevc`, `crate::bitreader`, and so on. That
looks like a crate dependency and is not one: every consumer mounts the cores it
needs **at its own crate root**, so `crate::` resolves to that root in each. In a
PIC module the crate root *is* the module root; in `tests/harness` and
`tools/hevc` it is their `lib.rs`. One file, three builds, no `cfg`.

Mount a core anywhere other than the root and its sibling paths break — which is
why `codec/container/mod.rs` can mount `mkv_demux` under `container::matroska`
(it has no `crate::` references) while `hevc.rs` must sit at the root of
`hevc_decode`.

## What is here

| Core | Owns | Mounted by |
| --- | --- | --- |
| `bitreader` | RBSP bit reader (emulation-prevention aware) shared by the H.264 and H.265 front ends | `hevc_decode` |
| `mkv_demux` | Clean-room incremental Matroska/EBML demuxer, sink-driven | `codec` (as `container::matroska`) |
| `mkv_es` | Matroska → elementary-stream adapter (T3.1.2) | host tests only |
| `es` | The elementary-stream boundary — the container/decoder seam (`EsSink`, Annex B framing) | host tests only |
| `g711` | ITU-T G.711 µ-law companding | `g711` |
| `hevc` | H.265 high-level syntax — VPS/SPS/PPS, slice headers, RPS, `pred_weight_table` | `hevc_decode` |
| `hevc_cabac` | CABAC context-variable initialisation (H.265 §9.3.2.2) | host tests only |
| `hevc_ctx_init` | CABAC context-initialisation array for the BCM2712 phase-1 engine | host tests only |
| `hevc_slice_msg` | Phase-1 slice-parameter message words (spec §7.3) | `hevc_decode` |
| `hevc_phase1` | Phase-1 (entropy) command-list assembly, incl. WPP | `hevc_decode` |
| `hevc_program` | Phase-2 launch programming (spec §10, §11) | `hevc_decode` |
| `hevc_bcm2712` | BCM2712 HEVC register-value encoding | `hevc_decode` |
| `hevc_detile` | Output surface: column-tiled NV12 → linear (nv12 / p010) | `hevc_decode` |
| `hevc_dpb` | Decoded-picture-buffer slot management (spec §9) | host tests only |
| `hevc_v4l2` | Parsed syntax → stateless-decode control structures | host tests only |

"host tests only" is a legitimate resting state here, unlike in Wave — a core
lands here as soon as it has host tests worth running, which is usually one rung
of the bring-up ladder before the bare-metal driver mounts it.
`es`/`mkv_es` are the exception: they are the *target* design of
`docs/reference/elementary-stream.md`, ahead of the pipeline that will use them.

## Mount rules

A module consumes a core in exactly one of two ways, and which one is not a
free choice:

- **`#[path]`** — the default, and required when the core carries inner
  attributes (`#![allow(...)]`). rustc rejects inner attributes when a file is
  macro-spliced into a `mod` body, so `include!` on such a core fails to
  compile. `es`, `hevc_bcm2712`, `hevc_v4l2` and `mkv_demux` carry them today.
- **`include!`** — only for a core with no inner attributes, when the mounting
  module needs the core's items in its own namespace rather than a submodule.

`tests/project_contract.rs` enforces this: a core with inner attributes must
never be reached by `include!`.

Cores must also stay free of `__aeabi` helpers that do not link on the 32-bit
PIC targets — a `u64` division or a float cast is enough.
`tools/ci/pic_link_check.sh` compiles this whole tree for
`thumbv8m.main-none-eabi` and greps the rlib, because a module build compiles
only the cores it happens to mount. That script's generated root also carries
the `#![no_std]` and `#![forbid(unsafe_code)]` the retired crate root used to
assert, so both properties are still enforced somewhere.

## Why this tree exists

Fluxor's WASM video adapter used to compile the Matroska demuxer straight out of
the codec module's private directory — a hard source coupling from a platform
provider into an application module, and the concrete blocker recorded as §4.1
of `.context/planning/codec-inventory.md`. The parser now has one home, and both
the PIC module and the host suites reach it from here.

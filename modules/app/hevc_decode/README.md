# `hevc_decode` — BCM2712 HEVC hardware decoder

The bare-metal driver for the BCM2712 (Pi 5 / CM5) HEVC decode block, written
clean-room against the released spec blocks. It allocates DMA buffers, stages a
clip's NALs, assembles the phase-1 command list, launches the entropy engine,
programs phase 2, and samples the reconstructed luma surface to prove the frame
is right.

bcm2712 only, `permissions = ["platform_raw"]` — every register access goes
through mediated `MMIO_READ32` / `MMIO_WRITE32`.

## The two-engine model

The block is stateless and split in two, which is why the driver is shaped the
way it is:

```
NALs → phase 1 (entropy/CABAC, command-list driven)
         → PU stream + coefficient stream (intermediate, in DMA)
       → phase 2 (reconstruction)
         → column-tiled NV12 output surface
```

Phase 1 is programmed by a *command list* the host builds, not by register
pokes; success is `P1_LIST_DONE == P1_LIST_COUNT`. On a mismatch the driver
reports `P1_CHECKPOINT`, because that is the only thing that separates "buffers
too small" (bit 3 coeff overflow, bit 4 PU overflow) from "bitstream
undecodable" (neither set) — spec §5, §6.4.

## Where the logic lives

Almost none of it is in this directory. Everything reusable is a core in
`modules/common`, `#[path]`-mounted so the host tests and the device build
compile identical bytes:

| Mounted core | Owns |
| --- | --- |
| `hevc` | H.265 high-level syntax — SPS/PPS, slice headers, RPS, weights |
| `bitreader` | RBSP bit reader |
| `hevc_bcm2712` | register-value encoding |
| `hevc_phase1` | phase-1 command-list assembly (incl. WPP) |
| `hevc_slice_msg` | §7.3 slice-parameter message words |
| `hevc_program` | phase-2 launch programming (§10, §11) |
| `hevc_detile` | column-tiled NV12 → linear |

`clip.rs` is **generated**, by
`cargo run --bin precompute -- <clip.hevc>` from `tools/hevc`. It carries
the geometry, sequence registers, per-frame constants and NAL bytes derived from
a clip's own bitstream by the same parser the decode path uses. Regenerate it;
do not hand-edit it.

> **PIC relocation hazard.** `clip.rs` exposes its slices through accessor
> **functions** (`nal(i)`, `ctx(i)`, `msgs(i)`, `ref_slots(i)`), not fields. A
> `&'static [T]` stored as a *field* of a `static` array does not relocate in a
> PIC module: the length is right (it is inline in the fat pointer) and the
> address is garbage. `&STATIC` taken inside a *function* is PC-relative and
> correct. This cost six rig runs to find; keep the accessors.

## Ports

| Port | Direction | Content | Carries |
| --- | --- | --- | --- |
| `probe_out` | out | `OctetStream` | unused — the result is the telemetry line |

Observability-exempt: the module runs one decode attempt and logs the outcome;
it emits no sustained byte stream.

## Verification

Certification is by rig ladder, one rung per capability, each pinned to a
byte-exact ffmpeg comparison in `tests/hardware/pi5_hevc_*.toml`:

| Rung | Proves |
| --- | --- |
| `probe` | the block's MMIO is reachable and clocked |
| `dma` | DMA alloc / flush / invalidate round-trips |
| `phase1` | the engine consumes a command list we built |
| `decode` | a single intra frame reconstructs correctly |
| `wpp` | wavefront parallel processing across 4×4 CTBs |
| `inter` | motion compensation, one L0 reference |
| `weighted` | explicit weighted prediction over a fade |

Every scenario pins `n=0x…` to the **literal** `BUILD_NONCE` in `mod.rs`, not a
pattern. A regex like `n=0x[0-9a-f]+` passes against a stale image, which is the
one failure a rig scenario must never have. Bump the literal in lockstep.

Sampling is one probe per CTB row in the *last* CTB column: row 0 needs no
context restore, so a probe there cannot discriminate a context bug. The P-frame
rungs sample the P frame's **own** DPB slot, which is untouched memory if it
never decoded.

## Not yet modelled

The precompute **refuses** these rather than guessing, each its own rung:
temporal MVP (collocated-MV buffers, §10, unmodelled) and multi-slice pictures.
B slices and multi-reference lists are unproven on silicon. Completion is a
fixed `P2_DWELL_STEPS` poll rather than an interrupt, and phases are not
pipelined (§11).

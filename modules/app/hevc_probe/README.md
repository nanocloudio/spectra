# `hevc_probe` — BCM2712 HEVC block bring-up probe

Rung 1 of the HEVC bring-up ladder, and deliberately the smallest thing that can
fail: it reads the decode block's hardware version register (spec r01/r02 §2,
§6.1 — expected `0x202`) over the mediated `MMIO_READ32` syscall and logs it.

Reads only. No decode, no DMA, no interrupts.

One run answers two questions at once:

1. **Is the block's MMIO reachable from bare metal at all?** — a wrong or
   unmapped address returns garbage rather than `0x202`.
2. **Did the VPU firmware leave it clocked at handoff**, as it does the GEM NIC,
   or does the clock still need enabling? — an unclocked block reads as zeroes
   or hangs the access, and both are distinguishable from a live `0x202`.

Answering those *before* adding DMA is the whole point:
when rung 3 later reports a command list that did not advance, this rung has
already eliminated "the block was never powered" as the cause.

bcm2712 only, `permissions = ["platform_raw"]`, `timer_class = "agnostic"` —
it reads no clock.

## Ports

| Port | Direction | Content | Carries |
| --- | --- | --- | --- |
| `probe_out` | out | `OctetStream` | unused — the result is the telemetry line |

Observability-exempt: one register read and one log line, no sustained stream.

## Verification

Like every rung of the ladder, the probe's pass rule pins the **literal**
`BUILD_NONCE` so a stale netboot image cannot satisfy it.

The rung it precedes is `hevc_dma`; the driver that eventually uses what both
prove is [`hevc_decode`](../hevc_decode/README.md).

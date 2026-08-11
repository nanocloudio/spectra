# `g711` — ITU-T G.711 µ-law codec

The bidirectional PCM ↔ µ-law bridge for a two-party voice path. Two
independent one-way flows share one module and one `module_step`, because they
are two directions of the same call rather than two modules that happen to be
adjacent:

- **encode** — `pcm_in` (stereo PCM) averaged to mono, companded, out on
  `ulaw_out`, feeding the RTP transmitter.
- **decode** — `ulaw_in` (µ-law from the jitter buffer's playout) expanded to
  linear samples, duplicated L=R, out on `pcm_out`, feeding playout.

Builds for rp2350 and bcm2712 — the same targets as Wave's `rtp` and `sip`,
which are what sit on the other side of both ports.

The companding math is **not here**: it is `modules/common/g711.rs`,
`#[path]`-mounted so the module and the host vector tests
(`tests/g711_vectors.rs`) compile the same bytes. This module is the channel
wrapper — polling, backpressure, and frame alignment — and nothing else.

## Ports

| Port | Direction | Content | Carries |
| --- | --- | --- | --- |
| `pcm_in` | in (0) | `AudioSample` | stereo PCM to encode |
| `ulaw_out` | out (0) | `OctetStream` | µ-law to the RTP transmitter |
| `ulaw_in` | in (1) | `OctetStream` | µ-law from playout |
| `pcm_out` | out (1) | `AudioSample` | stereo PCM to the speaker |

No parameters — sample rate and channel count are fixed by G.711 and by the
stereo PCM convention on the sample ports.

## Bounds

One step consumes at most 256 bytes of stereo PCM (64 frames → 64 µ-law bytes)
and at most 64 µ-law bytes (→ 256 bytes of stereo PCM). Both directions stage
into fixed buffers in module state and allocate nothing, so
`module_arena_size()` is untouched.

Each direction carries its own `pending_out` / `pending_offset` pair and
`drain_pending`s before reading, so a short `channel_write` never drops audio
and never lets the two directions interfere.

## Frame alignment

`channel_read` is a byte-stream read: it returns what is in the ring, not a
whole number of 4-byte stereo frames. Any 1–3 byte tail is **carried** to the
next step (`enc_carry_len`, parked at the front of `enc_in_buf`) rather than
dropped. Dropping it would shift every later frame in the stream by that many
bytes — swapping L/R and reading each sample across a frame boundary, silently,
for the remainder of the call. Producers today write whole frames so the tail is
normally zero, but nothing in the channel contract promises that and the failure
mode is noise rather than an error.

The decode direction needs no equivalent: µ-law is one byte per sample.

## Provenance

Relocated from `fluxor/modules/app/voip` (the encode and decode halves) under
Conclave plan S4.2, `Move G.711 to Spectra` — T4.2.2 module wrapping.

# `g711` — ITU-T G.711 codec (µ-law and A-law)

The bidirectional PCM ↔ PCMU/PCMA bridge for a two-party voice path. Two independent
one-way flows share one module and one `module_step`, because they are two
directions of the same call rather than two modules that happen to be adjacent:

- **encode** — `pcm_in` (stereo 8 kHz PCM) averaged to mono, companded under
  the `codec` parameter's law, and
  emitted on `encoded_out` as the fluxor encoded-media record stream
  (`abi::contracts::encoded`): one `STREAM` naming the law at 8 kHz mono, then one
  `UNIT` per `ptime` of audio, its `pts` counted in samples. Feeds Wave's `rtp`
  (`audio_in`).
- **decode** — `encoded_in`, a PCMU or PCMA record stream (Wave's
  `jitter.audio_out`; the `STREAM` record says which law),
  expanded to linear samples, duplicated L=R, out on `pcm_out`.

Builds for rp2350 and bcm2712 — the same targets as Wave's `rtp` and `sip`,
which are what sit on the other side of both encoded ports.

The companding math is **not here**: it is `modules/common/g711.rs`,
`#[path]`-mounted so every consumer compiles the same bytes. This module is the
stream wrapper — records, backpressure, frame alignment and concealment.

## Ports

| Port | Direction | Content | Carries |
| --- | --- | --- | --- |
| `pcm_in` | in (0) | `AudioSample` | stereo PCM to encode |
| `encoded_out` | out (0) | `AudioEncoded` | G.711 records; facts `codec = [pcmu, pcma]`, 8 kHz, mono, `max_payload = 320` |
| `encoded_in` | in (1) | `AudioEncoded` | G.711 records; same facts, `max_payload = 1460` (one RTP packet) |
| `pcm_out` | out (1) | `AudioSample` | stereo PCM to the speaker |

## Parameters

| Id | Name | Default | Meaning |
| --- | --- | --- | --- |
| 1 | `ptime` | 20 | Milliseconds per encoded unit: 10, 20, 30 or 40; anything else reads as 20 |
| 2 | `codec` | `pcmu` | The law encode uses: `pcmu` (µ-law) or `pcma` (A-law); anything else refuses construction |

## Decode rules

- The input stream must be PCMU or PCMA, raw, 8 kHz, mono, and each unit is
  expanded under the law its `STREAM` named. Any other `STREAM` is
  refused and counted, and its units are dropped until the next `STREAM`.
- A `DISCONTINUITY` unit whose `pts` jumps past the expected one is preceded by
  silence for the missing samples — concealment is the decoder's, and RFC 3551
  defines no PLC for G.711. A gap over one second is a stream that stopped and
  restarted, not a loss, and is not filled.
- A fragmented or `TRUNCATED` unit is not a G.711 access unit and is dropped.
- A record-stream fault (lost boundary, broken order) drops the carry and waits
  for a new `STREAM`.

## Bounds

The encoder reads no more PCM than completes the current unit and emits it the
step it fills, so its staging is one `STREAM` plus one 40 ms unit. The decoder
carries at most one record (`UNIT` header plus 1460 bytes) and stages one
decoded unit (5840 bytes of stereo PCM). Nothing is allocated, so
`module_arena_size()` is untouched. Each direction keeps its own
`pending_out` / `pending_offset` pair and drains before reading, so a short
`channel_write` never drops audio and never lets the two directions interfere.

## Frame alignment

`channel_read` is a byte-stream read: it returns what is in the ring, not a
whole number of 4-byte stereo frames. Any 1–3 byte tail is **carried** to the
next step (`enc_carry_len`, parked at the front of `enc_in_buf`) rather than
dropped. Dropping it would shift every later frame in the stream by that many
bytes — swapping L/R and reading each sample across a frame boundary, silently,
for the remainder of the call. The decode direction reads records, whose own
lengths delimit them; its carry is the record parser's.

## Provenance

Relocated from fluxor: the encode and decode halves of its former voip
module, rewrapped here as one bidirectional module.

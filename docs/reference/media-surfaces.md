# Media surface boundary

Fluxor owns the canonical content-type names and their on-wire
identifiers. Spectra consumes them: its manifests name these surfaces
on module ports (source: `modules/app/codec/manifest.toml`,
`modules/app/g711/manifest.toml`) and never assign replacement numeric
identifiers.

| Surface | Meaning | Spectra role |
| --- | --- | --- |
| `OctetStream` | Untyped encoded/container bytes | Sniff and parse only where the module contract permits |
| `AudioEncoded` | Codec-domain audio access units | Decode, encode, inspect, or convert |
| `AudioSample` | Decoded sample-domain audio | Produce or consume at codec boundaries |
| `VideoEncoded` | Codec-domain video access units | Decode, encode, inspect, or convert |
| `VideoRaster` | Pixel-domain frames | Produce decoded pixels |
| `MediaMuxed` | Deliberately combined container/timing stream | Mux or demux |
| `EventTimelineAudio` | Frame-aligned audio event timeline | Consume only through a declared adapter |
| `EventTimelineVideo` | Frame-aligned video event timeline | Consume only through a declared adapter |

Encoded surfaces are generic: `AudioEncoded` carries any audio codec's
access units and `VideoEncoded` any video codec's, never a per-codec
content type. Both carry fluxor's record stream
(`abi::contracts::encoded`): a `STREAM` record naming codec, packing,
clock rate, channels and configuration, then `UNIT` fragments, then
`END`. Spectra's ports declare the codecs they take in `[ports.facts]`
and check the `STREAM` record on receipt.

| Module | Port | Takes or emits |
| --- | --- | --- |
| `codec` | `audio_in` | AAC (raw with an AudioSpecificConfig, or ADTS), MP3 |
| `codec` | `video_in` | H.264 (Annex B, or length-prefixed with avcC) |
| `g711` | `encoded_in` / `encoded_out` | PCMU, raw, 8 kHz mono |

`VideoDraw` and `VideoScanout` belong to rendering and presentation
rather than codecs. Codec modules must not infer dimensions, sample
rate, channel layout, colour model, stride, time base, or ownership
where the public contract does not carry them; such metadata requires
an explicit fluxor-owned surface revision or a declared graph binding.

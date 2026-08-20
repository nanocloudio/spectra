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
access units and `VideoEncoded` any video codec's. Codec identity
travels in-band (encoded access units and container formats are
self-describing) or as a capability fact on the wiring edge, never as
a per-codec content type.

`VideoDraw` and `VideoScanout` belong to rendering and presentation
rather than codecs. Codec modules must not infer dimensions, sample
rate, channel layout, colour model, stride, time base, or ownership
where the public contract does not carry them; such metadata requires
an explicit fluxor-owned surface revision or a declared graph binding.

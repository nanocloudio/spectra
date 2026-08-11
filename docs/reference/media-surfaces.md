# Media surface boundary

Fluxor owns the canonical names and on-wire identifiers. Spectra consumes them.

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

`VideoDraw` and `VideoScanout` normally belong to rendering and presentation
rather than codecs. Codec modules must not infer dimensions, sample rate,
channel layout, colour model, stride, time base, or ownership where the public
contract does not carry them. Such metadata requires an explicit Fluxor-owned
surface revision or a declared graph binding.


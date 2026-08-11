# Spectra specification

## Product definition

Spectra is the shared Fluxor-native home for portable audio, image, video, and
media-container algorithms. It allows Truffle, Grove, Zedex, and other
applications to consume the same bounded codec modules without placing
application-level media code inside Fluxor.

Spectra is a capability provider, not a media product, playback service, session
planner, emulator, storage system, or hardware abstraction layer.

## Ownership

Spectra owns:

- portable demultiplexing and elementary-stream parsing;
- audio, image, and video codec algorithms;
- bounded sample, packet, frame, timestamp, and format descriptors needed by
  those algorithms;
- deterministic format conversion directly required at codec boundaries;
- codec capability declarations and target constraints;
- reference fixtures, conformance vectors, fuzzing, and parity thresholds; and
- signed `.fmod` packaging and OCI publication for Spectra modules.

Spectra does not own:

- the Fluxor ABI, graph model, scheduler, channels, content-type registry,
  capability resolver, target descriptors, or platform-provider contracts;
- camera acquisition, display scanout, audio-device output, browser host shims,
  or board drivers;
- Truffle catalogues, media identity, playback intent, routing, or activity;
- Grove sessions, instruments, effects graphs, mixing policy, or distributed
  performance;
- Zedex emulation cores or the production of emulator audio/video events;
- Loam objects, Quantum delivery, or Lattice state; or
- general rendering, perception, inference, or application workflow semantics.

## Fluxor contract

Fluxor remains authoritative for the wire-stable media surfaces, including
`OctetStream`, `AudioSample`, `AudioEncoded`, `VideoEncoded`, `VideoRaster`,
`VideoDraw`, `VideoScanout`, `MediaMuxed`, `EventTimelineAudio`, and
`EventTimelineVideo`, plus their rate classes and capability vocabulary.

Spectra manifests consume those names without assigning replacement numeric
identifiers. A media-contract change begins in Fluxor and is adopted by Spectra
only after compatibility validation.

Spectra modules are ordinary position-independent Fluxor modules. They use
bounded channels, explicit backpressure, declared memory and record limits,
target capability matching, and normal graph activation. They must not add a
second runtime or plugin ABI.

## Initial module

The compatibility-preserving first module is `modules/app/codec`:

```text
encoded: OctetStream
    -> format sniff and bounded dispatch
    -> WAV / MP3 / AAC
    -> BMP / GIF / PNG / JPEG
    -> Matroska + H.264 constrained baseline

audio:  AudioSample
pixels: VideoRaster
```

Relocation initially preserves module name, manifest version, port names,
content types, parameters, hardware targets, backpressure behaviour, and output
bytes. Refactoring into narrower decoder modules is a later compatibility
decision backed by measurements; it is not part of extraction.

## Provider boundary

Portable algorithms may be shared with platform adapters, but platform calls
remain outside Spectra. For example, Matroska parsing belongs in a reusable
Spectra core while WebCodecs lifecycle, browser promises, canvas presentation,
and host shim calls remain Fluxor WASM provider responsibilities.

Hardware-accelerated decoders may satisfy Spectra codec capabilities through
Fluxor providers. Provider selection and fallback are deployment decisions and
must not change the semantic media contract silently.

## Correctness and bounds

Every codec declares:

- accepted containers, profiles, levels, channel layouts, pixel formats, and
  target families;
- maximum input record, frame, working-set, and output-write sizes;
- incremental parsing and end-of-stream behaviour;
- malformed-input and unsupported-feature errors;
- reset, seek, discontinuity, flush, and backpressure semantics; and
- reference decoder, fixtures, objective parity thresholds, and licensing
  provenance.

Malformed or adversarial input must produce a bounded error, never panic,
overrun, allocate without a declared ceiling, or continue with ambiguous state.
All target declarations require an actual build and an appropriate execution or
cross-target conformance test.

## Compatibility

The extraction must prove:

1. the old and new module manifests compile to compatible public surfaces;
2. representative encoded fixtures produce byte-identical output, or an
   explicitly reviewed tolerance where the existing contract is numeric;
3. graph validation and target resolution remain unchanged;
4. Linux, WASM, BCM2712, and RP2350 paths continue where currently declared;
5. malformed-input, reset, EOF, starvation, and backpressure tests remain
   bounded; and
6. Fluxor examples consume a published or locked Spectra module rather than a
   second copied implementation.

## Initial consumers

- Truffle resolves media objects and playback intent into compatible Spectra
  decode capabilities.
- Grove incorporates decoded streams into portable sessions and DSP graphs.
- Zedex emits its native A/V event surfaces and may use Spectra for recording,
  streaming, or encoded asset paths.

Consumers depend on public Fluxor media contracts and Spectra capability/module
identity, never Spectra-private state or codec internals.


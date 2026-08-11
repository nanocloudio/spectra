# Extracting the Fluxor media codecs

The extraction is a relocation with compatibility proof, not a rewrite.

## Source inventory

Move first:

- `fluxor/modules/app/codec/mod.rs`;
- WAV, MP3, and AAC codec sources and tables;
- image decoder and BMP/GIF/PNG/JPEG helpers;
- Matroska demux and H.264 glue; and
- the portable H.264 decoder directory.

Move the module manifest, host-test Cargo manifest, module README, licensed test
fixtures, and codec parity tooling with the implementation.

Review separately:

- `modules/app/voip/g711.rs`: extract a portable G.711 core only if Truffle,
  Grove, or Zedex needs it outside the VoIP application;
- codec-specific reference scripts and small fixtures under Fluxor examples;
  retain only stable, redistributable conformance assets.

Keep in Fluxor:

- the content-type and capability registries;
- module ABI and SDK;
- channel, scheduling, backpressure, and target mechanisms;
- Linux/WASM browser shims and hardware providers;
- audio and display stacks and sinks;
- acquisition drivers; and
- generic Fluxor examples that prove runtime composition.

Mixers, synthesis, instruments, and effects are Grove concerns rather than
Spectra codec concerns.

## Migration sequence

1. Freeze current manifest bytes, fixture outputs, target builds, malformed
   input behaviour, and memory/buffer ceilings.
2. Copy the module without renaming its public surface and make its host tests
   pass in Spectra.
3. Extract `mkv_demux` into a portable Spectra core usable by both the PIC codec
   and Fluxor's WASM video adapter. Remove the direct Fluxor path import.
4. Publish the module by immutable digest and add it to the relevant Fluxor
   lockfile/catalogue.
5. Switch Fluxor examples and Truffle/Grove/Zedex consumers to the published
   module.
6. Remove the old Fluxor implementation only after cross-repository conformance
   passes and no graph resolves two competing codec implementations.

## Non-goals during extraction

- No port, content-type, or module rename.
- No simultaneous codec rewrite.
- No new media session or catalogue abstraction.
- No move of platform shims or device drivers.
- No claim that `VideoRaster` is a self-describing frame format unless Fluxor's
  contract is explicitly extended to make it so.

After parity, narrower modules or encoded-packet envelopes may be proposed
through normal Fluxor contract evolution.


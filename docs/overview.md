# Spectra documentation

Every document in `docs/`, grouped by what a reader is trying to do.

## Start here

- [`../README.md`](../README.md) — what Spectra is, the module map,
  setup, and the quick start.
- [`guides/running.md`](guides/running.md) — the simplest validated
  bring-up: decode a still image on a Linux host with one command.
- [`specification.md`](specification.md) — what Spectra owns, what it
  must never own, the fluxor contract, correctness requirements, and
  each decoder's licensing provenance.

## Reference

- [`reference/media-surfaces.md`](reference/media-surfaces.md) — the
  fluxor-owned content types Spectra produces and consumes, and its
  role per surface.
- [`reference/elementary-stream.md`](reference/elementary-stream.md) —
  the internal boundary between container parsers and decoders: the
  `EsSink` contract, its three load-bearing decisions, and what is
  wired today.

## Guides

- [`guides/running.md`](guides/running.md) — Linux-host bring-up and
  smoke check.

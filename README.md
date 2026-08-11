# Spectra

Spectra is the shared Fluxor-native media capability project for portable audio,
image, and video formats. It owns bounded codec and container algorithms that
can be consumed by Truffle, Grove, Zedex, and other Fluxor applications without
making those projects depend on codec implementations embedded in Fluxor.

```text
encoded packets or containers
        |
        v
Spectra demux, decode, encode, and format conversion
        |
        +-- AudioSample / AudioEncoded
        +-- VideoRaster / VideoEncoded
        `-- MediaMuxed
```

Fluxor remains authoritative for the module ABI, content-type identifiers,
channels, scheduling, capability matching, target providers, and platform
audio/display/camera adapters. Spectra supplies modules that use those
contracts.

## Initial extraction

The first migration target is `fluxor/modules/app/codec`, including WAV, MP3,
AAC, images, Matroska demux, and the portable H.264 baseline decoder. Module
names, ports, content types, target declarations, and observable behaviour stay
compatible during relocation.

The browser and Linux codec shims remain Fluxor platform adapters. Their
portable codec or demux algorithms should depend on Spectra through an explicit
source/package boundary rather than importing a file from a Fluxor application
module.

See:

- `docs/specification.md` for ownership and guarantees;
- `docs/guides/fluxor-extraction.md` for the migration sequence;
- `docs/reference/media-surfaces.md` for contract boundaries; and
- `.context/backlog.md` for implementation intent.

## Layout

No root crate, and no `crates/` — a sibling project's shared source is a source
tree, not a package (`../standards/dependencies.md` §1). Same shape as `wave`.

```
modules/
  common/      shared format cores, `#[path]`-mounted; publishes as spectra-common
  app/         the PIC modules: codec, g711, hevc_decode, hevc_probe
tests/harness/ the one test crate — every host suite, and the mock channel/
               provider/syscall doubles the modules run against
tools/         ci/ gates, gen/ code generators, hevc/ driver binaries
benches/       spectra-bench
fixtures/      conformance corpus, digest-pinned; benchmark media is derived
```

Each of the three host crates stands alone with its own `[workspace]`; the
`modules/**` trees are `no_std` PIC objects built by `fluxor modules build`,
which invokes `rustc` directly against each `mod.rs`.

## Development

```bash
make build                        # stage deps + host crates + PIC modules
make test                         # host suites, module tests, and the CI scripts
make lint                         # hygiene (see the table below)
make ci                           # the full gate

fluxor modules build --target rp2350   # or bcm2712 / wasm
```

Every lifecycle target is exactly its `fluxor` verb, with no prerequisites
(`../standards/make.md` §3) — the CLI reads the project's shape rather than each
Makefile re-encoding it, and it stages what it needs, so there is no `fluxor
sync` step to remember.

Where the checks actually run, since the verbs do not map one-to-one onto them:

| | fmt + clippy, host crates | host suites | module sources |
| --- | --- | --- | --- |
| `make test` | yes (`[ci.test]` scripts) | yes (34) | — |
| `make lint` | — | — | hygiene only |
| `make ci` | yes | yes | fmt + clippy |

`make lint` is deliberately thin: `fluxor lint` is hygiene, and the fmt/clippy
gate is `tools/ci/host_crates.sh`, wired as a `[ci.test]` script so it runs
under both `fluxor test` and `fluxor ci`. Reach for `make test` or `make ci`
before pushing — `make lint` alone will not catch a formatting or clippy
failure. The module sources are fmt-checked and clippied directly by `fluxor ci`
phases 1.1/1.2, which is coverage that only exists because there is no root
workspace for them to hide behind.

`make help` delegates to `fluxor help --make`, which walks `tools/` for real
rather than reciting a hand-written list that goes stale. Two things it cannot
know are below.

### Benchmarks

```bash
make bench      # regen the derived media corpus, then benches/spectra-bench
```

The corpus is derived and never committed; its digests are
(`fixtures/README.md`). Numbers from a hot or loaded board are a floor, not a
prediction — start from a cool machine (`../standards/rig.md` §6). Recorded
runs live in `docs/testing/perf-benchmarks.md`.

### Shadow-tracked trees

`tests/`, `benches/`, `examples/` and `fixtures/` are versioned in a second,
local-only history and never reach this repo's remote
(`../standards/test-tracking.md` §1):

```bash
make shadow-status                      # git shadow status
make shadow-log                         # git shadow log --oneline -20

git shadow add -Af tests benches examples fixtures \
  ':(exclude)*target/*' ':(exclude)fixtures/media/*'
```

Staging NEW files needs `-f`, because this repo's `.gitignore` outranks the
shadow exclude — and it MUST keep the exclude pathspecs, or `-f` force-adds
every cargo blob under a `target/` and every derived clip under
`fixtures/media/`. That is not hypothetical: 2273 build artefacts were once
versioned that way, because the exclude said `*/target/` and matched only one
path component.

Shadow edits are invisible to `git status`, so read `git shadow status`
alongside it. A `git mv` out of a tracked path into one of these trees keeps the
file tracked — `.gitignore` only governs files that are not already tracked —
so the guard is `git ls-files tests benches examples fixtures`, which must be
empty.

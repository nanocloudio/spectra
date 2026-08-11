#!/usr/bin/env bash
# THE HOST CRATES' fmt/clippy/build, AS A CI GATE.
#
# Spectra has no root cargo workspace (retired with `crates/spectra-cores`; the
# cores now live in `modules/common` and are `#[path]`-mounted, not linked).
# That changes what `fluxor ci` runs, in two ways:
#
#   * Phases 1.1/1.2 see no root `Cargo.toml` and switch to fmt-checking and
#     clippying the PIC module sources directly — a GAIN, since `modules/**` was
#     previously linted by neither. But it leaves the remaining host crates
#     fmt/clippy'd by nothing.
#   * Phase 2 is omitted (no root manifest, no `[ci.cargo] host_tools_crate`),
#     and phase 4's built-in `cargo-test (harness)` covers `tests/harness` — so
#     the harness's suites do run, but `tools/hevc` and
#     `benches/spectra-bench` are built by nothing.
#
# This closes both holes. Without it CI would summarise GREEN with the driver
# tools unbuilt and no host crate linted (standards/make.md §5 — the loam trap).
# Modelled on `wave/tools/ci/host_crates.sh`, which exists for the same reason.
#
# `[ci.test] scripts` in fluxor.toml is what makes this run.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

# `tests/harness` — lint only. Its suites are phase 4's job (`cargo-test
# (harness)`, built into fluxor ci); running them here would double the slowest
# phase of every CI run.
# The other two — lint AND build: nothing else compiles them at all.
CRATES=("tests/harness" "tools/hevc" "benches/spectra-bench")
BUILD_CRATES=("tools/hevc" "benches/spectra-bench")

# `-D warnings` for all three. The harness used to be exempt: clippy compiles a
# `#[path]`-mounted source as part of the mounting crate, so linting it lints
# every module and core it mounts — including generated AAC tables and two
# deliberately mechanical DSP ports. That exemption is gone, because those are
# now silenced where they belong instead of wholesale here:
#
#   * the generated tables, at their `include!` sites in `audio/aac/mod.rs`;
#   * the minimp3 and faad2 ports, on their two `pub mod` declarations;
#   * upstream Fluxor sources, at the `fs_bank` mount in the harness — that
#     tree must never be edited from here, so the allow travels with the mount.
#
# Each carries a reason naming why. The point of the split is that a NEW warning
# in first-party code now fails the gate rather than hiding among thousands of
# expected ones.

declare -A PKG=(
  ["tests/harness"]="spectra-test-harness"
  ["tools/hevc"]="spectra-hevc-tools"
  ["benches/spectra-bench"]="spectra-bench"
)

fail=0
for c in "${CRATES[@]}"; do
  dir="$ROOT/$c"
  if [ ! -f "$dir/Cargo.toml" ]; then
    echo "host_crates: MISSING $c/Cargo.toml"
    fail=1
    continue
  fi
  ( cd "$dir" && cargo fmt -p "${PKG[$c]}" -- --check ) || { echo "host_crates: fmt FAILED in $c"; fail=1; }
  ( cd "$dir" && cargo clippy --all-targets --all-features -- -D warnings ) \
    || { echo "host_crates: clippy FAILED in $c"; fail=1; }
done

for c in "${BUILD_CRATES[@]}"; do
  ( cd "$ROOT/$c" && cargo build --all-targets ) >/dev/null 2>&1 \
    || { echo "host_crates: build FAILED in $c"; fail=1; }
done

if [ "$fail" -ne 0 ]; then
  echo "host_crates: FAILED"
  exit 1
fi
echo "host_crates: ${#CRATES[@]} crate(s) linted, ${#BUILD_CRATES[@]} built"

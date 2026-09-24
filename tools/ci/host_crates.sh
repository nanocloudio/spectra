#!/usr/bin/env bash
# THE HOST CRATES' fmt/clippy/build, AS A CI GATE.
#
# Spectra has no root cargo workspace: the shared cores live in `modules/common`
# and are `#[path]`-mounted, not linked. With no root `Cargo.toml`, `fluxor ci`
# fmt-checks and clippies the PIC module sources directly, and its built-in
# `cargo-test (harness)` phase runs `tests/harness`'s suites — but nothing in
# it lints the host crates, and nothing builds `tools/hevc` or
# `benches/spectra-bench` at all. Without this script CI would summarise GREEN
# with the driver tools unbuilt and no host crate linted.
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

# `-D warnings` for all three. Clippy compiles a `#[path]`-mounted source as
# part of the mounting crate, so linting the harness lints every module and
# core it mounts. The two sources that cannot meet the lints are silenced
# where they live, never here:
#
#   * the minimp3 port, on its `pub mod` declaration (its value is being
#     diffable against the C original);
#   * upstream Fluxor sources, at the `fs_bank` mount in the harness — that
#     tree must never be edited from here, so the allow travels with the mount.
#
# Each carries a reason naming why, so a new warning in first-party code fails
# the gate rather than hiding among expected ones.

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

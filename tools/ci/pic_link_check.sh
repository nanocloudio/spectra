#!/usr/bin/env bash
# 32-bit PIC linkage guard for the shared cores. Wired as `[ci.test] scripts` in
# fluxor.toml (CI phase 3.5) so the canonical gate enforces it.
#
# On the 32-bit targets a `u64` division emits `__aeabi_uldivmod` and a float
# cast emits `__aeabi_d2uiz` / `__aeabi_f2uiz`. None of them link. This trap has
# been hit three times, and each time the ONLY thing that caught it was a full
# rp2350 module build — which does not cover `modules/common/**` at all, since a
# module mounts individual core files by `#[path]` and compiles the rest not at
# all. So check the cores directly.
#
# It also enforces two properties the deleted `spectra-cores` crate root used to
# assert and which had nowhere else to live: the cores are `no_std`, and they
# are free of `unsafe`. Both are in the generated root below.
#
# `rustc` directly rather than `cargo`: `modules/common` is a source tree, not a
# crate (`dependencies.md` §1 — it publishes as `spectra-common`), so there is
# no manifest to build. Compiling it the way a consumer does is also more
# faithful than a cargo profile would be.
#
# Verified to have teeth: replacing the 32-bit division in `mkv_es.rs`'s
# `timescale_hz` with the natural `u64` one makes `__aeabi_uldivmod` appear
# here, and reverting it removes it again.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT="$(pwd)"

TARGET=thumbv8m.main-none-eabi
OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

# One generated root mounting every core, in dependency-agnostic order — the
# cores reference each other as `crate::<core>`, which resolves precisely
# because they all sit at the crate root. This mirrors how a PIC module and the
# test harness mount them.
{
  echo '#![no_std]'
  echo '#![forbid(unsafe_code)]'
  echo '#![allow(dead_code, clippy::new_without_default)]'
  for f in "$ROOT"/modules/common/*.rs; do
    name="$(basename "$f" .rs)"
    echo "#[path = \"$f\"]"
    echo "pub mod $name;"
  done
} > "$OUT/cores.rs"

rustc --edition 2021 --crate-type rlib --target "$TARGET" \
      -o "$OUT/libspectra_common.rlib" "$OUT/cores.rs"

if { nm "$OUT/libspectra_common.rlib" 2>/dev/null || true; } \
    | grep -qE '__aeabi_(uldivmod|ldivmod|d2uiz|d2iz|f2uiz|f2iz)'; then
  echo "pic_link_check: FAILED — modules/common references a 32-bit helper"
  echo "that does not link on rp2350. Offending symbols:"
  { nm "$OUT/libspectra_common.rlib" 2>/dev/null || true; } \
    | grep -E '__aeabi_(uldivmod|ldivmod|d2uiz|d2iz|f2uiz|f2iz)' | sort -u
  echo "Narrow the operands to 32 bits before the operation."
  exit 1
fi
echo "pic_link_check: modules/common is free of unlinkable 32-bit helpers"

#!/usr/bin/env bash
# The canonical benchmark run — decode throughput for every corpus tier.
#
# A script rather than a Makefile recipe because `make bench` must be plain
# invocations in sequence (../standards/make.md §3), and this needs to change
# directory: `benches/spectra-bench` declares its own `[workspace]`, so it is
# built from inside itself.
#
# WHY THE BENCH IS NOT UNDER tools/ ITSELF. It links `spectra-test-harness` for
# the mock syscall table and arena model, and that crate is shadow-tracked — a
# crate in the primary repo cannot depend on one a primary-only clone does not
# have. `standards/tests.md` §1 already treats `benches/**` as a shadow tier.
# So the driver lives here, in the primary repo, and the crate it drives does
# not.
#
# The flags are the CI-comparable ones: changing them makes a run
# incomparable with the numbers recorded in .context/performance_baselines.md, so
# change them deliberately and re-baseline that document in the same commit.
#
# Conditions matter more than the flags. Numbers from a loaded or hot board are
# a floor, not a prediction (../standards/rig.md §6) — start from a cool machine
# with nothing else running, and regenerate the corpus first (`make bench` does,
# or `fixtures/regen.sh` by hand).
set -euo pipefail
cd "$(dirname "$0")/../benches/spectra-bench"

exec cargo run --release --bin spectra-bench -- \
  --duration 6 \
  --max-streams 16 \
  "$@"

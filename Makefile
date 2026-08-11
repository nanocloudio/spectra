# Spectra Makefile — the lifecycle, delegated.
#
# Every lifecycle target is exactly its `fluxor` verb, with no prerequisites
# (../standards/make.md §3). Not ceremony: per-repo recipe bodies are what
# drifted, so the CLI reads the project's shape instead of each Makefile
# re-encoding it. Staging is part of what the verb owns — this file used to
# carry an `$(SDK_ABI)` prerequisite that ran `fluxor sync` when the mounted SDK
# was missing, and the verb does that now.
#
# `bench`, `shadow-status` and `shadow-log` are not lifecycle targets; §3 allows
# plain invocations in sequence, which is what they are.
#
# Project workflow the CLI cannot know — shadow staging, benchmark conditions —
# lives in README.md rather than in a hand-written `help` block that would name
# the scripts existing when it was written and hide every one added since.

.PHONY: help build test lint ci publish clean bench shadow-status shadow-log

SHELL       := /bin/bash
.SHELLFLAGS := -euo pipefail -c

.DEFAULT_GOAL := build

help:
	@fluxor help --make

build:
	fluxor build

# Runs the host suites in `tests/harness` — `[ci.cargo] host_tools_crate` is
# what tells the CLI where they live — plus the module-test lane and the
# `[ci.test]` scripts, so `tools/ci/host_crates.sh` fmt/clippy/builds the three
# host crates here too.
test:
	fluxor test

lint:
	fluxor lint

ci:
	fluxor ci

publish:
	fluxor publish

clean:
	fluxor clean

# The corpus is derived and never committed (fixtures/README.md), so regenerate
# before measuring; `tools/bench.sh` holds the canonical CI-comparable flags.
# Numbers from a loaded or hot board are a floor, not a prediction
# (../standards/rig.md §6) — start from a cool machine with nothing else running.
bench:
	fixtures/regen.sh
	tools/bench.sh

# Shadow-tracked edits are invisible to `git status` on the primary, so read
# this alongside it. Staging NEW files needs `-f` plus the exclude pathspecs;
# see README.md.
shadow-status: ; git shadow status
shadow-log:    ; git shadow log --oneline -20

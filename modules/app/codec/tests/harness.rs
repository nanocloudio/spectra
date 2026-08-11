//! Module-core test harness for `codec` — declared by `manifest.toml
//! [test] harness` and run via `fluxor test` (standards/fluxor-modules.md
//! §0.2 lane 1).
//!
//! Relocated from `tests/harness/tests/aac_table_provenance.rs`
//! (registry_consolidation P7): the provenance proof is fully hermetic —
//! it `include!`s the AAC table files directly and recomputes them from
//! their defining formulas, touching no mock syscall table — so it
//! belongs in the hermetic module-test lane, tracked in git with the
//! tables it certifies. The channel-driven decode suites
//! (`codec_wav.rs`, `codec_image_frame.rs`, `codec_mkv_video.rs`,
//! `codec_dispatcher.rs`, `bank_codec_cycling.rs`) stay in
//! `tests/harness/`, which drives the module against mock channels.

#[cfg(test)]
mod aac_table_provenance;

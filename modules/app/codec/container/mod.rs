//! Container family — the muxed-stream formats, kept deliberately
//! separate from the essence codecs they carry.
//!
//! A container and a codec are orthogonal: Matroska carries H.264, HEVC,
//! AAC and much else, and H.264 arrives in Matroska, MP4 and raw Annex B
//! alike. Folding the two together is how a tree ends up with an
//! `mkv_h264.rs` beside an `mp4_h264.rs` beside an `mkv_hevc.rs`, each a
//! near-copy of the others. So the demuxers live here, the decoders live
//! under [`super::video`] / [`super::audio`], and the pipeline that joins
//! a chosen pair is the essence family's façade.
//!
//! Each container exposes a magic (for the root's [`detect`]) and an
//! incremental, `no_std` demuxer driven by a sink callback.
//!
//! One format today. MP4/ISO-BMFF and MPEG-TS are the obvious next two and
//! land as siblings of `matroska.rs` with no change to anything above.

/// Matroska / WebM (EBML).
///
/// Mounted by `#[path]` from `modules/common` rather than copied:
/// Fluxor's WASM video adapter parses the same container and must compile
/// the same source, not a second copy of it. `#[path]` rather than
/// `include!` keeps the file byte-identical — it carries its own inner
/// `#![allow]` attributes, which rustc rejects when macro-spliced into a
/// `mod` body.
#[allow(
    clippy::new_without_default,
    reason = "`MkvDemux::new()` is a `const fn` with no `Default` counterpart, and adding one would edit a core that the PIC build, the test harness and the driver tools all mount verbatim. The allow sits at each mount so the core stays a clean copy."
)]
#[cfg(feature = "host-test")]
#[path = "../../../common/mkv_demux.rs"]
pub mod matroska;
#[cfg(not(feature = "host-test"))]
#[path = "../../../common/mkv_demux.rs"]
pub(crate) mod matroska;

/// Containers this build can demux.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum Container {
    Matroska = 0,
}

/// Identify a container from the first bytes of a stream, or `None` when
/// this family does not claim them.
///
/// Same contract as [`super::image::detect`]: `None` means "not mine", not
/// "not yet" — so every magic is length-checked before it is compared.
pub fn detect(buf: &[u8]) -> Option<Container> {
    if buf.len() >= matroska::MKV_MAGIC.len() && buf[..4] == matroska::MKV_MAGIC {
        return Some(Container::Matroska);
    }
    None
}

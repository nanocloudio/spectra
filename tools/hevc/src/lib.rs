//! The shared format cores, mounted for the driver tools.
//!
//! Identical arrangement to `tests/harness/src/lib.rs`, and for the same
//! reason: the cores in `modules/common/**` reference each other as
//! `crate::hevc`, `crate::bitreader` and so on, which resolves only when they
//! are mounted at the CRATE ROOT. That one idiom is what lets a single file
//! compile unchanged in a PIC module (where the crate root is the module root),
//! in the test harness, and here.

#[path = "../../../modules/common/bitreader.rs"]
pub mod bitreader;
#[path = "../../../modules/common/hevc.rs"]
pub mod hevc;
#[path = "../../../modules/common/hevc_bcm2712.rs"]
pub mod hevc_bcm2712;
#[path = "../../../modules/common/hevc_cabac.rs"]
pub mod hevc_cabac;
#[path = "../../../modules/common/hevc_ctx_init.rs"]
pub mod hevc_ctx_init;
#[path = "../../../modules/common/hevc_detile.rs"]
pub mod hevc_detile;
#[path = "../../../modules/common/hevc_dpb.rs"]
pub mod hevc_dpb;
#[path = "../../../modules/common/hevc_phase1.rs"]
pub mod hevc_phase1;
#[path = "../../../modules/common/hevc_program.rs"]
pub mod hevc_program;
#[path = "../../../modules/common/hevc_slice_msg.rs"]
pub mod hevc_slice_msg;

//! HEVC decoded-picture-buffer slot management for the BCM2712 block (spec §9).
//!
//! The reconstruction engine has **16 reference-picture register sets** [r01
//! §9]. The bitstream, though, names references by picture order count (POC);
//! the front end ([`crate::hevc`]) derives a picture's reference sets as lists
//! of POCs. This module is the bridge: it tracks which decoded picture occupies
//! which hardware slot, maps a reference POC to its slot number for the slice
//! messages, and emits the 16 reference register sets for a launch.
//!
//! Two rules from the spec shape it:
//! - Slot assignment is software's to choose [r01 §9]; the natural and adopted
//!   policy is slot = DPB index. A reference's slot number is what the slice
//!   messages carry (descriptor bits 3:0, §7.3).
//! - **Every one of the 16 sets must hold a valid picture address** [r01 §9],
//!   so a corrupt stream cannot make the block fetch from an unprogrammed slot.
//!   Unused slots are therefore filled with some occupied picture, never left
//!   zero — [`Dpb::ref_registers`] enforces this.
//!
//! Eviction follows H.265 §8.3.2: after a picture, references not in the
//! current reference picture set are marked unused and their slots freed. That
//! marking is public-standard, driven by the POC lists the front end already
//! produces.
//!
//! `no_std`, allocation-free: a fixed 16-slot table.
//!
//! **Clean-room Team B.** The 16-slot model and the fill-unused rule are from
//! released spec r01 §9; the eviction semantics are public H.265. No GPL read.

use crate::hevc_program::RefPic;

/// Hardware reference-picture register sets [r01 §9].
pub const NUM_SLOTS: usize = 16;

/// One decoded picture resident in a slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DpbSlot {
    pub occupied: bool,
    pub poc: i32,
    /// Addresses already encoded as byte-address >> 6 [r01 §2, §9].
    pub luma_addr: u32,
    pub chroma_addr: u32,
    pub luma_stride: u32,
    pub chroma_stride: u32,
    /// Still needed as a reference by the current or a future picture.
    pub used_for_ref: bool,
    pub long_term: bool,
}

/// The 16-slot decoded-picture buffer.
#[derive(Clone, Debug)]
pub struct Dpb {
    slots: [DpbSlot; NUM_SLOTS],
}

impl Default for Dpb {
    fn default() -> Self {
        Self::new()
    }
}

impl Dpb {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            slots: [DpbSlot {
                occupied: false,
                poc: 0,
                luma_addr: 0,
                chroma_addr: 0,
                luma_stride: 0,
                chroma_stride: 0,
                used_for_ref: false,
                long_term: false,
            }; NUM_SLOTS],
        }
    }

    /// Slot currently holding `poc`, if any. This is the POC → slot lookup the
    /// slice-message assembly needs for each reference [r01 §7.3].
    #[must_use]
    pub fn slot_of_poc(&self, poc: i32) -> Option<u8> {
        self.slots
            .iter()
            .position(|s| s.occupied && s.poc == poc)
            .map(|i| i as u8)
    }

    /// Place a freshly decoded picture in the lowest free slot and return its
    /// slot number, or `None` if all 16 are occupied — which, after correct
    /// eviction, cannot happen for a conformant stream (its DPB fits in 16).
    pub fn allocate(&mut self, pic: DpbSlot) -> Option<u8> {
        let idx = self.slots.iter().position(|s| !s.occupied)?;
        self.slots[idx] = DpbSlot {
            occupied: true,
            ..pic
        };
        Some(idx as u8)
    }

    /// Mark every occupied slot whose POC is **not** in `keep` as no longer
    /// used for reference (H.265 §8.3.2 marking). `keep` is the union of the
    /// current picture's short- and long-term reference POCs — exactly what the
    /// front end's reference-set derivation yields.
    pub fn mark_unused_except(&mut self, keep: &[i32]) {
        for s in &mut self.slots {
            if s.occupied {
                s.used_for_ref = keep.contains(&s.poc);
            }
        }
    }

    /// Free every slot not used for reference. Call after `mark_unused_except`
    /// and after the current picture has itself been allocated (so it is not
    /// evicted before it can be referenced).
    pub fn evict_unused(&mut self) {
        for s in &mut self.slots {
            if s.occupied && !s.used_for_ref {
                *s = DpbSlot::default();
            }
        }
    }

    #[must_use]
    pub fn occupancy(&self) -> usize {
        self.slots.iter().filter(|s| s.occupied).count()
    }

    #[must_use]
    pub fn slot(&self, n: u8) -> &DpbSlot {
        &self.slots[n as usize]
    }

    /// Build the 16 reference register sets for a launch [r01 §9].
    ///
    /// Occupied slots contribute their real addresses. **Unused slots are
    /// filled with an occupied picture's addresses, never zeroed** — the spec
    /// requires every set to be a valid picture so a malformed reference index
    /// cannot fetch from nowhere. If the DPB is entirely empty (only possible
    /// before the first reference exists, i.e. at an IDR that references
    /// nothing) all sets are zero, which is harmless because such a picture
    /// issues no reference reads.
    #[must_use]
    pub fn ref_registers(&self) -> [RefPic; NUM_SLOTS] {
        let fill = self.slots.iter().find(|s| s.occupied);
        let make = |s: &DpbSlot| RefPic {
            luma_base: s.luma_addr,
            luma_stride: s.luma_stride,
            chroma_base: s.chroma_addr,
            chroma_stride: s.chroma_stride,
        };
        let fallback = fill.map(make).unwrap_or_default();

        let mut out = [RefPic::default(); NUM_SLOTS];
        for (o, s) in out.iter_mut().zip(self.slots.iter()) {
            *o = if s.occupied { make(s) } else { fallback };
        }
        out
    }
}

//! H.265 / HEVC high-level syntax.
//!
//! Parsing only — no reconstruction. That split is not an arbitrary
//! milestone: **every** decode backend needs exactly this and nothing more
//! from the bitstream layer.
//!
//! | Backend | Consumes |
//! | --- | --- |
//! | V4L2 stateless (`V4L2_CID_STATELESS_HEVC_SPS`, `_PPS`, `_SLICE_PARAMS`, …) | parsed headers + slice data |
//! | VAAPI, NVDEC, DXVA2 | the same, under different struct names |
//! | A bare-metal register driver | the same, fed to the block directly |
//! | A software decoder | the same, then reconstructs itself |
//!
//! So this module is the shared prerequisite for hardware decode *and* for a
//! portable software path, and it is worth building before choosing between
//! them. Nothing here touches a platform: it is bytes in, descriptions out,
//! which is what lets the same code run in a PIC module, a host test, and a
//! browser.
//!
//! Scope today: NAL layer, `profile_tier_level`, SPS, and the `hvcC`
//! configuration record that Matroska and ISOBMFF carry. PPS, slice headers,
//! reference picture sets and DPB management follow — they are what a
//! stateless backend additionally needs per picture.
//!
//! Written from ITU-T H.265 / ISO-IEC 23008-2. No decoder source consulted.

use crate::bitreader::RbspReader;

/// NAL unit types this module distinguishes. HEVC has 64; the ones named here
/// are those the front end must route on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NalType {
    /// Instantaneous decoder refresh — a clean random access point.
    IdrWRadl,
    IdrNLp,
    /// Clean random access.
    Cra,
    /// Broken link access.
    BlaWLp,
    BlaWRadl,
    BlaNLp,
    /// Non-IRAP coded slice.
    TrailN,
    TrailR,
    /// Parameter sets.
    Vps,
    Sps,
    Pps,
    /// Access unit delimiter, SEI, filler and the rest.
    Other(u8),
}

impl NalType {
    #[must_use]
    pub const fn from_u8(t: u8) -> Self {
        match t {
            0 => Self::TrailN,
            1 => Self::TrailR,
            16 => Self::BlaWLp,
            17 => Self::BlaWRadl,
            18 => Self::BlaNLp,
            19 => Self::IdrWRadl,
            20 => Self::IdrNLp,
            21 => Self::Cra,
            32 => Self::Vps,
            33 => Self::Sps,
            34 => Self::Pps,
            other => Self::Other(other),
        }
    }

    /// Intra random access point — a picture a decoder may start at.
    ///
    /// Seeking lands here, so a demuxer's Cues index is only useful if the
    /// pictures it points at satisfy this.
    #[must_use]
    pub const fn is_irap(self) -> bool {
        matches!(
            self,
            Self::IdrWRadl
                | Self::IdrNLp
                | Self::Cra
                | Self::BlaWLp
                | Self::BlaWRadl
                | Self::BlaNLp
        )
    }

    #[must_use]
    pub const fn is_parameter_set(self) -> bool {
        matches!(self, Self::Vps | Self::Sps | Self::Pps)
    }
}

/// The two-byte NAL unit header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NalHeader {
    pub nal_type: NalType,
    pub layer_id: u8,
    /// `nuh_temporal_id_plus1 - 1`.
    pub temporal_id: u8,
}

/// Parse the 2-byte NAL header from the start of a NAL unit.
///
/// Returns `None` when the payload is too short or `forbidden_zero_bit` is
/// set — the latter means we are not looking at a NAL at all, usually because
/// framing has desynchronised.
#[must_use]
pub fn parse_nal_header(nal: &[u8]) -> Option<NalHeader> {
    if nal.len() < 2 {
        return None;
    }
    if nal[0] & 0x80 != 0 {
        return None; // forbidden_zero_bit
    }
    let nal_type = (nal[0] >> 1) & 0x3F;
    let layer_id = ((nal[0] & 0x01) << 5) | (nal[1] >> 3);
    let tid_plus1 = nal[1] & 0x07;
    if tid_plus1 == 0 {
        return None; // reserved; nuh_temporal_id_plus1 must be >= 1
    }
    Some(NalHeader {
        nal_type: NalType::from_u8(nal_type),
        layer_id,
        temporal_id: tid_plus1 - 1,
    })
}

/// Profile and level, from `profile_tier_level`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ProfileTierLevel {
    pub profile_space: u8,
    /// False = Main tier, true = High tier.
    pub high_tier: bool,
    /// 1 = Main, 2 = Main 10, 3 = Main Still Picture, 4 = format range ext.
    pub profile_idc: u8,
    /// `general_level_idc`; level times 30 (level 5.1 -> 153).
    pub level_idc: u8,
    pub progressive_source: bool,
    pub interlaced_source: bool,
}

impl ProfileTierLevel {
    /// Level in the conventional decimal form — 153 becomes 5.1, as `(5, 1)`.
    #[must_use]
    pub const fn level(self) -> (u8, u8) {
        (self.level_idc / 30, (self.level_idc % 30) / 3)
    }
}

/// Sequence parameter set — the fields a backend or a caller needs.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Sps {
    pub sps_id: u8,
    pub vps_id: u8,
    pub ptl: ProfileTierLevel,
    /// 0 = monochrome, 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    pub chroma_format_idc: u8,
    pub separate_colour_plane: bool,
    /// Coded size, before the conformance window is applied.
    pub width: u32,
    pub height: u32,
    /// Display size, after cropping. This is what a viewer should see.
    pub cropped_width: u32,
    pub cropped_height: u32,
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_max_poc_lsb: u8,
    pub max_dec_pic_buffering: u8,
    pub max_num_reorder_pics: u8,
    /// `sps_max_sub_layers_minus1`. Retained for the hardware decode path,
    /// which needs it verbatim; the demux path never used it.
    pub max_sub_layers_minus1: u8,
    /// `sps_max_latency_increase_plus1` for the top sub-layer. Zero means
    /// "no limit signalled".
    pub max_latency_increase_plus1: u32,
    /// Coding tree block geometry. A backend sizes its per-CTB state from
    /// these, and tile/slice addressing is expressed in CTBs.
    pub log2_min_cb_size: u8,
    pub log2_ctb_size: u8,
    pub ctb_width: u32,
    pub ctb_height: u32,
    /// Luma transform block geometry. Read but discarded on the demux path;
    /// the decoder needs both to size transform units.
    pub log2_min_tb_size: u8,
    pub log2_diff_max_min_tb: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    /// PCM sample coding. Meaningful only when `pcm_enabled`; all zero
    /// otherwise. Rare in real content but part of the hardware input.
    pub pcm_bit_depth_luma: u8,
    pub pcm_bit_depth_chroma: u8,
    pub log2_min_pcm_cb_size: u8,
    pub log2_diff_max_min_pcm_cb_size: u8,
    pub pcm_loop_filter_disabled: bool,
    pub scaling_list_enabled: bool,
    pub amp_enabled: bool,
    pub sao_enabled: bool,
    pub pcm_enabled: bool,
    pub num_short_term_ref_pic_sets: u8,
    pub long_term_ref_pics_present: bool,
    pub num_long_term_ref_pics_sps: u8,
    pub temporal_mvp_enabled: bool,
    pub strong_intra_smoothing_enabled: bool,
    /// Colour signalling. `present` is false when the stream says nothing,
    /// in which case a consumer must apply its own default rather than
    /// assume BT.709 here.
    pub colour: ColourInfo,
}

impl Sps {
    /// True when the stream needs more than 8 bits per sample.
    ///
    /// The decisive question for the output path: UHD Blu-ray is Main 10, and
    /// an RGB565 raster cannot represent it.
    #[must_use]
    pub const fn is_high_bit_depth(self) -> bool {
        self.bit_depth_luma > 8 || self.bit_depth_chroma > 8
    }
}

/// Chroma subsampling divisors, `(SubWidthC, SubHeightC)`.
const fn sub_wh(chroma_format_idc: u8, separate_planes: bool) -> (u32, u32) {
    if separate_planes {
        return (1, 1);
    }
    match chroma_format_idc {
        1 => (2, 2), // 4:2:0
        2 => (2, 1), // 4:2:2
        _ => (1, 1), // monochrome or 4:4:4
    }
}

/// Parse `profile_tier_level`.
///
/// The shape is awkward and worth spelling out, because an off-by-one here
/// desynchronises the whole SPS and yields a plausible but wrong resolution:
/// a fixed 88-bit general block, then one presence-flag pair per sub-layer,
/// then padding to a byte boundary when there is at least one sub-layer, then
/// a variable block per sub-layer.
fn parse_ptl(r: &mut RbspReader<'_>, max_sub_layers_minus1: u8) -> Option<ProfileTierLevel> {
    let profile_space = r.u(2)? as u8;
    let high_tier = r.flag()?;
    let profile_idc = r.u(5)? as u8;
    // general_profile_compatibility_flag[32]
    r.skip(32)?;
    let progressive_source = r.flag()?;
    let interlaced_source = r.flag()?;
    let _non_packed = r.flag()?;
    let _frame_only = r.flag()?;
    // 43 reserved bits, then general_inbld_flag / reserved bit.
    r.skip(43)?;
    r.skip(1)?;
    let level_idc = r.u(8)? as u8;

    let n = max_sub_layers_minus1 as usize;
    let mut profile_present = [false; 8];
    let mut level_present = [false; 8];
    for i in 0..n {
        profile_present[i] = r.flag()?;
        level_present[i] = r.flag()?;
    }
    if n > 0 {
        // reserved_zero_2bits padding out to 8 sub-layers.
        for _ in n..8 {
            r.skip(2)?;
        }
    }
    for i in 0..n {
        if profile_present[i] {
            r.skip(2 + 1 + 5)?; // space, tier, idc
            r.skip(32)?; // compatibility flags
            r.skip(4)?; // progressive / interlaced / non-packed / frame-only
            r.skip(43)?;
            r.skip(1)?;
        }
        if level_present[i] {
            r.skip(8)?;
        }
    }

    Some(ProfileTierLevel {
        profile_space,
        high_tier,
        profile_idc,
        level_idc,
        progressive_source,
        interlaced_source,
    })
}

/// Result of parsing an SPS, with the evidence that the parse was aligned.
///
/// `trailing_ok` is the strongest cheap check available on a bitstream
/// parser. Every syntax structure ends with `rbsp_trailing_bits()` — a single
/// 1 bit, then zeros to the byte boundary. Landing on it means the parse
/// consumed *exactly* the right number of bits through
/// `profile_tier_level`, the scaling lists, the reference picture sets, HRD
/// and VUI. A single misread field anywhere would leave the reader at a
/// different offset and this would fail.
///
/// It is exposed rather than asserted internally because a stream may
/// legitimately carry SPS extensions this parser does not read, in which case
/// the reader stops early by design and the flag is false without the parse
/// being wrong.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpsParse {
    pub sps: Sps,
    /// True when the reader landed exactly on `rbsp_trailing_bits()`.
    pub trailing_ok: bool,
    /// True when `sps_extension_present_flag` was set, so the parse
    /// deliberately stopped before content it does not model.
    pub has_extensions: bool,
}

/// Parse an SPS and report whether the parse landed on the trailing bits.
///
/// Prefer this over [`parse_sps`] when validating a parser change; the flag
/// turns "it returned something plausible" into "it consumed the right bits".
#[must_use]
pub fn parse_sps_checked(nal: &[u8]) -> Option<SpsParse> {
    let (sps, mut r) = parse_sps_inner(nal, None)?;
    let has_extensions = r.flag().unwrap_or(false);
    let trailing_ok = if has_extensions {
        false
    } else {
        // rbsp_trailing_bits(): a 1 bit, then zeros to the byte boundary.
        match r.u(1) {
            Some(1) => {
                let mut ok = true;
                while let Some(b) = r.u(1) {
                    if b != 0 {
                        ok = false;
                        break;
                    }
                }
                ok
            }
            _ => false,
        }
    };
    Some(SpsParse {
        sps,
        trailing_ok,
        has_extensions,
    })
}

/// Parse an SPS from a NAL unit (header included).
///
/// Returns `None` on any malformed field rather than guessing — a wrong
/// resolution or bit depth propagates into buffer sizing, so a partial parse
/// is worse than no parse.
#[must_use]
pub fn parse_sps(nal: &[u8]) -> Option<Sps> {
    parse_sps_inner(nal, None).map(|(s, _)| s)
}

/// Parse an SPS and also retain its short-term reference picture sets.
///
/// Separate entry point because the sets are several kilobytes and most
/// callers — anything asking only about geometry, profile or colour — have no
/// use for them.
#[must_use]
pub fn parse_sps_with_rps(nal: &[u8], rps: &mut RpsTable) -> Option<Sps> {
    parse_sps_inner(nal, Some(rps)).map(|(s, _)| s)
}

fn parse_sps_inner<'a>(
    nal: &'a [u8],
    rps_out: Option<&mut RpsTable>,
) -> Option<(Sps, RbspReader<'a>)> {
    let hdr = parse_nal_header(nal)?;
    if hdr.nal_type != NalType::Sps {
        return None;
    }
    let mut r = RbspReader::new(&nal[2..]);

    let vps_id = r.u(4)? as u8;
    let max_sub_layers_minus1 = r.u(3)? as u8;
    if max_sub_layers_minus1 > 6 {
        return None;
    }
    let _temporal_id_nesting = r.flag()?;
    let ptl = parse_ptl(&mut r, max_sub_layers_minus1)?;

    let sps_id = u8::try_from(r.ue()?).ok()?;
    let chroma_format_idc = u8::try_from(r.ue()?).ok()?;
    if chroma_format_idc > 3 {
        return None;
    }
    let separate_colour_plane = if chroma_format_idc == 3 {
        r.flag()?
    } else {
        false
    };

    let width = r.ue()?;
    let height = r.ue()?;
    if width == 0 || height == 0 {
        return None;
    }

    let (mut crop_l, mut crop_r, mut crop_t, mut crop_b) = (0u32, 0u32, 0u32, 0u32);
    if r.flag()? {
        crop_l = r.ue()?;
        crop_r = r.ue()?;
        crop_t = r.ue()?;
        crop_b = r.ue()?;
    }

    let bit_depth_luma = u8::try_from(r.ue()?.checked_add(8)?).ok()?;
    let bit_depth_chroma = u8::try_from(r.ue()?.checked_add(8)?).ok()?;
    if bit_depth_luma > 16 || bit_depth_chroma > 16 {
        return None;
    }
    let log2_max_poc_lsb = u8::try_from(r.ue()?.checked_add(4)?).ok()?;
    if log2_max_poc_lsb > 16 {
        return None;
    }

    // sps_sub_layer_ordering_info_present_flag selects whether the DPB sizes
    // are given per sub-layer or only for the highest.
    let sub_layer_ordering = r.flag()?;
    let first = if sub_layer_ordering {
        0
    } else {
        max_sub_layers_minus1
    };
    let mut max_dec_pic_buffering = 0u8;
    let mut max_num_reorder_pics = 0u8;
    let mut max_latency_increase_plus1 = 0u32;
    for _ in first..=max_sub_layers_minus1 {
        max_dec_pic_buffering = u8::try_from(r.ue()?.saturating_add(1).min(255)).ok()?;
        max_num_reorder_pics = u8::try_from(r.ue()?.min(255)).ok()?;
        max_latency_increase_plus1 = r.ue()?;
    }

    let log2_min_cb_size = u8::try_from(r.ue()?.checked_add(3)?).ok()?;
    let log2_diff_max_min_cb = u8::try_from(r.ue()?).ok()?;
    let log2_ctb_size = log2_min_cb_size.checked_add(log2_diff_max_min_cb)?;
    // The standard allows CTBs of 16, 32 or 64 luma samples.
    if !(4..=6).contains(&log2_ctb_size) {
        return None;
    }
    let log2_min_tb_size = u8::try_from(r.ue()?.checked_add(2)?).ok()?;
    let log2_diff_max_min_tb = u8::try_from(r.ue()?).ok()?;
    let max_transform_hierarchy_depth_inter = u8::try_from(r.ue()?).ok()?;
    let max_transform_hierarchy_depth_intra = u8::try_from(r.ue()?).ok()?;

    let scaling_list_enabled = r.flag()?;
    if scaling_list_enabled && r.flag()? {
        skip_scaling_list_data(&mut r)?;
    }
    let amp_enabled = r.flag()?;
    let sao_enabled = r.flag()?;
    let pcm_enabled = r.flag()?;
    let mut pcm_bit_depth_luma = 0u8;
    let mut pcm_bit_depth_chroma = 0u8;
    let mut log2_min_pcm_cb_size = 0u8;
    let mut log2_diff_max_min_pcm_cb_size = 0u8;
    let mut pcm_loop_filter_disabled = false;
    if pcm_enabled {
        pcm_bit_depth_luma = (r.u(4)? as u8) + 1;
        pcm_bit_depth_chroma = (r.u(4)? as u8) + 1;
        log2_min_pcm_cb_size = u8::try_from(r.ue()?.checked_add(3)?).ok()?;
        log2_diff_max_min_pcm_cb_size = u8::try_from(r.ue()?).ok()?;
        pcm_loop_filter_disabled = r.flag()?;
    }

    let num_st_rps = r.ue()?;
    if num_st_rps as usize > MAX_ST_RPS {
        return None;
    }
    let num_short_term_ref_pic_sets = u8::try_from(num_st_rps).ok()?;
    // Every set is retained, because a later one may be coded as a delta
    // against an earlier one — the derivation needs the referenced set's
    // actual deltas, not just how many it had.
    let mut table = RpsTable::new();
    for i in 0..num_st_rps as usize {
        let set = parse_st_ref_pic_set(&mut r, i, num_st_rps as usize, &table.sets[..i])?;
        table.sets[i] = set;
        table.count = u8::try_from(i + 1).ok()?;
    }
    if let Some(out) = rps_out {
        *out = table;
    }

    let long_term_ref_pics_present = r.flag()?;
    let mut num_long_term_ref_pics_sps = 0u8;
    if long_term_ref_pics_present {
        let n = r.ue()?;
        if n > 32 {
            return None;
        }
        num_long_term_ref_pics_sps = u8::try_from(n).ok()?;
        for _ in 0..n {
            r.skip(log2_max_poc_lsb as usize)?; // lt_ref_pic_poc_lsb_sps
            r.flag()?; // used_by_curr_pic_lt_sps_flag
        }
    }
    let temporal_mvp_enabled = r.flag()?;
    let strong_intra_smoothing_enabled = r.flag()?;
    let colour = if r.flag()? {
        parse_vui(&mut r, max_sub_layers_minus1)?
    } else {
        ColourInfo::default()
    };

    // Cropping is expressed in chroma units.
    let (sub_w, sub_h) = sub_wh(chroma_format_idc, separate_colour_plane);
    let cut_w = crop_l.checked_add(crop_r)?.checked_mul(sub_w)?;
    let cut_h = crop_t.checked_add(crop_b)?.checked_mul(sub_h)?;
    let cropped_width = width.checked_sub(cut_w)?;
    let cropped_height = height.checked_sub(cut_h)?;
    if cropped_width == 0 || cropped_height == 0 {
        return None;
    }

    let ctb_size = 1u32 << log2_ctb_size;
    let ctb_width = width.div_ceil(ctb_size);
    let ctb_height = height.div_ceil(ctb_size);

    let sps = Sps {
        sps_id,
        vps_id,
        ptl,
        chroma_format_idc,
        separate_colour_plane,
        width,
        height,
        cropped_width,
        cropped_height,
        bit_depth_luma,
        bit_depth_chroma,
        log2_max_poc_lsb,
        max_dec_pic_buffering,
        max_num_reorder_pics,
        max_sub_layers_minus1,
        max_latency_increase_plus1,
        log2_min_cb_size,
        log2_ctb_size,
        ctb_width,
        ctb_height,
        log2_min_tb_size,
        log2_diff_max_min_tb,
        max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra,
        pcm_bit_depth_luma,
        pcm_bit_depth_chroma,
        log2_min_pcm_cb_size,
        log2_diff_max_min_pcm_cb_size,
        pcm_loop_filter_disabled,
        scaling_list_enabled,
        amp_enabled,
        sao_enabled,
        pcm_enabled,
        num_short_term_ref_pic_sets,
        long_term_ref_pics_present,
        num_long_term_ref_pics_sps,
        temporal_mvp_enabled,
        strong_intra_smoothing_enabled,
        colour,
    };
    Some((sps, r))
}

/// Colour signalling from VUI, which is where HDR announces itself.
///
/// UHD Blu-ray is BT.2020 primaries (9) with the SMPTE ST 2084 PQ transfer
/// (16) or HLG (18). `docs/reference/media-surfaces.md` forbids Spectra
/// *inferring* a colour model, so these are carried verbatim from the
/// bitstream for a Fluxor-owned surface to interpret — the codec reports what
/// the stream said and nothing more.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ColourInfo {
    pub present: bool,
    /// ISO/IEC 23001-8 colour primaries. 1 = BT.709, 9 = BT.2020.
    pub primaries: u8,
    /// Transfer characteristics. 1 = BT.709, 16 = PQ (ST 2084), 18 = HLG.
    pub transfer: u8,
    /// Matrix coefficients. 1 = BT.709, 9 = BT.2020 non-constant luminance.
    pub matrix: u8,
    /// False = limited ("tv") range, true = full ("pc") range.
    pub full_range: bool,
}

impl ColourInfo {
    /// Whether the stream signals a high-dynamic-range transfer function.
    ///
    /// Only PQ and HLG qualify; BT.2020 primaries alone are wide gamut, not
    /// HDR, and conflating the two is a common way to get tone mapping wrong.
    #[must_use]
    pub const fn is_hdr(self) -> bool {
        self.present && (self.transfer == 16 || self.transfer == 18)
    }

    /// Wide colour gamut (BT.2020 primaries), independent of transfer.
    #[must_use]
    pub const fn is_wide_gamut(self) -> bool {
        self.present && self.primaries == 9
    }
}

/// Skip `hrd_parameters`. Bit-exact consumption is all that matters; nothing
/// in it is needed to decode.
fn skip_hrd(r: &mut RbspReader<'_>, common_inf: bool, max_sub_layers_minus1: u8) -> Option<()> {
    let mut nal_hrd = false;
    let mut vcl_hrd = false;
    let mut sub_pic = false;
    if common_inf {
        nal_hrd = r.flag()?;
        vcl_hrd = r.flag()?;
        if nal_hrd || vcl_hrd {
            sub_pic = r.flag()?;
            if sub_pic {
                r.skip(8 + 5 + 1 + 5)?;
            }
            r.skip(4 + 4)?; // bit_rate_scale, cpb_size_scale
            if sub_pic {
                r.skip(4)?; // cpb_size_du_scale
            }
            r.skip(5 + 5 + 5)?;
        }
    }
    for _ in 0..=max_sub_layers_minus1 {
        let fixed_general = r.flag()?;
        let fixed_within_cvs = if fixed_general { true } else { r.flag()? };
        let mut low_delay = false;
        if fixed_within_cvs {
            r.ue()?; // elemental_duration_in_tc_minus1
        } else {
            low_delay = r.flag()?;
        }
        let cpb_cnt = if low_delay { 0 } else { r.ue()? };
        if cpb_cnt > 32 {
            return None;
        }
        for _ in 0..(if nal_hrd { 1 } else { 0 }) + (if vcl_hrd { 1 } else { 0 }) {
            for _ in 0..=cpb_cnt {
                r.ue()?; // bit_rate_value_minus1
                r.ue()?; // cpb_size_value_minus1
                if sub_pic {
                    r.ue()?; // cpb_size_du_value_minus1
                    r.ue()?; // bit_rate_du_value_minus1
                }
                r.flag()?; // cbr_flag
            }
        }
    }
    Some(())
}

/// Parse `vui_parameters`, returning the colour signalling.
///
/// Everything else is consumed and discarded, but consumed *exactly*: the SPS
/// extension flags and the RBSP stop bit follow, and the test suite verifies
/// the parse lands on them.
fn parse_vui(r: &mut RbspReader<'_>, max_sub_layers_minus1: u8) -> Option<ColourInfo> {
    if r.flag()? {
        // aspect_ratio_info_present_flag
        let idc = r.u(8)?;
        if idc == 255 {
            r.skip(32)?; // sar_width, sar_height
        }
    }
    if r.flag()? {
        r.flag()?; // overscan_appropriate_flag
    }

    let mut colour = ColourInfo::default();
    if r.flag()? {
        // video_signal_type_present_flag
        r.skip(3)?; // video_format
        colour.full_range = r.flag()?;
        if r.flag()? {
            // colour_description_present_flag
            colour.present = true;
            colour.primaries = r.u(8)? as u8;
            colour.transfer = r.u(8)? as u8;
            colour.matrix = r.u(8)? as u8;
        }
    }
    if r.flag()? {
        // chroma_loc_info_present_flag
        r.ue()?;
        r.ue()?;
    }
    r.skip(3)?; // neutral_chroma, field_seq, frame_field_info
    if r.flag()? {
        // default_display_window_flag
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    if r.flag()? {
        // vui_timing_info_present_flag
        r.skip(32)?; // num_units_in_tick
        r.skip(32)?; // time_scale
        if r.flag()? {
            r.ue()?; // num_ticks_poc_diff_one_minus1
        }
        if r.flag()? {
            skip_hrd(r, true, max_sub_layers_minus1)?;
        }
    }
    if r.flag()? {
        // bitstream_restriction_flag
        r.skip(3)?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
        r.ue()?;
    }
    Some(colour)
}

/// Maximum `num_short_term_ref_pic_sets`. The standard's own ceiling is 64.
pub const MAX_ST_RPS: usize = 64;

/// Skip `scaling_list_data()`.
///
/// The coefficients are not retained: a stateless backend that needs them
/// re-reads the raw parameter set, and a software decoder builds its own
/// tables. What matters here is consuming exactly the right number of bits,
/// because everything after this in the parameter set depends on it.
fn skip_scaling_list_data(r: &mut RbspReader<'_>) -> Option<()> {
    for size_id in 0..4u32 {
        let step = if size_id == 3 { 3 } else { 1 };
        let mut matrix_id = 0u32;
        while matrix_id < 6 {
            if r.flag()? {
                // Explicitly coded.
                let coef_num = core::cmp::min(64u32, 1u32 << (4 + (size_id << 1)));
                if size_id > 1 {
                    r.se()?; // scaling_list_dc_coef_minus8
                }
                for _ in 0..coef_num {
                    r.se()?; // scaling_list_delta_coef
                }
            } else {
                r.ue()?; // scaling_list_pred_matrix_id_delta
            }
            matrix_id += step;
        }
    }
    Some(())
}

/// Deltas retained per short-term reference picture set.
///
/// The standard bounds `NumDeltaPocs` by `sps_max_dec_pic_buffering`, which
/// is itself at most 16, so this is the real ceiling rather than a guess.
pub const MAX_RPS_DELTAS: usize = 16;

/// A short-term reference picture set, with contents.
///
/// Negative deltas come first, ordered nearest-first (-1, -2, …), then
/// positive, also nearest-first (+1, +2, …). That ordering is not incidental:
/// reference list initialisation walks these in order, so getting it wrong
/// produces lists that reference the wrong pictures without any error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShortTermRps {
    pub num_negative: u8,
    pub num_positive: u8,
    /// POC deltas relative to the current picture.
    pub delta_poc: [i32; MAX_RPS_DELTAS],
    /// Whether each entry is used by the current picture, as opposed to being
    /// held only for a later one.
    pub used: [bool; MAX_RPS_DELTAS],
}

impl Default for ShortTermRps {
    fn default() -> Self {
        Self {
            num_negative: 0,
            num_positive: 0,
            delta_poc: [0; MAX_RPS_DELTAS],
            used: [false; MAX_RPS_DELTAS],
        }
    }
}

impl ShortTermRps {
    #[must_use]
    pub const fn num_delta_pocs(&self) -> usize {
        self.num_negative as usize + self.num_positive as usize
    }

    /// How many entries are used by the current picture — the short-term part
    /// of `NumPicTotalCurr` (H.265 §7.4.7.2).
    #[must_use]
    pub fn num_used_by_curr(&self) -> u32 {
        self.used[..self.num_delta_pocs()]
            .iter()
            .filter(|&&u| u)
            .count() as u32
    }
}

/// A parsed set of short-term reference picture sets from an SPS.
///
/// Held separately from [`Sps`] rather than inside it: at 64 sets this is
/// several kilobytes, and `Sps` is a small `Copy` value passed around freely.
/// A caller that needs the sets owns one of these; a caller that only wants
/// geometry and profile does not pay for it.
#[derive(Clone, Debug)]
pub struct RpsTable {
    sets: [ShortTermRps; MAX_ST_RPS],
    count: u8,
}

impl Default for RpsTable {
    fn default() -> Self {
        Self::new()
    }
}

impl RpsTable {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sets: [ShortTermRps::default(); MAX_ST_RPS],
            count: 0,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.count as usize
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[must_use]
    pub fn get(&self, i: usize) -> Option<&ShortTermRps> {
        if i < self.count as usize {
            Some(&self.sets[i])
        } else {
            None
        }
    }
}

/// Parse one `st_ref_pic_set`, returning its contents.
///
/// `prev` holds every previously parsed set, because a set may be coded as a
/// delta against an earlier one. That branch is the fiddly half of HEVC's
/// reference signalling: it walks the referenced set's positive deltas in
/// reverse, then the delta itself, then the negatives — and an error anywhere
/// produces a valid-looking set that references the wrong pictures.
fn parse_st_ref_pic_set(
    r: &mut RbspReader<'_>,
    idx: usize,
    num_sets: usize,
    prev: &[ShortTermRps],
) -> Option<ShortTermRps> {
    let inter_pred = if idx != 0 { r.flag()? } else { false };
    let mut out = ShortTermRps::default();

    if inter_pred {
        let delta_idx_minus1 = if idx == num_sets { r.ue()? } else { 0 };
        let delta_idx = usize::try_from(delta_idx_minus1).ok()?.checked_add(1)?;
        let ref_idx = idx.checked_sub(delta_idx)?;
        let reference = prev.get(ref_idx)?;

        let delta_rps_sign = r.flag()?;
        let abs_delta_rps_minus1 = r.ue()?;
        let abs = i32::try_from(abs_delta_rps_minus1.checked_add(1)?).ok()?;
        let delta_rps = if delta_rps_sign { -abs } else { abs };

        let n = reference.num_delta_pocs();
        let mut used_flag = [false; MAX_RPS_DELTAS + 1];
        let mut use_delta = [true; MAX_RPS_DELTAS + 1];
        for j in 0..=n {
            used_flag[j] = r.flag()?;
            if !used_flag[j] {
                use_delta[j] = r.flag()?;
            }
        }

        // Negative side: the reference's positives (reversed), then the delta
        // itself, then the reference's negatives.
        let mut i = 0usize;
        let neg_ref = reference.num_negative as usize;
        for j in (0..reference.num_positive as usize).rev() {
            let d = reference.delta_poc[neg_ref + j].checked_add(delta_rps)?;
            if d < 0 && use_delta[neg_ref + j] {
                if i >= MAX_RPS_DELTAS {
                    return None;
                }
                out.delta_poc[i] = d;
                out.used[i] = used_flag[neg_ref + j];
                i += 1;
            }
        }
        if delta_rps < 0 && use_delta[n] {
            if i >= MAX_RPS_DELTAS {
                return None;
            }
            out.delta_poc[i] = delta_rps;
            out.used[i] = used_flag[n];
            i += 1;
        }
        for j in 0..neg_ref {
            let d = reference.delta_poc[j].checked_add(delta_rps)?;
            if d < 0 && use_delta[j] {
                if i >= MAX_RPS_DELTAS {
                    return None;
                }
                out.delta_poc[i] = d;
                out.used[i] = used_flag[j];
                i += 1;
            }
        }
        out.num_negative = u8::try_from(i).ok()?;

        // Positive side: mirror image.
        let mut k = i;
        for j in (0..neg_ref).rev() {
            let d = reference.delta_poc[j].checked_add(delta_rps)?;
            if d > 0 && use_delta[j] {
                if k >= MAX_RPS_DELTAS {
                    return None;
                }
                out.delta_poc[k] = d;
                out.used[k] = used_flag[j];
                k += 1;
            }
        }
        if delta_rps > 0 && use_delta[n] {
            if k >= MAX_RPS_DELTAS {
                return None;
            }
            out.delta_poc[k] = delta_rps;
            out.used[k] = used_flag[n];
            k += 1;
        }
        for j in 0..reference.num_positive as usize {
            let d = reference.delta_poc[neg_ref + j].checked_add(delta_rps)?;
            if d > 0 && use_delta[neg_ref + j] {
                if k >= MAX_RPS_DELTAS {
                    return None;
                }
                out.delta_poc[k] = d;
                out.used[k] = used_flag[neg_ref + j];
                k += 1;
            }
        }
        out.num_positive = u8::try_from(k - i).ok()?;
        Some(out)
    } else {
        let num_negative = r.ue()?;
        let num_positive = r.ue()?;
        if num_negative as usize > MAX_RPS_DELTAS
            || num_positive as usize > MAX_RPS_DELTAS
            || (num_negative + num_positive) as usize > MAX_RPS_DELTAS
        {
            return None;
        }
        out.num_negative = u8::try_from(num_negative).ok()?;
        out.num_positive = u8::try_from(num_positive).ok()?;

        // Deltas are coded incrementally, so each is relative to the last.
        let mut prev_poc = 0i32;
        for i in 0..num_negative as usize {
            let d = i32::try_from(r.ue()?.checked_add(1)?).ok()?;
            prev_poc = prev_poc.checked_sub(d)?;
            out.delta_poc[i] = prev_poc;
            out.used[i] = r.flag()?;
        }
        prev_poc = 0;
        for i in 0..num_positive as usize {
            let d = i32::try_from(r.ue()?.checked_add(1)?).ok()?;
            prev_poc = prev_poc.checked_add(d)?;
            out.delta_poc[num_negative as usize + i] = prev_poc;
            out.used[num_negative as usize + i] = r.flag()?;
        }
        Some(out)
    }
}

/// Picture parameter set — the fields a backend needs.
///
/// Mirrors what `V4L2_CID_STATELESS_HEVC_PPS` carries, which is also what
/// VAAPI and NVDEC want under other names.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pps {
    pub pps_id: u8,
    pub sps_id: u8,
    pub dependent_slice_segments_enabled: bool,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u8,
    pub sign_data_hiding_enabled: bool,
    pub cabac_init_present: bool,
    pub num_ref_idx_l0_default_active: u8,
    pub num_ref_idx_l1_default_active: u8,
    pub init_qp: i8,
    pub constrained_intra_pred: bool,
    pub transform_skip_enabled: bool,
    pub cu_qp_delta_enabled: bool,
    pub diff_cu_qp_delta_depth: u8,
    pub cb_qp_offset: i8,
    pub cr_qp_offset: i8,
    pub slice_chroma_qp_offsets_present: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub transquant_bypass_enabled: bool,
    /// Tiles and wavefront parallelism. A backend must know: they change how
    /// slice data is segmented, and entry point offsets only exist when one
    /// of them is on.
    pub tiles_enabled: bool,
    pub entropy_coding_sync_enabled: bool,
    pub num_tile_columns: u16,
    pub num_tile_rows: u16,
    pub uniform_spacing: bool,
    pub loop_filter_across_tiles_enabled: bool,
    pub loop_filter_across_slices_enabled: bool,
    pub deblocking_filter_control_present: bool,
    pub deblocking_filter_override_enabled: bool,
    pub deblocking_filter_disabled: bool,
    pub beta_offset_div2: i8,
    pub tc_offset_div2: i8,
    pub scaling_list_data_present: bool,
    pub lists_modification_present: bool,
    pub log2_parallel_merge_level: u8,
    pub slice_segment_header_extension_present: bool,
}

/// Parse a PPS from a NAL unit (header included).
#[must_use]
pub fn parse_pps(nal: &[u8]) -> Option<Pps> {
    let hdr = parse_nal_header(nal)?;
    if hdr.nal_type != NalType::Pps {
        return None;
    }
    let mut r = RbspReader::new(&nal[2..]);
    let mut p = Pps {
        pps_id: u8::try_from(r.ue()?).ok()?,
        sps_id: u8::try_from(r.ue()?).ok()?,
        ..Pps::default()
    };
    p.dependent_slice_segments_enabled = r.flag()?;
    p.output_flag_present = r.flag()?;
    p.num_extra_slice_header_bits = r.u(3)? as u8;
    p.sign_data_hiding_enabled = r.flag()?;
    p.cabac_init_present = r.flag()?;
    p.num_ref_idx_l0_default_active = u8::try_from(r.ue()?.checked_add(1)?).ok()?;
    p.num_ref_idx_l1_default_active = u8::try_from(r.ue()?.checked_add(1)?).ok()?;
    p.init_qp = i8::try_from(r.se()?.checked_add(26)?).ok()?;
    p.constrained_intra_pred = r.flag()?;
    p.transform_skip_enabled = r.flag()?;
    p.cu_qp_delta_enabled = r.flag()?;
    if p.cu_qp_delta_enabled {
        p.diff_cu_qp_delta_depth = u8::try_from(r.ue()?).ok()?;
    }
    p.cb_qp_offset = i8::try_from(r.se()?).ok()?;
    p.cr_qp_offset = i8::try_from(r.se()?).ok()?;
    p.slice_chroma_qp_offsets_present = r.flag()?;
    p.weighted_pred = r.flag()?;
    p.weighted_bipred = r.flag()?;
    p.transquant_bypass_enabled = r.flag()?;
    p.tiles_enabled = r.flag()?;
    p.entropy_coding_sync_enabled = r.flag()?;

    p.num_tile_columns = 1;
    p.num_tile_rows = 1;
    p.uniform_spacing = true;
    p.loop_filter_across_tiles_enabled = true;
    if p.tiles_enabled {
        p.num_tile_columns = u16::try_from(r.ue()?.checked_add(1)?).ok()?;
        p.num_tile_rows = u16::try_from(r.ue()?.checked_add(1)?).ok()?;
        // A picture cannot have more tiles than CTBs; bound the loops below
        // against a corrupt count before allocating any time to them.
        if p.num_tile_columns > 1024 || p.num_tile_rows > 1024 {
            return None;
        }
        p.uniform_spacing = r.flag()?;
        if !p.uniform_spacing {
            for _ in 1..p.num_tile_columns {
                r.ue()?; // column_width_minus1
            }
            for _ in 1..p.num_tile_rows {
                r.ue()?; // row_height_minus1
            }
        }
        p.loop_filter_across_tiles_enabled = r.flag()?;
    }

    p.loop_filter_across_slices_enabled = r.flag()?;
    p.deblocking_filter_control_present = r.flag()?;
    if p.deblocking_filter_control_present {
        p.deblocking_filter_override_enabled = r.flag()?;
        p.deblocking_filter_disabled = r.flag()?;
        if !p.deblocking_filter_disabled {
            p.beta_offset_div2 = i8::try_from(r.se()?).ok()?;
            p.tc_offset_div2 = i8::try_from(r.se()?).ok()?;
        }
    }
    p.scaling_list_data_present = r.flag()?;
    if p.scaling_list_data_present {
        skip_scaling_list_data(&mut r)?;
    }
    p.lists_modification_present = r.flag()?;
    p.log2_parallel_merge_level = u8::try_from(r.ue()?.checked_add(2)?).ok()?;
    p.slice_segment_header_extension_present = r.flag()?;

    Some(p)
}

/// Slice coding type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliceType {
    /// Bi-predictive. Its presence is the signal that decode order and
    /// presentation order differ, which is what makes DTS meaningful.
    B,
    P,
    I,
}

impl SliceType {
    const fn from_ue(v: u32) -> Option<Self> {
        match v {
            0 => Some(Self::B),
            1 => Some(Self::P),
            2 => Some(Self::I),
            _ => None,
        }
    }
}

/// The slice segment header fields a backend needs before slice data.
///
/// Parsing stops at the point where reference list construction begins —
/// `pred_weight_table` and the entry point offsets need `NumPicTotalCurr`,
/// which comes from evaluating the reference picture set against the DPB.
/// Everything up to there is what identifies and orders the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SliceHeader {
    pub first_slice_in_pic: bool,
    pub dependent_slice_segment: bool,
    pub pps_id: u8,
    /// Absent for a dependent slice segment, which inherits it.
    pub slice_type: Option<SliceType>,
    /// `slice_pic_order_cnt_lsb`. Zero for IDR, which does not code it.
    pub poc_lsb: u16,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub temporal_mvp_enabled: bool,
    /// The short-term reference picture set in force for this picture —
    /// either coded inline in the header or selected from the SPS table.
    /// `None` for IDR, which references nothing.
    pub rps: Option<ShortTermRps>,
    /// Effective `num_ref_idx_lX_active`, after any slice-level override.
    /// Zero for an I slice.
    pub num_ref_idx_l0_active: u8,
    pub num_ref_idx_l1_active: u8,

    // ── Fields below are parsed by the full Main/Main-10 pass and are what a
    //    hardware back end programs per slice. Zero/false for a header parsed
    //    only up to reference-list construction (see `full`).
    /// The slice's luma QP: `pps.init_qp + slice_qp_delta` [H.265 §7.4.7.1].
    pub slice_qp: i8,
    /// Slice-level chroma QP offsets, added to the PPS offsets. Zero unless the
    /// PPS signals `slice_chroma_qp_offsets_present`.
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    /// `MaxNumMergeCand = 5 − five_minus_max_num_merge_cand`. Zero for I.
    pub max_merge_cand: u8,
    /// Temporal-MVP collocation. Meaningful only when `temporal_mvp_enabled`.
    pub collocated_from_l0: bool,
    pub collocated_ref_idx: u8,
    /// `mvd_l1_zero_flag` — B slices only.
    pub mvd_l1_zero: bool,
    /// `cabac_init_flag` — selects the P/B context table swap.
    pub cabac_init: bool,
    /// Effective deblocking parameters, after any slice-level override folds in
    /// over the PPS defaults.
    pub deblocking_disabled: bool,
    pub beta_offset_div2: i8,
    pub tc_offset_div2: i8,
    /// Effective `slice_loop_filter_across_slices_enabled_flag`.
    pub loop_filter_across_slices: bool,
    /// A `pred_weight_table` was present for this slice (weighted prediction).
    /// The weights themselves are not yet captured — see the parser note.
    pub weighted_pred: bool,
    /// `num_entry_point_offsets` — non-zero only with tiles or WPP.
    pub num_entry_point_offsets: u32,
    /// Byte offset of the slice segment **data** from the start of the RBSP
    /// (after `byte_alignment`). This is what phase-1's bitstream feed needs to
    /// point past the header. NB: RBSP-relative — it counts de-emulated bytes,
    /// which equals the raw offset unless the header itself carried an
    /// emulation-prevention byte (rare in practice; flagged for the provider).
    pub data_byte_offset: u32,
    /// True when the full Main/Main-10 pass ran to `byte_alignment` and the
    /// fields above are populated; false for a header parsed only far enough to
    /// identify and order the picture.
    pub full: bool,
}

/// Parse a slice segment header.
///
/// Needs the active SPS and PPS: the header's shape depends on them
/// throughout — whether a dependent-slice flag is present, how wide the
/// segment address field is, whether SAO flags appear. That coupling is why a
/// slice header cannot be parsed standalone, and why a backend must track
/// parameter sets rather than treat each NAL independently.
#[must_use]
pub fn parse_slice_header(nal: &[u8], sps: &Sps, pps: &Pps) -> Option<SliceHeader> {
    parse_slice_header_inner(nal, sps, pps, None, false, None)
}

/// Parse a slice segment header **completely**, through `byte_alignment`, for
/// the Main and Main-10 profiles.
///
/// This is what a hardware back end needs: it populates the QP, chroma offsets,
/// merge count, collocated reference, deblocking, loop-filter, entry-point and
/// slice-data-offset fields that [`parse_slice_header`] stops short of. The
/// returned header has [`SliceHeader::full`] set.
///
/// Scoped to Main / Main 10: the screen-content (`use_integer_mv`) and
/// range-extension (`cu_chroma_qp_offset`, ACT) slice-header syntax is not
/// handled, because those profiles are out of scope for this decoder. A
/// dependent slice segment still returns early, inheriting from its
/// independent segment, and carries `full = false`.
#[must_use]
pub fn parse_slice_header_full(
    nal: &[u8],
    sps: &Sps,
    pps: &Pps,
    rps_table: Option<&RpsTable>,
) -> Option<SliceHeader> {
    parse_slice_header_inner(nal, sps, pps, rps_table, true, None)
}

/// As [`parse_slice_header`], but able to resolve a reference picture set
/// selected from the SPS table rather than coded inline.
///
/// A stream that declares its sets in the SPS and selects one per slice needs
/// the table to know which; without it the set is reported as `None` and
/// reference list construction cannot proceed for that picture.
#[must_use]
pub fn parse_slice_header_with_rps(
    nal: &[u8],
    sps: &Sps,
    pps: &Pps,
    rps_table: Option<&RpsTable>,
) -> Option<SliceHeader> {
    parse_slice_header_inner(nal, sps, pps, rps_table, false, None)
}

/// As [`parse_slice_header_full`], but also captures the `pred_weight_table`
/// into `weights`.
///
/// Weighted prediction is not optional for real content — x265 enables `weightp`
/// by default — and a back end that programs a weighted slice without the
/// weights decodes a picture with the wrong brightness rather than failing. This
/// is the entry point a hardware back end should use.
#[must_use]
pub fn parse_slice_header_weighted(
    nal: &[u8],
    sps: &Sps,
    pps: &Pps,
    rps_table: Option<&RpsTable>,
    weights: &mut PredWeightTable,
) -> Option<SliceHeader> {
    parse_slice_header_inner(nal, sps, pps, rps_table, true, Some(weights))
}

fn parse_slice_header_inner(
    nal: &[u8],
    sps: &Sps,
    pps: &Pps,
    rps_table: Option<&RpsTable>,
    full: bool,
    weights: Option<&mut PredWeightTable>,
) -> Option<SliceHeader> {
    let hdr = parse_nal_header(nal)?;
    let t = match hdr.nal_type {
        NalType::Other(v) if v > 21 => return None, // not a slice
        NalType::Vps | NalType::Sps | NalType::Pps => return None,
        other => other,
    };
    let mut r = RbspReader::new(&nal[2..]);

    let first_slice_in_pic = r.flag()?;
    if t.is_irap() {
        r.flag()?; // no_output_of_prior_pics_flag
    }
    let pps_id = u8::try_from(r.ue()?).ok()?;

    let mut dependent = false;
    if !first_slice_in_pic {
        if pps.dependent_slice_segments_enabled {
            dependent = r.flag()?;
        }
        // slice_segment_address is Ceil(Log2(PicSizeInCtbsY)) bits.
        let pic_size_ctbs = sps.ctb_width.checked_mul(sps.ctb_height)?;
        let bits = 32 - pic_size_ctbs.saturating_sub(1).leading_zeros();
        r.skip(bits as usize)?;
    }

    let mut header = SliceHeader {
        first_slice_in_pic,
        dependent_slice_segment: dependent,
        pps_id,
        slice_type: None,
        poc_lsb: 0,
        sao_luma: false,
        sao_chroma: false,
        temporal_mvp_enabled: false,
        rps: None,
        num_ref_idx_l0_active: 0,
        num_ref_idx_l1_active: 0,
        slice_qp: 0,
        slice_cb_qp_offset: 0,
        slice_cr_qp_offset: 0,
        max_merge_cand: 0,
        collocated_from_l0: false,
        collocated_ref_idx: 0,
        mvd_l1_zero: false,
        cabac_init: false,
        deblocking_disabled: pps.deblocking_filter_disabled,
        beta_offset_div2: pps.beta_offset_div2,
        tc_offset_div2: pps.tc_offset_div2,
        loop_filter_across_slices: pps.loop_filter_across_slices_enabled,
        weighted_pred: false,
        num_entry_point_offsets: 0,
        data_byte_offset: 0,
        full: false,
    };
    if dependent {
        // A dependent segment inherits everything below from the independent
        // segment that preceded it.
        return Some(header);
    }

    r.skip(pps.num_extra_slice_header_bits as usize)?;
    header.slice_type = Some(SliceType::from_ue(r.ue()?)?);
    if pps.output_flag_present {
        r.flag()?; // pic_output_flag
    }
    if sps.separate_colour_plane {
        r.skip(2)?; // colour_plane_id
    }

    let mut num_lt_used = 0u32;
    let is_idr = matches!(t, NalType::IdrWRadl | NalType::IdrNLp);
    if !is_idr {
        header.poc_lsb = u16::try_from(r.u(sps.log2_max_poc_lsb)?).ok()?;
        if !r.flag()? {
            // short_term_ref_pic_set_sps_flag clear: the set is coded inline,
            // at index num_short_term_ref_pic_sets.
            let n = sps.num_short_term_ref_pic_sets as usize;
            let prev = rps_table.map_or(&[][..], |t| &t.sets[..t.count as usize]);
            header.rps = Some(parse_st_ref_pic_set(&mut r, n, n, prev)?);
        } else {
            // Selected from the SPS table. The index field is only present
            // when there is more than one set to choose between.
            let idx = if sps.num_short_term_ref_pic_sets > 1 {
                let bits = 32 - u32::from(sps.num_short_term_ref_pic_sets - 1).leading_zeros();
                r.u(bits as u8)? as usize
            } else {
                0
            };
            header.rps = rps_table.and_then(|t| t.get(idx)).copied();
        }
        if sps.long_term_ref_pics_present {
            let num_lt_sps = if sps.num_long_term_ref_pics_sps > 0 {
                r.ue()?
            } else {
                0
            };
            let num_lt_pics = r.ue()?;
            let total = num_lt_sps.checked_add(num_lt_pics)?;
            if total > 64 {
                return None;
            }
            for i in 0..total {
                if i < num_lt_sps {
                    if sps.num_long_term_ref_pics_sps > 1 {
                        let bits =
                            32 - u32::from(sps.num_long_term_ref_pics_sps - 1).leading_zeros();
                        r.skip(bits as usize)?;
                    }
                    // used_by_curr for an SPS-listed candidate comes from the
                    // SPS long-term set, which this parser does not retain;
                    // counted as used (an upper bound on NumPicTotalCurr, which
                    // only affects list-modification field widths in the rare
                    // LT + lists_modification case).
                    num_lt_used += 1;
                } else {
                    r.skip(sps.log2_max_poc_lsb as usize)?; // poc_lsb_lt
                    if r.flag()? {
                        // used_by_curr_pic_lt_flag
                        num_lt_used += 1;
                    }
                }
                if r.flag()? {
                    r.ue()?; // delta_poc_msb_cycle_lt
                }
            }
        }
        if sps.temporal_mvp_enabled {
            header.temporal_mvp_enabled = r.flag()?;
        }
    }

    if sps.sao_enabled {
        header.sao_luma = r.flag()?;
        // ChromaArrayType is 0 only for monochrome or separate planes.
        let chroma_array_type = if sps.separate_colour_plane {
            0
        } else {
            sps.chroma_format_idc
        };
        if chroma_array_type != 0 {
            header.sao_chroma = r.flag()?;
        }
    }

    if matches!(header.slice_type, Some(SliceType::P | SliceType::B)) {
        let is_b = header.slice_type == Some(SliceType::B);
        let (mut l0, mut l1) = (
            pps.num_ref_idx_l0_default_active,
            pps.num_ref_idx_l1_default_active,
        );
        if r.flag()? {
            // num_ref_idx_active_override_flag
            l0 = u8::try_from(r.ue()?.checked_add(1)?).ok()?;
            if is_b {
                l1 = u8::try_from(r.ue()?.checked_add(1)?).ok()?;
            }
        }
        if l0 as usize > MAX_RPS_DELTAS || l1 as usize > MAX_RPS_DELTAS {
            return None;
        }
        header.num_ref_idx_l0_active = l0;
        header.num_ref_idx_l1_active = if is_b { l1 } else { 0 };

        if !full {
            // The lightweight pass stops here: everything below identifies
            // decoding parameters, not the picture's identity or order.
            // `init_ref_pic_lists` produces the INITIAL lists from what is
            // known so far.
            return Some(header);
        }

        let chroma_array_type = if sps.separate_colour_plane {
            0
        } else {
            sps.chroma_format_idc
        };

        // NumPicTotalCurr: short-term used (from the RPS) + long-term used.
        let st_used = header
            .rps
            .as_ref()
            .map_or(0u32, |rps| rps.num_used_by_curr());
        let num_pic_total_curr = st_used + num_lt_used;

        // ref_pic_lists_modification (H.265 §7.3.6.2).
        if pps.lists_modification_present && num_pic_total_curr > 1 {
            let entry_bits = 32 - (num_pic_total_curr - 1).leading_zeros();
            if r.flag()? {
                // ref_pic_list_modification_flag_l0
                for _ in 0..l0 {
                    r.skip(entry_bits as usize)?; // list_entry_l0
                }
            }
            if is_b && r.flag()? {
                for _ in 0..l1 {
                    r.skip(entry_bits as usize)?; // list_entry_l1
                }
            }
        }

        if is_b {
            header.mvd_l1_zero = r.flag()?;
        }
        if pps.cabac_init_present {
            header.cabac_init = r.flag()?;
        }
        if header.temporal_mvp_enabled {
            header.collocated_from_l0 = if is_b { r.flag()? } else { true };
            let active = if header.collocated_from_l0 { l0 } else { l1 };
            if active > 1 {
                header.collocated_ref_idx = u8::try_from(r.ue()?).ok()?;
            }
        }

        // pred_weight_table (H.265 §7.3.6.3): present for weighted P / bi-pred
        // B. The values are consumed to stay aligned; capture of the weights
        // themselves is deferred (see `weighted_pred`).
        let weighted = (pps.weighted_pred && !is_b) || (pps.weighted_bipred && is_b);
        if weighted {
            header.weighted_pred = true;
            parse_pred_weight_table(
                &mut r,
                chroma_array_type,
                l0,
                if is_b { l1 } else { 0 },
                sps.bit_depth_chroma,
                weights,
            )?;
        }

        // five_minus_max_num_merge_cand → MaxNumMergeCand.
        let five_minus = r.ue()?;
        header.max_merge_cand = u8::try_from(5u32.checked_sub(five_minus)?).ok()?;
        // (Main / Main 10: no use_integer_mv_flag.)
    }

    if !full {
        return Some(header);
    }

    // slice_qp_delta applies to every slice type.
    let qp = i32::from(pps.init_qp) + r.se()?;
    header.slice_qp = i8::try_from(qp).ok()?;

    if pps.slice_chroma_qp_offsets_present {
        header.slice_cb_qp_offset = i8::try_from(r.se()?).ok()?;
        header.slice_cr_qp_offset = i8::try_from(r.se()?).ok()?;
    }
    // (Main / Main 10: no slice_act_*_qp_offset, no cu_chroma_qp_offset_enabled.)

    let mut deblocking_override = false;
    if pps.deblocking_filter_override_enabled {
        deblocking_override = r.flag()?;
    }
    if deblocking_override {
        header.deblocking_disabled = r.flag()?;
        if !header.deblocking_disabled {
            header.beta_offset_div2 = i8::try_from(r.se()?).ok()?;
            header.tc_offset_div2 = i8::try_from(r.se()?).ok()?;
        }
    }

    if pps.loop_filter_across_slices_enabled
        && (header.sao_luma || header.sao_chroma || !header.deblocking_disabled)
    {
        header.loop_filter_across_slices = r.flag()?;
    }

    if pps.tiles_enabled || pps.entropy_coding_sync_enabled {
        header.num_entry_point_offsets = r.ue()?;
        if header.num_entry_point_offsets > 0 {
            let offset_len = r.ue()?.checked_add(1)?;
            if offset_len > 32 {
                return None;
            }
            for _ in 0..header.num_entry_point_offsets {
                r.skip(offset_len as usize)?; // entry_point_offset_minus1
            }
        }
    }

    if pps.slice_segment_header_extension_present {
        let len = r.ue()?;
        r.skip((len as usize).checked_mul(8)?)?;
    }

    // byte_alignment(): a 1 bit then zeros to the byte boundary. Landing on it
    // is the slice-header analogue of the SPS trailing-bits check — it proves
    // the whole header was consumed at exactly the right width.
    if !r.flag()? {
        return None; // alignment_bit_equal_to_one must be 1
    }
    while !r.bits_read().is_multiple_of(8) {
        if r.flag()? {
            return None; // alignment_bit_equal_to_zero must be 0
        }
    }
    header.data_byte_offset = u32::try_from(r.bits_read() / 8).ok()?;
    header.full = true;

    Some(header)
}

/// Consume a `pred_weight_table` (H.265 §7.3.6.3) without retaining the
/// weights, so the reader lands on the syntax that follows. `l1` is 0 for a
/// P slice.
/// A slice's `pred_weight_table`, with every value RECONSTRUCTED rather than
/// left as the coded delta [H.265 §7.3.6.3, §7.4.7.3].
///
/// The hardware wants the reconstructed weight (`delta + default`), so doing
/// the reconstruction here keeps the "+ (1 << denom)" in one audited place
/// instead of in every back end. Indexed by list (0 = L0, 1 = L1) then by
/// reference index within that list.
///
/// Kept out of [`SliceHeader`] and filled through an out-parameter: it is ~400
/// bytes, and `SliceHeader` is `Copy` and returned by value on a path that most
/// callers use without weights at all.
#[derive(Clone, Copy, Debug)]
pub struct PredWeightTable {
    /// `luma_log2_weight_denom` and `ChromaLog2WeightDenom`.
    pub luma_denom: u8,
    pub chroma_denom: u8,
    /// `LumaWeightLX[i]` — already `(1 << luma_denom) + delta`.
    pub luma_weight: [[i16; MAX_RPS_DELTAS]; 2],
    /// `luma_offset_lX[i]`, as coded (8-bit range).
    pub luma_offset: [[i16; MAX_RPS_DELTAS]; 2],
    /// `ChromaWeightLX[i][j]`, j = 0 for Cb, 1 for Cr.
    pub chroma_weight: [[[i16; 2]; MAX_RPS_DELTAS]; 2],
    /// `ChromaOffsetLX[i][j]`, derived per §7.4.7.3 (NOT the coded delta).
    pub chroma_offset: [[[i16; 2]; MAX_RPS_DELTAS]; 2],
}

impl Default for PredWeightTable {
    fn default() -> Self {
        Self {
            luma_denom: 0,
            chroma_denom: 0,
            luma_weight: [[0; MAX_RPS_DELTAS]; 2],
            luma_offset: [[0; MAX_RPS_DELTAS]; 2],
            chroma_weight: [[[0; 2]; MAX_RPS_DELTAS]; 2],
            chroma_offset: [[[0; 2]; MAX_RPS_DELTAS]; 2],
        }
    }
}

fn parse_pred_weight_table(
    r: &mut RbspReader<'_>,
    chroma_array_type: u8,
    l0: u8,
    l1: u8,
    bit_depth_chroma: u8,
    mut out: Option<&mut PredWeightTable>,
) -> Option<()> {
    let luma_denom = u8::try_from(r.ue()?).ok()?;
    // A denominator above 7 is out of range for the 3-bit field every back end
    // packs it into, and H.265 bounds it to 0..=7.
    if luma_denom > 7 {
        return None;
    }
    let mut chroma_denom = 0u8;
    if chroma_array_type != 0 {
        let d = i32::from(luma_denom) + r.se()?;
        if !(0..=7).contains(&d) {
            return None;
        }
        chroma_denom = d as u8;
    }
    // Chroma offsets are derived against a half-range that depends on the
    // chroma bit depth [H.265 §7.4.7.3]. For 8-bit this is 128.
    let half_range_c: i32 = 1 << (i32::from(bit_depth_chroma) - 1);
    if let Some(w) = out.as_deref_mut() {
        w.luma_denom = luma_denom;
        w.chroma_denom = chroma_denom;
        // Absent flags mean the DEFAULT weight (1 << denom) and zero offset,
        // not zero weight — a zero weight would black out the prediction.
        for list in 0..2 {
            for i in 0..MAX_RPS_DELTAS {
                w.luma_weight[list][i] = 1i16 << luma_denom;
                w.chroma_weight[list][i] = [1i16 << chroma_denom; 2];
            }
        }
    }
    // The bitstream order is: ALL luma flags, then ALL chroma flags, then the
    // deltas — not interleaved per entry. Read the flags into fixed arrays
    // first (num_ref_idx active is ≤ 16), then the deltas. The presence
    // condition on each flag (a reference at a different layer or POC) is
    // always true for the single-layer streams this decoder handles, so the
    // flag is taken as always present.
    for (list, num) in [l0, l1].into_iter().enumerate() {
        let n = (num as usize).min(MAX_RPS_DELTAS);
        let mut luma_flags = [false; MAX_RPS_DELTAS];
        let mut chroma_flags = [false; MAX_RPS_DELTAS];
        for f in luma_flags.iter_mut().take(n) {
            *f = r.flag()?;
        }
        if chroma_array_type != 0 {
            for f in chroma_flags.iter_mut().take(n) {
                *f = r.flag()?;
            }
        }
        for i in 0..n {
            if luma_flags[i] {
                let delta = r.se()?;
                let offset = r.se()?;
                if let Some(w) = out.as_deref_mut() {
                    w.luma_weight[list][i] = i16::try_from((1i32 << luma_denom) + delta).ok()?;
                    w.luma_offset[list][i] = i16::try_from(offset).ok()?;
                }
            }
            if chroma_flags[i] {
                for j in 0..2 {
                    let delta_w = r.se()?;
                    let delta_o = r.se()?;
                    if let Some(w) = out.as_deref_mut() {
                        let weight = (1i32 << chroma_denom) + delta_w;
                        w.chroma_weight[list][i][j] = i16::try_from(weight).ok()?;
                        // ChromaOffsetLX = Clip3(-half, half-1,
                        //   half + delta - ((half * weight) >> chroma_denom))
                        // [H.265 §7.4.7.3]. The subtracted term is why the
                        // coded delta cannot be handed to hardware directly.
                        let derived = half_range_c + delta_o
                            - ((half_range_c * i32::from(w.chroma_weight[list][i][j]))
                                >> chroma_denom);
                        let clipped = derived.clamp(-half_range_c, half_range_c - 1);
                        w.chroma_offset[list][i][j] = i16::try_from(clipped).ok()?;
                    }
                }
            }
        }
    }
    Some(())
}

/// Picture order count tracking across a coded video sequence.
///
/// POC is how HEVC expresses presentation order, and it is derived rather
/// than transmitted: the slice header carries only the low bits, and the high
/// bits are inferred from the previous picture. That inference is stateful, so
/// it cannot live in the slice parser.
///
/// A backend needs POC for the DPB and for output reordering; without it, a
/// stream with B-frames presents in decode order, which looks like stuttering
/// rather than an error.
#[derive(Clone, Copy, Debug, Default)]
pub struct PocTracker {
    prev_poc_lsb: u32,
    prev_poc_msb: u32,
    have_prev: bool,
}

impl PocTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_poc_lsb: 0,
            prev_poc_msb: 0,
            have_prev: false,
        }
    }

    /// Reset at a new coded video sequence.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Compute `PicOrderCntVal` for a picture.
    ///
    /// `temporal_id` and `sub_layer_non_reference` decide whether this picture
    /// becomes the reference for the next derivation: only a TemporalId 0
    /// picture that is not a sub-layer non-reference, RASL or RADL picture
    /// updates the state.
    pub fn compute(
        &mut self,
        nal_type: NalType,
        poc_lsb: u16,
        log2_max_poc_lsb: u8,
        temporal_id: u8,
    ) -> i32 {
        let max_lsb = 1u32 << log2_max_poc_lsb;
        let lsb = u32::from(poc_lsb);

        // With no previous picture there is nothing to infer from, and the
        // first picture of a sequence is an IRAP at POC 0 anyway.
        let poc_msb = if self.have_prev {
            let prev_lsb = self.prev_poc_lsb;
            let prev_msb = self.prev_poc_msb;
            let half = max_lsb / 2;
            if lsb < prev_lsb && (prev_lsb - lsb) >= half {
                prev_msb.wrapping_add(max_lsb)
            } else if lsb > prev_lsb && (lsb - prev_lsb) > half {
                prev_msb.wrapping_sub(max_lsb)
            } else {
                prev_msb
            }
        } else {
            0
        };

        let poc = poc_msb.wrapping_add(lsb) as i32;

        // IDR resets the sequence: its POC is 0 and it becomes the anchor.
        if matches!(nal_type, NalType::IdrWRadl | NalType::IdrNLp) {
            self.prev_poc_lsb = 0;
            self.prev_poc_msb = 0;
            self.have_prev = true;
            return 0;
        }
        if temporal_id == 0 {
            self.prev_poc_lsb = lsb;
            self.prev_poc_msb = poc_msb;
            self.have_prev = true;
        }
        poc
    }
}

/// The `hvcC` configuration record, as Matroska CodecPrivate and ISOBMFF
/// carry it. HEVC's counterpart to `avcC`.
#[derive(Clone, Copy, Debug)]
pub struct HvcC<'a> {
    /// Bytes per NAL length prefix in sample data (1, 2 or 4).
    pub nal_length_size: u8,
    /// First VPS, SPS and PPS found. Multiple sets per record are legal but
    /// our encode path does not produce them.
    pub vps: &'a [u8],
    pub sps: &'a [u8],
    pub pps: &'a [u8],
}

/// Parse an `hvcC` record.
///
/// Layout: a 22-byte fixed header, then `numOfArrays`, then per array a type
/// byte, a 16-bit NAL count, and each NAL as a 16-bit length plus payload.
#[must_use]
pub fn parse_hvcc(rec: &[u8]) -> Option<HvcC<'_>> {
    if rec.len() < 23 || rec[0] != 1 {
        return None;
    }
    let nal_length_size = (rec[21] & 0x03) + 1;
    let num_arrays = rec[22] as usize;

    let mut off = 23usize;
    let (mut vps, mut sps, mut pps): (&[u8], &[u8], &[u8]) = (&[], &[], &[]);

    for _ in 0..num_arrays {
        if off + 3 > rec.len() {
            return None;
        }
        let nal_type = rec[off] & 0x3F;
        let count = u16::from_be_bytes([rec[off + 1], rec[off + 2]]) as usize;
        off += 3;
        for i in 0..count {
            if off + 2 > rec.len() {
                return None;
            }
            let len = u16::from_be_bytes([rec[off], rec[off + 1]]) as usize;
            off += 2;
            if off + len > rec.len() {
                return None;
            }
            if i == 0 {
                let nal = &rec[off..off + len];
                match NalType::from_u8(nal_type) {
                    NalType::Vps => vps = nal,
                    NalType::Sps => sps = nal,
                    NalType::Pps => pps = nal,
                    _ => {}
                }
            }
            off += len;
        }
    }

    Some(HvcC {
        nal_length_size,
        vps,
        sps,
        pps,
    })
}

// ── Reference picture sets and list initialisation ──────────────────────────

/// The current picture's reference picture sets, as POC values.
///
/// `StCurrBefore` holds references earlier in presentation order, `StCurrAfter`
/// later. The split matters: L0 is initialised before-then-after and L1
/// after-then-before, which is what makes a B picture's two lists point in
/// opposite temporal directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefPicSets {
    before: [i32; MAX_RPS_DELTAS],
    n_before: u8,
    after: [i32; MAX_RPS_DELTAS],
    n_after: u8,
}

impl RefPicSets {
    #[must_use]
    pub fn st_curr_before(&self) -> &[i32] {
        &self.before[..self.n_before as usize]
    }

    #[must_use]
    pub fn st_curr_after(&self) -> &[i32] {
        &self.after[..self.n_after as usize]
    }

    /// `NumPicTotalCurr` — how many pictures the current one may reference.
    ///
    /// A P or B slice whose value is zero is malformed: it has references
    /// active but nothing to point at.
    #[must_use]
    pub const fn num_pic_total_curr(&self) -> usize {
        self.n_before as usize + self.n_after as usize
    }
}

/// Derive the current picture's reference picture sets from its RPS.
///
/// Only entries flagged `used_by_curr_pic` are included — the rest are held
/// in the DPB for a later picture and must not appear in a reference list.
#[must_use]
pub fn derive_ref_pic_sets(current_poc: i32, rps: &ShortTermRps) -> RefPicSets {
    let mut out = RefPicSets {
        before: [0; MAX_RPS_DELTAS],
        n_before: 0,
        after: [0; MAX_RPS_DELTAS],
        n_after: 0,
    };
    for i in 0..rps.num_negative as usize {
        if rps.used[i] {
            out.before[out.n_before as usize] = current_poc.wrapping_add(rps.delta_poc[i]);
            out.n_before += 1;
        }
    }
    let base = rps.num_negative as usize;
    for i in 0..rps.num_positive as usize {
        if rps.used[base + i] {
            out.after[out.n_after as usize] = current_poc.wrapping_add(rps.delta_poc[base + i]);
            out.n_after += 1;
        }
    }
    out
}

/// An initial reference picture list, as POC values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefPicList {
    entries: [i32; MAX_RPS_DELTAS],
    len: u8,
}

impl RefPicList {
    #[must_use]
    pub fn as_slice(&self) -> &[i32] {
        &self.entries[..self.len as usize]
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Build the initial `RefPicList0` and `RefPicList1`.
///
/// The temporary list is the concatenation — before-then-after for L0,
/// after-then-before for L1 — repeated until it is at least
/// `num_ref_idx_lX_active` long, then truncated to exactly that. The repetition
/// is not a quirk: a stream may declare more active references than it has
/// distinct pictures, and the standard requires the list wrap rather than be
/// short.
///
/// These are the INITIAL lists. A stream with `lists_modification_present` in
/// its PPS reorders them afterwards, and that syntax is not parsed yet — see
/// `parse_slice_header`.
#[must_use]
pub fn init_ref_pic_lists(
    sets: &RefPicSets,
    slice_type: SliceType,
    num_ref_idx_l0_active: u8,
    num_ref_idx_l1_active: u8,
) -> (RefPicList, RefPicList) {
    let empty = RefPicList {
        entries: [0; MAX_RPS_DELTAS],
        len: 0,
    };
    if slice_type == SliceType::I {
        return (empty, empty);
    }

    let build = |first: &[i32], second: &[i32], want: u8| -> RefPicList {
        let mut list = RefPicList {
            entries: [0; MAX_RPS_DELTAS],
            len: 0,
        };
        let total = first.len() + second.len();
        if total == 0 || want == 0 {
            return list;
        }
        let want = core::cmp::min(want as usize, MAX_RPS_DELTAS);
        let mut i = 0usize;
        while (list.len as usize) < want {
            let src = if i % total < first.len() {
                first[i % total]
            } else {
                second[(i % total) - first.len()]
            };
            list.entries[list.len as usize] = src;
            list.len += 1;
            i += 1;
        }
        list
    };

    let l0 = build(
        sets.st_curr_before(),
        sets.st_curr_after(),
        num_ref_idx_l0_active,
    );
    let l1 = if slice_type == SliceType::B {
        build(
            sets.st_curr_after(),
            sets.st_curr_before(),
            num_ref_idx_l1_active,
        )
    } else {
        empty
    };
    (l0, l1)
}

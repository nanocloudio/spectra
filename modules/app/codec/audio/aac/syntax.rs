//! Bitstream syntax of an AAC-LC raw data block [spec r01 §3, §4, §6].
//!
//! Parsing fills a [`Channel`] per audio channel: window information, the
//! codebook and scalefactor of every band, the TNS filters, and the quantised
//! spectral coefficients in their natural layout. Nothing here reconstructs
//! audio; the tools in `tools.rs` and the filterbank in `filterbank.rs` do
//! that from what is parsed. Every limit checked is one §10 lists.

use super::bits::BitReader;
use super::tables::{self, Codebook, HCB_SF, HCB_SIGNED, HCB_SPECTRAL, TNS_COEF_3, TNS_COEF_4};
use super::Fault;

/// Bands per window: at most 51 long, 15 short [spec r01 §10].
pub const MAX_BANDS: usize = 51;
/// Window groups: at most 8 (one per short window) [spec r01 §4.1].
pub const MAX_GROUPS: usize = 8;
/// TNS filters per channel: 3 in the one long window, or 1 in each of 8
/// short windows [spec r01 §4.5].
pub const MAX_TNS_FILTERS: usize = 8;
/// TNS filter order accepted [spec r01 §4.5, §10].
pub const TNS_MAX_ORDER: usize = 20;

/// Window sequences [spec r01 §4.1].
pub const SEQ_LONG: u8 = 0;
pub const SEQ_LONG_START: u8 = 1;
pub const SEQ_EIGHT_SHORT: u8 = 2;
pub const SEQ_LONG_STOP: u8 = 3;

/// Section codebook numbers with no spectral data [spec r01 §4.2].
pub const CB_ZERO: u8 = 0;
pub const CB_NOISE: u8 = 13;
pub const CB_INTENSITY_OUT: u8 = 14;
pub const CB_INTENSITY_IN: u8 = 15;

/// Element ids [spec r01 §3.1].
pub const ID_SCE: u32 = 0;
pub const ID_CPE: u32 = 1;
pub const ID_CCE: u32 = 2;
pub const ID_LFE: u32 = 3;
pub const ID_DSE: u32 = 4;
pub const ID_PCE: u32 = 5;
pub const ID_FIL: u32 = 6;
pub const ID_END: u32 = 7;

/// The band tables and limits one sampling-frequency index selects
/// [spec r01 T1, T3, T4, T17].
#[derive(Clone, Copy)]
pub struct Rate {
    pub index: u8,
    pub long: &'static [u16],
    pub short: &'static [u16],
    pub tns_long: u8,
    pub tns_short: u8,
}

impl Rate {
    pub fn for_index(index: u8) -> Option<Self> {
        if index > 12 {
            return None;
        }
        let i = index as usize;
        Some(Self {
            index,
            long: tables::BANDS_LONG[tables::LONG_TABLE_OF[i] as usize],
            short: tables::BANDS_SHORT[tables::SHORT_TABLE_OF[i] as usize],
            tns_long: tables::TNS_MAX_BAND_LONG[i],
            tns_short: tables::TNS_MAX_BAND_SHORT[i],
        })
    }

    pub fn hz(&self) -> u32 {
        tables::SAMPLE_RATES[self.index as usize]
    }

    /// Band start table and band count for a window sequence.
    pub fn bands(&self, sequence: u8) -> (&'static [u16], usize) {
        if sequence == SEQ_EIGHT_SHORT {
            (self.short, self.short.len() - 1)
        } else {
            (self.long, self.long.len() - 1)
        }
    }

    pub fn tns_limit(&self, sequence: u8) -> usize {
        if sequence == SEQ_EIGHT_SHORT {
            self.tns_short as usize
        } else {
            self.tns_long as usize
        }
    }
}

/// Window information and grouping [spec r01 §4.1].
#[derive(Clone, Copy)]
#[repr(C)]
pub struct WindowInfo {
    pub sequence: u8,
    pub shape: u8,
    pub max_band: u8,
    pub num_groups: u8,
    /// Windows in each group; sums to 8 for the eight-short sequence, 1 else.
    pub group_len: [u8; MAX_GROUPS],
}

impl WindowInfo {
    pub const fn zero() -> Self {
        Self {
            sequence: 0,
            shape: 0,
            max_band: 0,
            num_groups: 1,
            group_len: [1, 0, 0, 0, 0, 0, 0, 0],
        }
    }

    pub const fn num_windows(&self) -> usize {
        if self.sequence == SEQ_EIGHT_SHORT {
            8
        } else {
            1
        }
    }
}

/// One TNS filter [spec r01 §4.5], coefficients already mapped to reflection
/// coefficients [spec r01 §7.6].
#[derive(Clone, Copy)]
#[repr(C)]
pub struct TnsFilter {
    pub window: u8,
    pub length: u8,
    pub order: u8,
    pub direction: u8,
    pub coef: [f32; TNS_MAX_ORDER],
}

impl TnsFilter {
    pub const fn zero() -> Self {
        Self {
            window: 0,
            length: 0,
            order: 0,
            direction: 0,
            coef: [0.0; TNS_MAX_ORDER],
        }
    }
}

/// Everything parsed for one audio channel of a raw data block, plus its
/// spectrum: quantised integers after parsing, real coefficients after
/// dequantisation, in the natural layout (`window * 128 + k` for short
/// windows) [spec r01 §4.7].
#[repr(C)]
pub struct Channel {
    pub info: WindowInfo,
    pub global_gain: u8,
    pub tns_count: u8,
    /// Codebook of every band per group [spec r01 §4.2].
    pub band_cb: [[u8; MAX_BANDS]; MAX_GROUPS],
    /// Scalefactor, intensity position or noise energy per band [spec r01 §4.3].
    pub sf: [[i16; MAX_BANDS]; MAX_GROUPS],
    pub tns: [TnsFilter; MAX_TNS_FILTERS],
    pub spectrum: [f32; 1024],
}

impl Channel {
    pub const fn zero() -> Self {
        Self {
            info: WindowInfo::zero(),
            global_gain: 0,
            tns_count: 0,
            band_cb: [[0; MAX_BANDS]; MAX_GROUPS],
            sf: [[0; MAX_BANDS]; MAX_GROUPS],
            tns: [TnsFilter::zero(); MAX_TNS_FILTERS],
            spectrum: [0.0; 1024],
        }
    }
}

/// The M/S mask of a channel pair [spec r01 §3.3].
#[repr(C)]
pub struct MsInfo {
    /// 0 none, 1 per band, 2 all bands.
    pub mode: u8,
    pub used: [[u8; MAX_BANDS]; MAX_GROUPS],
}

impl MsInfo {
    pub const fn zero() -> Self {
        Self {
            mode: 0,
            used: [[0; MAX_BANDS]; MAX_GROUPS],
        }
    }
}

/// Read one codeword of `book` and return its row index [spec r01 §6.1].
fn huff(br: &mut BitReader<'_>, book: &Codebook) -> Result<usize, Fault> {
    let mut node = 0usize;
    loop {
        let bit = usize::from(br.bit()?);
        let next = book.nodes[node][bit];
        if next & 0x8000 != 0 {
            return Ok(usize::from(next & 0x7FFF));
        }
        node = usize::from(next);
    }
}

/// One scalefactor-codebook difference, −60..+60 [spec r01 §4.3].
fn sf_delta(br: &mut BitReader<'_>) -> Result<i16, Fault> {
    Ok(i16::from(HCB_SF.values[huff(br, &HCB_SF)?]))
}

/// `window_info()` [spec r01 §4.1].
pub fn parse_window_info(br: &mut BitReader<'_>, rate: &Rate) -> Result<WindowInfo, Fault> {
    if br.bit()? {
        return Err(Fault::Malformed);
    }
    let sequence = br.read(2)? as u8;
    let shape = br.read(1)? as u8;
    let mut info = WindowInfo {
        sequence,
        shape,
        max_band: 0,
        num_groups: 1,
        group_len: [0; MAX_GROUPS],
    };
    if sequence == SEQ_EIGHT_SHORT {
        info.max_band = br.read(4)? as u8;
        let grouping = br.read(7)?;
        // Bit 6 is window 1; a set bit joins the previous group.
        let mut groups = 0usize;
        info.group_len[0] = 1;
        for w in 1..8 {
            if (grouping >> (7 - w)) & 1 == 1 {
                info.group_len[groups] += 1;
            } else {
                groups += 1;
                info.group_len[groups] = 1;
            }
        }
        info.num_groups = groups as u8 + 1;
    } else {
        info.max_band = br.read(6)? as u8;
        if br.bit()? {
            // Main-profile prediction: not an LC stream [spec r01 §4.1].
            return Err(Fault::NotLc);
        }
        info.group_len[0] = 1;
    }
    let (_, num_bands) = rate.bands(sequence);
    if usize::from(info.max_band) > num_bands {
        return Err(Fault::Malformed);
    }
    Ok(info)
}

/// `section_data()` into `ch.band_cb` [spec r01 §4.2].
fn parse_sections(br: &mut BitReader<'_>, ch: &mut Channel) -> Result<(), Fault> {
    let short = ch.info.sequence == SEQ_EIGHT_SHORT;
    let (len_bits, escape) = if short { (3, 7) } else { (5, 31) };
    let max_band = usize::from(ch.info.max_band);
    for g in 0..usize::from(ch.info.num_groups) {
        let mut b = 0usize;
        while b < max_band {
            let cb = br.read(4)? as u8;
            if cb == 12 {
                return Err(Fault::Malformed);
            }
            let mut run = 0usize;
            loop {
                let incr = br.read(len_bits)? as usize;
                run += incr;
                if incr != escape {
                    break;
                }
            }
            if b + run > max_band {
                return Err(Fault::Malformed);
            }
            for band in &mut ch.band_cb[g][b..b + run] {
                *band = cb;
            }
            b += run;
        }
    }
    Ok(())
}

/// `scale_factor_data()` into `ch.sf` [spec r01 §4.3].
fn parse_scalefactors(br: &mut BitReader<'_>, ch: &mut Channel) -> Result<(), Fault> {
    let mut sf = i16::from(ch.global_gain);
    let mut pos: i16 = 0;
    let mut nrg = i16::from(ch.global_gain) - 90;
    let mut noise_first = true;
    for g in 0..usize::from(ch.info.num_groups) {
        for b in 0..usize::from(ch.info.max_band) {
            ch.sf[g][b] = match ch.band_cb[g][b] {
                CB_ZERO => 0,
                CB_INTENSITY_OUT | CB_INTENSITY_IN => {
                    pos = pos.saturating_add(sf_delta(br)?);
                    pos
                }
                CB_NOISE => {
                    if noise_first {
                        noise_first = false;
                        nrg = nrg.saturating_add(br.read(9)? as i16 - 256);
                    } else {
                        nrg = nrg.saturating_add(sf_delta(br)?);
                    }
                    nrg
                }
                _ => {
                    sf += sf_delta(br)?;
                    if !(0..=255).contains(&sf) {
                        return Err(Fault::Malformed);
                    }
                    sf
                }
            };
        }
    }
    Ok(())
}

/// `pulse_data()` [spec r01 §4.4].
#[derive(Clone, Copy)]
struct Pulse {
    count: usize,
    start_band: usize,
    offset: [u8; 4],
    amplitude: [u8; 4],
}

fn parse_pulse(br: &mut BitReader<'_>, ch: &Channel, rate: &Rate) -> Result<Pulse, Fault> {
    if ch.info.sequence == SEQ_EIGHT_SHORT {
        return Err(Fault::Malformed);
    }
    let count = br.read(2)? as usize + 1;
    let start_band = br.read(6)? as usize;
    let (_, num_bands) = rate.bands(ch.info.sequence);
    if start_band >= num_bands {
        return Err(Fault::Malformed);
    }
    let mut p = Pulse {
        count,
        start_band,
        offset: [0; 4],
        amplitude: [0; 4],
    };
    for i in 0..count {
        p.offset[i] = br.read(5)? as u8;
        p.amplitude[i] = br.read(4)? as u8;
    }
    Ok(p)
}

/// Pulse application on the quantised coefficients [spec r01 §7.2].
fn apply_pulse(ch: &mut Channel, rate: &Rate, p: &Pulse) -> Result<(), Fault> {
    let mut k = usize::from(rate.long[p.start_band]);
    for i in 0..p.count {
        k += usize::from(p.offset[i]);
        if k >= 1024 {
            return Err(Fault::Malformed);
        }
        let a = f32::from(p.amplitude[i]);
        let q = &mut ch.spectrum[k];
        if *q > 0.0 {
            *q += a;
        } else {
            *q -= a;
        }
    }
    Ok(())
}

/// `tns_data()` into `ch.tns` with coefficients mapped [spec r01 §4.5, §7.6].
fn parse_tns(br: &mut BitReader<'_>, ch: &mut Channel) -> Result<(), Fault> {
    let short = ch.info.sequence == SEQ_EIGHT_SHORT;
    let (n_windows, w_nfilt, w_length, w_order) = if short { (8, 1, 4, 3) } else { (1, 2, 6, 5) };
    let mut count = 0usize;
    for w in 0..n_windows {
        let n_filters = br.read(w_nfilt)? as usize;
        let resolution = if n_filters > 0 { br.read(1)? } else { 0 };
        for _ in 0..n_filters {
            if count >= MAX_TNS_FILTERS {
                return Err(Fault::Malformed);
            }
            let length = br.read(w_length)? as u8;
            let order = br.read(w_order)? as usize;
            if order > TNS_MAX_ORDER {
                return Err(Fault::Malformed);
            }
            let f = &mut ch.tns[count];
            *f = TnsFilter::zero();
            f.window = w as u8;
            f.length = length;
            f.order = order as u8;
            if order > 0 {
                f.direction = br.read(1)? as u8;
                let compress = br.read(1)?;
                let nominal = 3 + resolution;
                let bits = nominal - compress;
                let table: &[f32] = if nominal == 3 {
                    &TNS_COEF_3
                } else {
                    &TNS_COEF_4
                };
                for c in f.coef.iter_mut().take(order) {
                    // Two's-complement value of `bits` bits, masked to the
                    // nominal resolution to index the table.
                    let raw = br.read(bits)?;
                    let signed = if raw >= 1 << (bits - 1) {
                        raw as i32 - (1 << bits)
                    } else {
                        raw as i32
                    };
                    *c = table[(signed & ((1 << nominal) - 1)) as usize];
                }
            }
            count += 1;
        }
    }
    ch.tns_count = count as u8;
    Ok(())
}

/// Decode the values of one codeword of spectral codebook `cb` into `out`
/// (quads or pairs), with sign bits and escapes [spec r01 §4.7].
fn spectral_tuple(br: &mut BitReader<'_>, cb: u8, out: &mut [f32; 4]) -> Result<usize, Fault> {
    let book = HCB_SPECTRAL[usize::from(cb)];
    let dim = usize::from(book.dim);
    let row = huff(br, book)?;
    let values = &book.values[row * dim..row * dim + dim];
    for (o, &v) in out.iter_mut().zip(values) {
        *o = f32::from(v);
    }
    if !HCB_SIGNED[usize::from(cb)] {
        for o in out.iter_mut().take(dim) {
            if *o != 0.0 && br.bit()? {
                *o = -*o;
            }
        }
    }
    if cb == 11 {
        for o in out.iter_mut().take(dim) {
            if *o == 16.0 || *o == -16.0 {
                let mut n = 0u32;
                while br.bit()? {
                    n += 1;
                    if n > 8 {
                        return Err(Fault::Malformed);
                    }
                }
                let v = br.read(n + 4)?;
                let mag = ((1u32 << (n + 4)) + v) as f32;
                *o = if *o < 0.0 { -mag } else { mag };
            }
        }
    }
    Ok(dim)
}

/// `spectral_data()` into `ch.spectrum` as quantised integers in the natural
/// layout [spec r01 §4.7].
fn parse_spectral(br: &mut BitReader<'_>, ch: &mut Channel, rate: &Rate) -> Result<(), Fault> {
    ch.spectrum = [0.0; 1024];
    let (starts, _) = rate.bands(ch.info.sequence);
    let short = ch.info.sequence == SEQ_EIGHT_SHORT;
    let mut tuple = [0.0f32; 4];
    let mut window0 = 0usize;
    for g in 0..usize::from(ch.info.num_groups) {
        let windows = usize::from(ch.info.group_len[g]);
        for b in 0..usize::from(ch.info.max_band) {
            let cb = ch.band_cb[g][b];
            if cb == CB_ZERO || cb >= CB_NOISE {
                continue;
            }
            let (lo, hi) = (usize::from(starts[b]), usize::from(starts[b + 1]));
            let windows_here = if short { windows } else { 1 };
            for w in 0..windows_here {
                let base = if short { (window0 + w) * 128 } else { 0 };
                let mut k = lo;
                while k < hi {
                    let n = spectral_tuple(br, cb, &mut tuple)?;
                    ch.spectrum[base + k..base + k + n].copy_from_slice(&tuple[..n]);
                    k += n;
                }
            }
        }
        window0 += windows;
    }
    Ok(())
}

/// `channel_stream()` [spec r01 §4]. With `common`, the window info is the
/// pair's shared one and is not read here.
pub fn parse_channel_stream(
    br: &mut BitReader<'_>,
    rate: &Rate,
    ch: &mut Channel,
    common: Option<&WindowInfo>,
) -> Result<(), Fault> {
    ch.global_gain = br.read(8)? as u8;
    ch.info = match common {
        Some(info) => *info,
        None => parse_window_info(br, rate)?,
    };
    ch.band_cb = [[0; MAX_BANDS]; MAX_GROUPS];
    ch.tns_count = 0;
    parse_sections(br, ch)?;
    parse_scalefactors(br, ch)?;
    let pulse = if br.bit()? {
        Some(parse_pulse(br, ch, rate)?)
    } else {
        None
    };
    if br.bit()? {
        parse_tns(br, ch)?;
    }
    if br.bit()? {
        // Gain control: SSR only [spec r01 §4.6].
        return Err(Fault::NotLc);
    }
    parse_spectral(br, ch, rate)?;
    if let Some(p) = pulse {
        apply_pulse(ch, rate, &p)?;
    }
    Ok(())
}

/// The M/S data of a channel pair [spec r01 §3.3].
pub fn parse_ms(br: &mut BitReader<'_>, info: &WindowInfo, ms: &mut MsInfo) -> Result<(), Fault> {
    ms.mode = br.read(2)? as u8;
    ms.used = [[0; MAX_BANDS]; MAX_GROUPS];
    match ms.mode {
        0 => {}
        1 => {
            for g in 0..usize::from(info.num_groups) {
                for b in 0..usize::from(info.max_band) {
                    ms.used[g][b] = br.read(1)? as u8;
                }
            }
        }
        2 => {
            for g in 0..usize::from(info.num_groups) {
                for b in 0..usize::from(info.max_band) {
                    ms.used[g][b] = 1;
                }
            }
        }
        _ => return Err(Fault::Malformed),
    }
    Ok(())
}

/// Coupling channel element: parsed to stay in sync, then discarded
/// [spec r01 §3.4]. `scratch` receives the coupling channel's stream.
pub fn parse_cce(br: &mut BitReader<'_>, rate: &Rate, scratch: &mut Channel) -> Result<(), Fault> {
    let _tag = br.read(4)?;
    let independent = br.bit()?;
    let num_targets = br.read(3)? as usize + 1;
    let mut gain_lists = 0usize;
    for _ in 0..num_targets {
        gain_lists += 1;
        let target_is_cpe = br.bit()?;
        let _target_tag = br.read(4)?;
        if target_is_cpe {
            let l = br.bit()?;
            let r = br.bit()?;
            if l && r {
                gain_lists += 1;
            }
        }
    }
    let _domain = br.bit()?;
    let _gain_sign = br.bit()?;
    let _gain_scale = br.read(2)?;
    parse_channel_stream(br, rate, scratch, None)?;
    for _ in 1..gain_lists {
        let common_gain = if independent { true } else { br.bit()? };
        if common_gain {
            sf_delta(br)?;
        } else {
            for g in 0..usize::from(scratch.info.num_groups) {
                for b in 0..usize::from(scratch.info.max_band) {
                    if scratch.band_cb[g][b] != CB_ZERO {
                        sf_delta(br)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Data stream element: skipped [spec r01 §3.5].
pub fn skip_dse(br: &mut BitReader<'_>) -> Result<(), Fault> {
    let _tag = br.read(4)?;
    let aligned = br.bit()?;
    let mut count = br.read(8)? as usize;
    if count == 255 {
        count += br.read(8)? as usize;
    }
    if aligned {
        br.align()?;
    }
    br.skip(count * 8)
}

/// Fill element: the body is skipped whole [spec r01 §3.7].
pub fn skip_fil(br: &mut BitReader<'_>) -> Result<(), Fault> {
    let mut count = br.read(4)? as usize;
    if count == 15 {
        count += br.read(8)? as usize;
        count -= 1;
    }
    br.skip(count * 8)
}

/// Program configuration element, parsed to stay in sync [spec r01 §3.6].
/// Returns the number of output channels it declares.
pub fn parse_pce(br: &mut BitReader<'_>) -> Result<usize, Fault> {
    let _tag = br.read(4)?;
    let _object_type = br.read(2)?;
    let _sampling_index = br.read(4)?;
    let n_front = br.read(4)? as usize;
    let n_side = br.read(4)? as usize;
    let n_back = br.read(4)? as usize;
    let n_lfe = br.read(2)? as usize;
    let n_assoc = br.read(3)? as usize;
    let n_coupling = br.read(4)? as usize;
    if br.bit()? {
        br.read(4)?;
    }
    if br.bit()? {
        br.read(4)?;
    }
    if br.bit()? {
        br.read(3)?;
    }
    let mut channels = 0usize;
    for _ in 0..n_front + n_side + n_back {
        let is_cpe = br.bit()?;
        br.read(4)?;
        channels += if is_cpe { 2 } else { 1 };
    }
    for _ in 0..n_lfe {
        br.read(4)?;
        channels += 1;
    }
    for _ in 0..n_assoc {
        br.read(4)?;
    }
    for _ in 0..n_coupling {
        br.read(5)?;
    }
    br.align()?;
    let comment_len = br.read(8)? as usize;
    br.skip(comment_len * 8)?;
    Ok(channels)
}

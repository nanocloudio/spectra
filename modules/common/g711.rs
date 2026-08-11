//! ITU-T G.711 µ-law companding — the narrowband telephony audio core.
//!
//! Design and rationale mirror the other cores in this crate: pure algorithm,
//! `no_std`, no I/O, no allocation, no Fluxor ABI. The µ-law codec is a byte
//! ↔ 14-bit-companded-sample mapping and nothing more — sample rate, channel
//! layout, packetisation and jitter handling are all caller concerns and stay
//! in the composing module (Conclave voice / Fluxor VoIP glue), never here.
//!
//! Relocated from `fluxor/modules/app/voip/g711.rs` under Conclave plan S4.2
//! (`Move G.711 to Spectra`). The decode table is bit-identical to that origin.
//! The **encoder is corrected here**: the origin computed the segment by
//! counting bits above bit 7 of the biased magnitude, one segment high across
//! the whole range, so no codeword survived a decode→encode round trip and the
//! sign bit collapsed at full scale. This core uses the standard ITU segment
//! search, which is idempotent against the decode table (`encode(decode(c))`
//! returns `c` for all 255 non-redundant codewords; `0x7f` and `0xff` both
//! decode to zero and canonically re-encode to `0xff`). It therefore diverges
//! from the origin encoder *by design* — the parity relocation (S4.2) proved
//! the move, and this is the plan's follow-up semantic correction. The vectors
//! live in `tests/g711_vectors.rs`.
//!
//! # Boundary
//!
//! [`ulaw_encode`] takes one linear 16-bit PCM sample and returns its µ-law
//! octet; [`ulaw_decode`] inverts it through the standard expansion table.
//! Both are total functions with no failure mode. Anything that reads a
//! channel, mixes stereo to mono, or paces playout belongs in the module.

/// µ-law bias added to the magnitude before segment extraction (ITU-T G.711).
const ULAW_BIAS: i32 = 0x84;

/// µ-law clip level — magnitudes above this saturate before companding.
const ULAW_CLIP: i32 = 0x7F7B;

/// Upper magnitude bound of each of the eight µ-law segments (post-bias). The
/// segment is the index of the first bound the biased magnitude does not exceed.
const ULAW_SEG_END: [i32; 8] = [0xFF, 0x1FF, 0x3FF, 0x7FF, 0xFFF, 0x1FFF, 0x3FFF, 0x7FFF];

/// µ-law → linear-PCM expansion table (256 entries, ITU-T G.711).
///
/// Index by the received µ-law octet; the value is the reconstructed signed
/// 14-bit sample sign-extended into `i16`.
static ULAW_TO_LINEAR: [i16; 256] = [
    -32124, -31100, -30076, -29052, -28028, -27004, -25980, -24956, -23932, -22908, -21884, -20860,
    -19836, -18812, -17788, -16764, -15996, -15484, -14972, -14460, -13948, -13436, -12924, -12412,
    -11900, -11388, -10876, -10364, -9852, -9340, -8828, -8316, -7932, -7676, -7420, -7164, -6908,
    -6652, -6396, -6140, -5884, -5628, -5372, -5116, -4860, -4604, -4348, -4092, -3900, -3772,
    -3644, -3516, -3388, -3260, -3132, -3004, -2876, -2748, -2620, -2492, -2364, -2236, -2108,
    -1980, -1884, -1820, -1756, -1692, -1628, -1564, -1500, -1436, -1372, -1308, -1244, -1180,
    -1116, -1052, -988, -924, -876, -844, -812, -780, -748, -716, -684, -652, -620, -588, -556,
    -524, -492, -460, -428, -396, -372, -356, -340, -324, -308, -292, -276, -260, -244, -228, -212,
    -196, -180, -164, -148, -132, -120, -112, -104, -96, -88, -80, -72, -64, -56, -48, -40, -32,
    -24, -16, -8, 0, 32124, 31100, 30076, 29052, 28028, 27004, 25980, 24956, 23932, 22908, 21884,
    20860, 19836, 18812, 17788, 16764, 15996, 15484, 14972, 14460, 13948, 13436, 12924, 12412,
    11900, 11388, 10876, 10364, 9852, 9340, 8828, 8316, 7932, 7676, 7420, 7164, 6908, 6652, 6396,
    6140, 5884, 5628, 5372, 5116, 4860, 4604, 4348, 4092, 3900, 3772, 3644, 3516, 3388, 3260, 3132,
    3004, 2876, 2748, 2620, 2492, 2364, 2236, 2108, 1980, 1884, 1820, 1756, 1692, 1628, 1564, 1500,
    1436, 1372, 1308, 1244, 1180, 1116, 1052, 988, 924, 876, 844, 812, 780, 748, 716, 684, 652,
    620, 588, 556, 524, 492, 460, 428, 396, 372, 356, 340, 324, 308, 292, 276, 260, 244, 228, 212,
    196, 180, 164, 148, 132, 120, 112, 104, 96, 88, 80, 72, 64, 56, 48, 40, 32, 24, 16, 8, 0,
];

/// Expand one µ-law octet to its linear 16-bit PCM sample.
#[inline]
#[must_use]
pub fn ulaw_decode(byte: u8) -> i16 {
    ULAW_TO_LINEAR[byte as usize]
}

/// Compand one linear 16-bit PCM sample to its µ-law octet (ITU-T G.711).
#[inline]
#[must_use]
pub fn ulaw_encode(sample: i16) -> u8 {
    let sign: u8 = if sample < 0 { 0x80 } else { 0 };
    let mut mag = (sample as i32).abs();
    if mag > ULAW_CLIP {
        mag = ULAW_CLIP;
    }
    mag += ULAW_BIAS;

    // Segment = index of the first bound the biased magnitude does not exceed.
    let segment = ULAW_SEG_END.iter().position(|&end| mag <= end).unwrap_or(8);
    if segment >= 8 {
        return 0x7F ^ sign;
    }

    let quant = ((mag >> (segment + 3)) & 0x0F) as u8;
    !(sign | ((segment as u8) << 4) | quant)
}

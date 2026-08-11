//! Image essence family — the façade the codec root talks to, and the
//! chassis every image format shares.
//!
//! One file per format beside this one, each exposing a single
//! `<fmt>_decode(&mut ImageState) -> bool`: [`bmp`] (no decompression),
//! [`gif`] (LZW), [`png`] (DEFLATE + filters, via [`deflate`]), [`jpeg`]
//! (Huffman + IDCT + YCbCr). Everything else — the [`Phase`] machine,
//! channel I/O, the encoded accumulator, nearest-neighbour scaling, the
//! RGB565 output path, and error logging — lives here and is shared.
//!
//! The family contract the root dispatches through is `detect` / `init` /
//! `feed_detect` / `step` / `is_done`, the same five the `audio` and
//! `video` families expose.
//!
//! Memory layout (heap, via `syscalls.heap_alloc`):
//!   * `encoded[]` — accumulator for the encoded source bytes (up to
//!     `max_bytes`, 8 MiB default = 1920x1080 24-bit + slack)
//!   * `pending[]` — decoded RGB565 LE frame (width x height x 2)
//!
//! Both are freed and re-allocated when dimensions change, and allocated
//! lazily on first byte / first decode so audio-only deployments don't pay
//! the cost. Per-format scratch (LZW dictionary, JPEG tables, etc.) is
//! `heap_alloc`'d inside the decoder and freed before return.

use super::abi::SyscallTable;
use super::dev_log;

pub mod bmp;
pub mod deflate;
pub mod gif;
pub mod jpeg;
pub mod png;

// ── Format dispatch ───────────────────────────────────────────────────────

#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum ImageFormat {
    Bmp = 0,
    Gif = 1,
    Png = 2,
    Jpeg = 3,
}

// ── Format magics ─────────────────────────────────────────────────────────

pub const BMP_MAGIC: &[u8; 2] = b"BM";
pub const GIF_MAGIC: &[u8; 4] = b"GIF8"; // GIF87a / GIF89a
pub const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
pub const JPEG_MAGIC: &[u8; 3] = b"\xff\xd8\xff"; // SOI (FF D8) + first segment marker (FF xx)

/// Identify an image format from the first bytes of a stream, or `None`
/// when this family does not claim them.
///
/// Returning `None` is NOT the same as "need more bytes" — the root asks
/// each family in turn and commits on the first claim, so a family that
/// cannot yet tell must not claim. Every magic here is checked against
/// `buf.len()` for exactly that reason, and each format's own decoder
/// re-checks its magic before parsing: this is a fast happy-path commit,
/// not a validation boundary.
pub fn detect(buf: &[u8]) -> Option<ImageFormat> {
    if buf.len() >= BMP_MAGIC.len() && &buf[..2] == BMP_MAGIC.as_slice() {
        return Some(ImageFormat::Bmp);
    }
    if buf.len() >= GIF_MAGIC.len() && &buf[..4] == GIF_MAGIC.as_slice() {
        return Some(ImageFormat::Gif);
    }
    if buf.len() >= PNG_MAGIC.len() && &buf[..8] == PNG_MAGIC.as_slice() {
        return Some(ImageFormat::Png);
    }
    if buf.len() >= JPEG_MAGIC.len() && &buf[..3] == JPEG_MAGIC.as_slice() {
        return Some(ImageFormat::Jpeg);
    }
    None
}

/// Bytes of a file's head shown to [`detect`]. The longest magic is PNG's
/// 8-byte signature; 16 is headroom.
const DETECT_HEAD: u32 = 16;

const BMP_HEADER_LEN: usize = 54;
const IN_CHUNK: usize = 1024;
const WRITE_CHUNK: usize = 1024;
const EOF_TICKS: u32 = 200;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum Phase {
    Ingest = 0,
    Decoded = 1,
    Draining = 2,
    Error = 3,
}

// ── State ─────────────────────────────────────────────────────────────────

#[repr(C)]
pub struct ImageState {
    pub syscalls: *const SyscallTable,
    pub in_chan: i32,
    pub out_chan: i32, // pixels port

    // params (parent walks TLV into these before calling image_init)
    pub dst_w: u16,
    pub dst_h: u16,
    pub scale_mode: u8,
    _pad0: u8,
    pub max_bytes: u32,

    // accumulator
    pub(super) encoded: *mut u8,
    pub encoded_used: u32,
    encoded_cap: u32,

    // decoded RGB565 LE
    pub(super) pending: *mut u8,
    pub(super) pending_size: u32,
    pub(super) pending_pos: u32,

    // state
    pub phase: Phase,
    /// Set by the parent before `image_init` so `decode_buffer` knows
    /// which decompressor to dispatch to. Defaults to Bmp (0) so
    /// existing BMP fixtures keep working without explicit init.
    pub image_format: ImageFormat,
    /// Number of bytes in `last_err` (0 = no recorded error).
    pub(super) last_err_len: u8,
    _pad1: u8,
    /// Last decode-failure reason — sticky, surfaced via the parent's
    /// heartbeat so the rig telemetry sees the diagnosis even when
    /// log_net dropped the one-shot dev_log fired from inside
    /// `decode_buffer`.
    pub(super) last_err: [u8; 48],
    quiet_ticks: u32,
}

// ── Lifecycle ─────────────────────────────────────────────────────────────

pub unsafe fn image_init(
    s: &mut ImageState,
    syscalls: *const SyscallTable,
    in_chan: i32,
    out_chan: i32,
) {
    s.syscalls = syscalls;
    s.in_chan = in_chan;
    s.out_chan = out_chan;
    // params (dst_w / dst_h / scale_mode / max_bytes) are written by
    // the parent's TLV walker before image_init is called; we leave
    // them alone here. Buffers + phase are reset though.
    s.encoded = core::ptr::null_mut();
    s.encoded_used = 0;
    s.encoded_cap = 0;
    s.pending = core::ptr::null_mut();
    s.pending_size = 0;
    s.pending_pos = 0;
    s.phase = Phase::Ingest;
    s.quiet_ticks = 0;
    // `image_format` is staged by the parent's `init_codec` BEFORE
    // `image_init` is called, so we leave it untouched here.
    dev_log(&*syscalls, 3, b"[dec] image".as_ptr(), 11);
}

/// Replay the bytes the parent consumed during format detection so
/// the BMP header parser sees them. The parent feeds the
/// `detect_buf` bytes here before the first `image_step` call.
pub unsafe fn image_feed_detect(s: &mut ImageState, buf: *const u8, len: usize) {
    if !reserve_encoded(s, len) {
        log(s, b"[img] detect bytes overflow");
        s.phase = Phase::Error;
        return;
    }
    core::ptr::copy_nonoverlapping(buf, s.encoded.add(s.encoded_used as usize), len);
    s.encoded_used += len as u32;
}

#[allow(
    dead_code,
    reason = "target-conditional or kept for diagnostic use; the cfg-gated build path doesn't always reach it"
)]
pub unsafe fn image_is_done(s: &ImageState) -> bool {
    matches!(s.phase, Phase::Error)
        || (matches!(s.phase, Phase::Draining) && s.pending_pos >= s.pending_size)
}

// ── Step ──────────────────────────────────────────────────────────────────

pub unsafe fn image_step(s: &mut ImageState) -> i32 {
    match s.phase {
        Phase::Error => {
            // A bad header is permanent for the current encoded
            // buffer — there's nothing the codec can do to recover
            // from a malformed file by re-running the parser. Stay
            // in Error until the upstream serves NEW bytes (= bank
            // advanced to the next file), at which point reset to
            // Ingest and start accumulating again. The freshly-read
            // bytes are replayed into the encoded buffer so they
            // become the head of the next image.
            let mut scratch = [0u8; IN_CHUNK];
            let n = ((*s.syscalls).channel_read)(s.in_chan, scratch.as_mut_ptr(), scratch.len());
            if n > 0 {
                s.encoded_used = 0;
                s.quiet_ticks = 0;
                s.phase = Phase::Ingest;
                let nn = n as usize;
                if reserve_encoded(s, nn) {
                    core::ptr::copy_nonoverlapping(
                        scratch.as_ptr(),
                        s.encoded.add(s.encoded_used as usize),
                        nn,
                    );
                    s.encoded_used += nn as u32;
                } else {
                    s.phase = Phase::Error;
                }
            }
            0
        }

        Phase::Draining => {
            if s.pending_pos < s.pending_size {
                let take = (s.pending_size - s.pending_pos).min(WRITE_CHUNK as u32);
                let w = ((*s.syscalls).channel_write)(
                    s.out_chan,
                    s.pending.add(s.pending_pos as usize),
                    take as usize,
                );
                if w > 0 {
                    s.pending_pos += w as u32;
                }
                return 0;
            }
            // Fully drained — reset for the next image. Bank's
            // IOCTL_FLUSH on the next file lines this up.
            s.pending_pos = 0;
            s.encoded_used = 0;
            s.quiet_ticks = 0;
            s.phase = Phase::Ingest;
            0
        }

        Phase::Decoded => {
            s.phase = Phase::Draining;
            0
        }

        Phase::Ingest => {
            let mut chunk = [0u8; IN_CHUNK];
            let n = ((*s.syscalls).channel_read)(s.in_chan, chunk.as_mut_ptr(), chunk.len());
            if n > 0 {
                let n = n as usize;
                // Is this append the START of a file? Reads are up to IN_CHUNK
                // (1024 B), so "the head has arrived" cannot be tested by
                // looking at how much has accumulated afterwards.
                let starts_file = s.encoded_used == 0;
                if !reserve_encoded(s, n) {
                    log(s, b"[img] encoded buffer overflow");
                    s.phase = Phase::Error;
                    return 0;
                }
                core::ptr::copy_nonoverlapping(
                    chunk.as_ptr(),
                    s.encoded.add(s.encoded_used as usize),
                    n,
                );
                s.encoded_used += n as u32;
                s.quiet_ticks = 0;

                // Re-sniff the format from the head of EVERY file, not just the
                // first. A gallery is heterogeneous — the pi5 viewer serves BMP,
                // GIF, JPEG and PNG out of one directory — but the root commits
                // its `FMT_*` tag once, at the first detection, and never
                // revisits it: `owns_eof_cycle()` deliberately excludes images
                // from the parent's HUP reset (it would wipe a frame mid-drain),
                // and this family returns itself to `Ingest` when a drain
                // completes. So without this, file 2 was decoded by file 1's
                // decoder and produced nothing at all.
                //
                // The root's tag stays correct at the family level — every value
                // it can hold dispatches here — so format selection WITHIN the
                // family is this family's business, and doing it from the
                // accumulated head keeps the heap buffers alive across files
                // rather than resetting (and leaking) them.
                if starts_file {
                    let head = core::slice::from_raw_parts(
                        s.encoded,
                        (s.encoded_used as usize).min(DETECT_HEAD as usize),
                    );
                    if let Some(f) = detect(head) {
                        s.image_format = f;
                    }
                }
                return 0;
            }
            if s.encoded_used == 0 {
                return 0;
            }
            // End of image. Two ways to know, and the order matters.
            //
            // A latched HUP with the ring drained is a POSITIVE end-of-stream
            // from the producer: the file is complete, decode it now. The
            // video family has always done this; the image family did not,
            // and paid for it twice. It sat through `EOF_TICKS` of scheduler
            // time after every image it already had in full — and, worse, a
            // producer that merely STALLED for that long (a network fetch
            // hiccup mid-file) was indistinguishable from one that had
            // finished, so the decoder would batch-decode a truncated file
            // and land in `Phase::Error`.
            //
            // The quiet-tick count stays as the fallback, because not every
            // producer HUPs — a fixed-size local read may simply stop. It is
            // now the second answer rather than the only one.
            let poll = ((*s.syscalls).channel_poll)(s.in_chan, super::POLL_IN | super::POLL_HUP);
            let ended = (poll as u32) & super::POLL_HUP != 0 && (poll as u32) & super::POLL_IN == 0;
            if !ended {
                s.quiet_ticks = s.quiet_ticks.saturating_add(1);
                if s.quiet_ticks < EOF_TICKS {
                    return 0;
                }
            }
            if !decode_buffer(s) {
                s.phase = Phase::Error;
                return 0;
            }
            s.phase = Phase::Decoded;
            0
        }
    }
}

// ── Decode helpers ───────────────────────────────────────────────────────
//
// `pub(super)` so the sibling format files (`gif.rs`, `png.rs`,
// `jpeg.rs`) can share the encoded/pending/scaling chassis.

/// Ceiling for the encoded accumulator when `max_bytes` is unset.
const DEFAULT_MAX_ENCODED: u32 = 8 * 1024 * 1024;
/// Initial encoded allocation. `max_bytes` is a CEILING, not the size to
/// grab up front — the buffer grows toward it on demand (see
/// [`reserve_encoded`]). Eagerly allocating the full ceiling would starve
/// the module heap: a 16 MiB `max_bytes` against the codec's 10 MiB
/// `module_arena_size()` fails every `heap_alloc`, so the image never
/// decodes and downstream sinks (linux_display) get no frame.
const INITIAL_ENCODED_CAP: u32 = 256 * 1024;

#[inline]
fn encoded_ceiling(s: &ImageState) -> u32 {
    if s.max_bytes == 0 {
        DEFAULT_MAX_ENCODED
    } else {
        s.max_bytes
    }
}

pub(super) unsafe fn ensure_encoded(s: &mut ImageState) -> bool {
    if !s.encoded.is_null() {
        return true;
    }
    let cap = INITIAL_ENCODED_CAP.min(encoded_ceiling(s));
    let p = ((*s.syscalls).heap_alloc)(cap);
    if p.is_null() {
        log(s, b"[img] encoded alloc failed");
        return false;
    }
    s.encoded = p;
    s.encoded_cap = cap;
    true
}

/// Ensure `encoded` has room for `additional` more bytes past
/// `encoded_used`, growing the buffer (doubling, bounded by the
/// `max_bytes` ceiling) if needed. Returns false only when the total
/// would exceed the ceiling or a (re)allocation fails — so `max_bytes`
/// stays a hard limit while a small image never over-allocates.
pub(super) unsafe fn reserve_encoded(s: &mut ImageState, additional: usize) -> bool {
    if !ensure_encoded(s) {
        return false;
    }
    let ceiling = encoded_ceiling(s) as usize;
    let needed = s.encoded_used as usize + additional;
    if needed > ceiling {
        return false;
    }
    if needed <= s.encoded_cap as usize {
        return true;
    }
    let mut new_cap = s.encoded_cap as usize;
    while new_cap < needed {
        new_cap = new_cap.saturating_mul(2);
    }
    if new_cap > ceiling {
        new_cap = ceiling;
    }
    let p = ((*s.syscalls).heap_realloc)(s.encoded, new_cap as u32);
    if p.is_null() {
        log(s, b"[img] encoded grow failed");
        return false;
    }
    s.encoded = p;
    s.encoded_cap = new_cap as u32;
    true
}

#[inline(always)]
pub(super) fn le_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline(always)]
pub(super) fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline(always)]
pub(super) fn le_i32(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline(always)]
pub(super) fn be_u16(b: &[u8], off: usize) -> u16 {
    u16::from_be_bytes([b[off], b[off + 1]])
}

#[inline(always)]
pub(super) fn be_u32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Allocate or re-allocate `s.pending` to fit `dst_w × dst_h × 2`
/// bytes (RGB565 LE). Format decoders call this once per decode after
/// they have parsed their dimensions. Re-uses the buffer when the
/// size matches the previous decode.
pub(super) unsafe fn ensure_pending(s: &mut ImageState, dst_w: usize, dst_h: usize) -> bool {
    // `dst_w`/`dst_h` come from YAML params and are only bounded by u16, so
    // 65535x65535x2 is 8.6 GB — which WRAPS a 32-bit `usize` (rp2350, wasm32)
    // and would hand `heap_alloc` a small truncated size for a buffer every
    // format decoder then writes full-raster into. Compute the size where it
    // cannot wrap and refuse what will not fit an allocation request.
    let dst_bytes = match (dst_w as u64)
        .checked_mul(dst_h as u64)
        .and_then(|px| px.checked_mul(2))
    {
        Some(n) if n <= u32::MAX as u64 => n as usize,
        _ => {
            log(s, b"[img] output raster too large");
            return false;
        }
    };
    if s.pending.is_null() || s.pending_size as usize != dst_bytes {
        if !s.pending.is_null() {
            ((*s.syscalls).heap_free)(s.pending);
        }
        let p = ((*s.syscalls).heap_alloc)(dst_bytes as u32);
        if p.is_null() {
            log(s, b"[img] pending alloc failed");
            return false;
        }
        s.pending = p;
        s.pending_size = dst_bytes as u32;
    }
    s.pending_pos = 0;
    true
}

/// Pack a (r,g,b) triple (each 0..=255) into RGB565 little-endian
/// at `dst[off..off+2]`.
#[inline(always)]
pub(super) unsafe fn store_rgb565(dst: *mut u8, off: usize, r: u8, g: u8, b: u8) {
    let v = (((r as u16) >> 3) << 11) | (((g as u16) >> 2) << 5) | ((b as u16) >> 3);
    *dst.add(off) = (v & 0xFF) as u8;
    *dst.add(off + 1) = (v >> 8) as u8;
}

/// Decode `s.encoded[..s.encoded_used]` into `s.pending` (RGB565 LE).
/// Dispatch by `s.image_format`; format-specific decoders live in
/// their own files (`gif.rs`, `png.rs`, `image_jpeg.rs`)
/// and write straight into `s.pending` via the shared helpers below.
/// Returns false on any parse / format failure.
unsafe fn decode_buffer(s: &mut ImageState) -> bool {
    match s.image_format {
        ImageFormat::Bmp => bmp::bmp_decode(s),
        ImageFormat::Gif => gif::gif_decode(s),
        ImageFormat::Png => png::png_decode(s),
        ImageFormat::Jpeg => jpeg::jpeg_decode(s),
    }
}

// ── Logging helpers ──────────────────────────────────────────────────────

/// Emit a log line AND record it in `last_err` so the parent's
/// heartbeat can resurface the reason later. Used by per-format
/// decoders on every error branch — the original one-shot
/// `dev_log` from inside `decode_buffer` is often dropped by
/// log_net during early boot before its UDP stream is fully
/// draining, so the sticky copy is the reliable diagnosis path.
#[inline]
pub(super) unsafe fn log(s: &mut ImageState, msg: &[u8]) {
    dev_log(&*s.syscalls, 3, msg.as_ptr(), msg.len());
    let n = msg.len().min(s.last_err.len());
    s.last_err[..n].copy_from_slice(&msg[..n]);
    s.last_err_len = n as u8;
}

pub(super) unsafe fn log_dims(s: &ImageState, prefix: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) {
    let mut buf = [0u8; 96];
    let p = buf.as_mut_ptr();
    let mut q = 0usize;
    let mut t = 0;
    while t < prefix.len() {
        *p.add(q) = prefix[t];
        q += 1;
        t += 1;
    }
    q += super::fmt_u32_raw(p.add(q), sw);
    *p.add(q) = b'x';
    q += 1;
    q += super::fmt_u32_raw(p.add(q), sh);
    let arrow = b" -> ";
    let mut t = 0;
    while t < arrow.len() {
        *p.add(q) = arrow[t];
        q += 1;
        t += 1;
    }
    q += super::fmt_u32_raw(p.add(q), dw);
    *p.add(q) = b'x';
    q += 1;
    q += super::fmt_u32_raw(p.add(q), dh);
    dev_log(&*s.syscalls, 3, p, q);
}

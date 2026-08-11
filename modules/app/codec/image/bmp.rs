// BMP decoder — 24-bit uncompressed BI_RGB only.
//
// The simplest member of the image family: no entropy coding and no
// decompression, so it needs nothing from `deflate.rs`. Shares the
// family chassis in `mod.rs` (encoded accumulator, `pending[]` RGB565
// output, nearest-neighbour scaling, logging) exactly as GIF/PNG/JPEG do.

use super::super::scale::Nearest;
use super::{
    ensure_pending, le_i32, le_u16, le_u32, log, log_dims, store_rgb565, ImageState,
    BMP_HEADER_LEN, BMP_MAGIC,
};

/// BMP-specific decoder (24-bit uncompressed BI_RGB only).
pub unsafe fn bmp_decode(s: &mut ImageState) -> bool {
    let buf = core::slice::from_raw_parts(s.encoded, s.encoded_used as usize);
    if buf.len() < BMP_HEADER_LEN {
        log(s, b"[img] truncated header");
        return false;
    }
    if buf[0] != BMP_MAGIC[0] || buf[1] != BMP_MAGIC[1] {
        log(s, b"[img] missing BM magic");
        return false;
    }
    let pixel_offset = le_u32(buf, 10) as usize;
    let src_w = le_i32(buf, 18);
    let src_h_signed = le_i32(buf, 22);
    let bpp = le_u16(buf, 28);
    let compression = le_u32(buf, 30);

    if src_w <= 0 || src_h_signed == 0 {
        log(s, b"[img] invalid dimensions");
        return false;
    }
    if bpp != 24 {
        log(s, b"[img] only 24-bit BMP supported");
        return false;
    }
    if compression != 0 {
        log(s, b"[img] only uncompressed BI_RGB supported");
        return false;
    }
    let src_w = src_w as usize;
    let src_h = src_h_signed.unsigned_abs() as usize;
    let bottom_up = src_h_signed > 0;
    let row_stride = (src_w * 3 + 3) & !3;
    let needed = pixel_offset + row_stride * src_h;
    if needed > buf.len() {
        log(s, b"[img] pixel data truncated");
        return false;
    }

    let dst_w = s.dst_w as usize;
    let dst_h = s.dst_h as usize;
    if !ensure_pending(s, dst_w, dst_h) {
        return false;
    }

    let pixel_data = &buf[pixel_offset..pixel_offset + row_stride * src_h];

    // stretch only; the root warns at init when fit/fill was asked for.
    let _ = s.scale_mode;

    // Precompute src_x for each dst_x to keep the divide out of the hot loop.

    let mut ymap = Nearest::new(src_h, dst_h);
    let mut xmap = Nearest::new(src_w, dst_w);

    for dy in 0..dst_h {
        let sy_raw = ymap.index();
        // BMP rows run bottom-up unless the height was negative.
        let sy = if bottom_up {
            (src_h - 1).saturating_sub(sy_raw)
        } else {
            sy_raw
        };
        let row = &pixel_data[sy * row_stride..sy * row_stride + src_w * 3];
        let dst_row_off = dy * dst_w * 2;
        xmap.restart();
        for dx in 0..dst_w {
            let sx = xmap.index();
            let b = row[sx * 3];
            let g = row[sx * 3 + 1];
            let r = row[sx * 3 + 2];
            store_rgb565(s.pending, dst_row_off + dx * 2, r, g, b);
            xmap.advance();
        }
        ymap.advance();
    }

    log_dims(
        s,
        b"[img] decoded ",
        src_w as u32,
        src_h as u32,
        dst_w as u32,
        dst_h as u32,
    );
    true
}

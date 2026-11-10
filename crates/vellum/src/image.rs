//! Images: kitty graphics placed at the cursor, and a halfblock fallback drawn into cells.
//!
//! Zellij 0.45 implements the kitty protocol itself: it stores an image per pane and re-emits it to
//! the host terminal (research.md §2). It refuses unicode placeholders and shared memory, so the one
//! path is direct transmission placed at the cursor with `C=1` (cursor stays put). The image's
//! content hash is its kitty id, so an unchanged image is never sent twice.

use crate::color::Rgb;
use crate::grid::Grid;
use crate::style::Style;
use std::fmt::Write;
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    /// RGBA8, row-major.
    pub rgba: Vec<u8>,
    /// Content hash; doubles as the kitty image id (never 0).
    pub id: u32,
}

impl Image {
    pub fn load_png(path: &Path) -> Result<Image, String> {
        let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut dec = png::Decoder::new(std::io::BufReader::new(f));
        dec.set_transformations(png::Transformations::normalize_to_color8());
        let mut r = dec.read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let px = (info.width * info.height) as usize;
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf[..px * 4].to_vec(),
            png::ColorType::Rgb => buf[..px * 3].chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => buf[..px * 2].chunks(2).flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
            png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return Err("indexed png not expanded".into()),
        };
        Ok(Image::from_rgba(info.width, info.height, rgba))
    }

    pub fn from_rgba(w: u32, h: u32, rgba: Vec<u8>) -> Image {
        // FNV-1a over size and pixels
        let mut hsh: u32 = 0x811c9dc5;
        for b in w.to_le_bytes().iter().chain(h.to_le_bytes().iter()).chain(rgba.iter()) {
            hsh ^= *b as u32;
            hsh = hsh.wrapping_mul(0x01000193);
        }
        Image { w, h, rgba, id: (hsh & 0x7fff_ffff).max(1) }
    }

    /// Rows a `cols`-wide slot needs to keep the aspect ratio, given the cell's pixel size.
    pub fn rows_for(&self, cols: u16, cell_w: u16, cell_h: u16) -> u16 {
        let px_w = cols as f32 * cell_w as f32;
        let px_h = px_w * self.h as f32 / self.w.max(1) as f32;
        ((px_h / cell_h as f32).round() as u16).max(1)
    }

    /// A box-filtered copy no larger than `w`×`h` that keeps the aspect ratio (never upscaled).
    /// Sending an image at its slot's pixel size, not its source size, is what keeps a placement
    /// cheap: an 818×335 PNG is 1.5 MB of base64 at full size.
    pub fn fit(&self, w: u32, h: u32) -> Image {
        let k = (w as f32 / self.w.max(1) as f32).min(h as f32 / self.h.max(1) as f32);
        if k >= 1.0 {
            return self.clone();
        }
        let (nw, nh) = (((self.w as f32 * k).round() as u32).max(1), ((self.h as f32 * k).round() as u32).max(1));
        let (sx, sy) = (self.w as f32 / nw as f32, self.h as f32 / nh as f32);
        let mut rgba = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            for x in 0..nw {
                let (x0, y0) = (x as f32 * sx, y as f32 * sy);
                rgba.extend_from_slice(&self.area_rgba(x0, y0, x0 + sx, y0 + sy));
            }
        }
        Image::from_rgba(nw, nh, rgba)
    }

    /// Average of a pixel rectangle, alpha kept (premultiplied while summing, so transparent
    /// pixels don't darken the edges).
    fn area_rgba(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> [u8; 4] {
        let (xa, ya) = (x0.floor() as u32, y0.floor() as u32);
        let (xb, yb) = ((x1.ceil() as u32).max(xa + 1), (y1.ceil() as u32).max(ya + 1));
        let (mut r, mut g, mut b, mut a, mut n) = (0f32, 0f32, 0f32, 0f32, 0f32);
        for y in ya..yb {
            for x in xa..xb {
                let p = self.px(x, y);
                let al = p[3] as f32;
                r += p[0] as f32 * al;
                g += p[1] as f32 * al;
                b += p[2] as f32 * al;
                a += al;
                n += 1.0;
            }
        }
        if a == 0.0 {
            return [0, 0, 0, 0];
        }
        [(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n) as u8]
    }

    fn px(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y.min(self.h - 1) * self.w + x.min(self.w - 1)) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    /// Average colour of a pixel rectangle, alpha-blended over `bg`.
    fn area(&self, x0: f32, y0: f32, x1: f32, y1: f32, bg: Rgb) -> Rgb {
        let (xa, ya) = (x0.floor() as u32, y0.floor() as u32);
        let (xb, yb) = ((x1.ceil() as u32).max(xa + 1), (y1.ceil() as u32).max(ya + 1));
        let (mut r, mut g, mut b, mut n) = (0f32, 0f32, 0f32, 0f32);
        for y in ya..yb {
            for x in xa..xb {
                let p = self.px(x, y);
                let a = p[3] as f32 / 255.0;
                r += p[0] as f32 * a + bg.0 as f32 * (1.0 - a);
                g += p[1] as f32 * a + bg.1 as f32 * (1.0 - a);
                b += p[2] as f32 * a + bg.2 as f32 * (1.0 - a);
                n += 1.0;
            }
        }
        Rgb((r / n) as u8, (g / n) as u8, (b / n) as u8)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(B64[(n >> 18) as usize & 63] as char);
        s.push(B64[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    s
}

/// Transmit and place an image at the cursor in a `cols`×`rows` cell rectangle. Chunked at 4096
/// base64 bytes as the protocol requires; `q=2` silences replies; `C=1` leaves the cursor still.
pub fn kitty_place(img: &Image, cols: u16, rows: u16) -> String {
    let data = base64(&img.rgba);
    let bytes = data.as_bytes();
    let mut out = String::new();
    let chunks: Vec<&[u8]> = bytes.chunks(4096).collect();
    for (i, ch) in chunks.iter().enumerate() {
        let more = (i + 1 < chunks.len()) as u8;
        if i == 0 {
            let _ = write!(
                out,
                "\x1b_Ga=T,q=2,f=32,t=d,i={},s={},v={},c={},r={},C=1,m={};",
                img.id, img.w, img.h, cols, rows, more
            );
        } else {
            let _ = write!(out, "\x1b_Gm={more};");
        }
        out.push_str(std::str::from_utf8(ch).unwrap());
        out.push_str("\x1b\\");
    }
    out
}

/// Delete an image and its placements.
pub fn kitty_delete(id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\")
}

/// Ask whether the terminal speaks kitty graphics, followed by DA1. A kitty reply that arrives
/// before the DA1 reply means images will reach the screen; DA1 alone means they won't.
pub fn kitty_probe() -> &'static str {
    "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c"
}

/// The fallback: each cell shows two pixels stacked, as the upper-half block `▀` with the top colour
/// as foreground and the bottom as background. A dim `image: blocks` tag says it's a fallback.
pub fn halfblocks(g: &mut Grid, img: &Image, x: u16, y: u16, cols: u16, rows: u16, bg: Rgb) {
    let sx = img.w as f32 / cols.max(1) as f32;
    let sy = img.h as f32 / (rows.max(1) * 2) as f32;
    for r in 0..rows {
        for c in 0..cols {
            let x0 = c as f32 * sx;
            let top = img.area(x0, (r * 2) as f32 * sy, x0 + sx, (r * 2 + 1) as f32 * sy, bg);
            let bot = img.area(x0, (r * 2 + 1) as f32 * sy, x0 + sx, (r * 2 + 2) as f32 * sy, bg);
            g.put(x + c, y + r, "▀", Style::fg(top).on(bot));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b64() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
    #[test]
    fn kitty_chunks_and_ids() {
        let img = Image::from_rgba(64, 64, vec![7; 64 * 64 * 4]);
        let s = kitty_place(&img, 8, 4);
        assert!(s.starts_with(&format!("\x1b_Ga=T,q=2,f=32,t=d,i={},s=64,v=64,c=8,r=4,C=1,m=1;", img.id)));
        assert!(s.contains("\x1b_Gm=0;"));
        let same = Image::from_rgba(64, 64, vec![7; 64 * 64 * 4]);
        assert_eq!(img.id, same.id);
        assert_ne!(img.id, Image::from_rgba(64, 64, vec![8; 64 * 64 * 4]).id);
    }
    #[test]
    fn halfblock_colours() {
        // top half red, bottom half blue
        let mut px = Vec::new();
        for y in 0..2 {
            px.extend_from_slice(if y == 0 { &[255, 0, 0, 255] } else { &[0, 0, 255, 255] });
        }
        let img = Image::from_rgba(1, 2, px);
        let mut g = Grid::new(1, 1);
        halfblocks(&mut g, &img, 0, 0, 1, 1, Rgb(0, 0, 0));
        let c = g.get(0, 0).unwrap();
        assert_eq!(c.style.fg, Some(Rgb(255, 0, 0)));
        assert_eq!(c.style.bg, Some(Rgb(0, 0, 255)));
        assert_eq!(img.rows_for(10, 10, 20), 10);
    }
    #[test]
    fn fit_downscales_and_keeps_aspect() {
        let img = Image::from_rgba(800, 400, vec![200; 800 * 400 * 4]);
        let f = img.fit(400, 400);
        assert_eq!((f.w, f.h), (400, 200));
        assert_eq!(&f.rgba[..4], &[200, 200, 200, 200]);
        assert_eq!(img.fit(2000, 2000).w, 800, "never upscaled");
    }
}

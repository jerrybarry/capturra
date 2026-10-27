//! Cursor sprites: decode the recorded PNGs and alpha-blend them onto RGB frames.
//!
//! The recorded images are large (the macOS arrow is ~10 pixels per point), so drawing
//! averages many source samples per output pixel instead of picking one, which would shimmer.

use anyhow::{bail, Context, Result};
use capturra_core::{EventLog, Hotspot, SizePt};
use std::collections::HashMap;
use std::path::Path;

/// An RGBA image with premultiplied alpha, which is what averaging and blending need.
pub struct Sprite {
    pub w: usize,
    pub h: usize,
    px: Vec<[f32; 4]>,
}

impl Sprite {
    /// Build from straight (non-premultiplied) RGBA8.
    pub fn from_rgba8(w: usize, h: usize, rgba: &[u8]) -> Self {
        assert_eq!(rgba.len(), w * h * 4);
        let px = rgba
            .chunks_exact(4)
            .map(|p| {
                let a = p[3] as f32 / 255.0;
                [p[0] as f32 / 255.0 * a, p[1] as f32 / 255.0 * a, p[2] as f32 / 255.0 * a, a]
            })
            .collect();
        Self { w, h, px }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut dec = png::Decoder::new(std::io::BufReader::new(file));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().with_context(|| format!("reading PNG header of {}", path.display()))?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).with_context(|| format!("decoding {}", path.display()))?;
        let (w, h) = (info.width as usize, info.height as usize);
        let data = &buf[..info.buffer_size()];
        let rgba: Vec<u8> = match info.color_type {
            png::ColorType::Rgba => data.to_vec(),
            png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
            png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => bail!("{}: palette PNG was not expanded", path.display()),
        };
        Ok(Self::from_rgba8(w, h, &rgba))
    }

    /// Bilinear sample at source coordinates. Points outside the image are transparent; inside
    /// it, neighbours are clamped to the edge so an opaque border stays opaque when upscaled.
    fn sample(&self, sx: f64, sy: f64) -> [f32; 4] {
        if sx < 0.0 || sy < 0.0 || sx >= self.w as f64 || sy >= self.h as f64 {
            return [0.0; 4];
        }
        let (fx, fy) = (sx - 0.5, sy - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = ((fx - x0) as f32, (fy - y0) as f32);
        let at = |x: f64, y: f64| -> [f32; 4] {
            let xi = x.clamp(0.0, (self.w - 1) as f64) as usize;
            let yi = y.clamp(0.0, (self.h - 1) as f64) as usize;
            self.px[yi * self.w + xi]
        };
        let (p00, p10, p01, p11) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        let mut out = [0.0; 4];
        for c in 0..4 {
            let top = p00[c] * (1.0 - tx) + p10[c] * tx;
            let bot = p01[c] * (1.0 - tx) + p11[c] * tx;
            out[c] = top * (1.0 - ty) + bot * ty;
        }
        out
    }
}

/// Where the sprite's top-left lands so that its hotspot sits on `(ox, oy)`.
/// `px_per_pt` is output pixels per point, already including the zoom.
pub fn sprite_origin(ox: f64, oy: f64, hotspot: Hotspot, px_per_pt: f64) -> (f64, f64) {
    (ox - hotspot.x * px_per_pt, oy - hotspot.y * px_per_pt)
}

/// Alpha-blend `sprite` onto an RGB24 frame with its hotspot at `(ox, oy)` (output pixels).
/// The sprite covers `size_pt * px_per_pt` output pixels.
pub fn draw_sprite(
    dst: &mut [u8],
    w: usize,
    h: usize,
    sprite: &Sprite,
    size_pt: SizePt,
    hotspot: Hotspot,
    ox: f64,
    oy: f64,
    px_per_pt: f64,
) {
    let (dw, dh) = (size_pt.w * px_per_pt, size_pt.h * px_per_pt);
    if dw <= 0.0 || dh <= 0.0 {
        return;
    }
    let (left, top) = sprite_origin(ox, oy, hotspot, px_per_pt);
    let x0 = left.floor().max(0.0) as usize;
    let y0 = top.floor().max(0.0) as usize;
    let x1 = ((left + dw).ceil().max(0.0) as usize).min(w);
    let y1 = ((top + dh).ceil().max(0.0) as usize).min(h);
    // Source samples per output pixel, per axis: enough to cover the downscale ratio.
    let nx = ((sprite.w as f64 / dw).ceil() as usize).clamp(1, 16);
    let ny = ((sprite.h as f64 / dh).ceil() as usize).clamp(1, 16);
    let (sx_per_px, sy_per_px) = (sprite.w as f64 / dw, sprite.h as f64 / dh);

    for y in y0..y1 {
        for x in x0..x1 {
            let mut acc = [0.0f32; 4];
            for j in 0..ny {
                let sy = (y as f64 - top + (j as f64 + 0.5) / ny as f64) * sy_per_px;
                for i in 0..nx {
                    let sx = (x as f64 - left + (i as f64 + 0.5) / nx as f64) * sx_per_px;
                    let s = sprite.sample(sx, sy);
                    for c in 0..4 {
                        acc[c] += s[c];
                    }
                }
            }
            let n = (nx * ny) as f32;
            let a = acc[3] / n;
            if a <= 0.0 {
                continue;
            }
            let i = (y * w + x) * 3;
            for c in 0..3 {
                let over = acc[c] / n * 255.0 + dst[i + c] as f32 * (1.0 - a);
                dst[i + c] = over.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Decoded sprites by cursor id. An image that fails to load is reported and left out, so
/// the renderer falls back to the circle for it instead of failing the whole export.
pub fn load_all(log: &EventLog, dir: &Path) -> HashMap<u32, Sprite> {
    let mut out = HashMap::new();
    for c in &log.cursors {
        match Sprite::load(&dir.join(&c.file)) {
            Ok(s) => {
                out.insert(c.id, s);
            }
            Err(e) => eprintln!("warning: cursor {} not drawn, using the circle instead: {e:#}", c.id),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAY: u8 = 100;

    fn frame(w: usize, h: usize) -> Vec<u8> {
        vec![GRAY; w * h * 3]
    }

    fn solid(w: usize, h: usize, rgba: [u8; 4]) -> Sprite {
        Sprite::from_rgba8(w, h, &rgba.iter().copied().cycle().take(w * h * 4).collect::<Vec<_>>())
    }

    fn px(f: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 3;
        [f[i], f[i + 1], f[i + 2]]
    }

    const RED: [u8; 3] = [255, 0, 0];
    const BG: [u8; 3] = [GRAY, GRAY, GRAY];
    const ZERO: Hotspot = Hotspot { x: 0.0, y: 0.0 };

    #[test]
    fn opaque_sprite_covers_exactly_its_point_size_at_the_hotspot() {
        let (w, h) = (32, 32);
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &solid(8, 8, [255, 0, 0, 255]), SizePt { w: 8.0, h: 8.0 }, ZERO, 10.0, 10.0, 1.0);
        assert_eq!(px(&f, w, 10, 10), RED);
        assert_eq!(px(&f, w, 17, 17), RED);
        assert_eq!(px(&f, w, 9, 10), BG);
        assert_eq!(px(&f, w, 10, 9), BG);
        assert_eq!(px(&f, w, 18, 17), BG);
        assert_eq!(px(&f, w, 17, 18), BG);
    }

    #[test]
    fn hotspot_lands_on_the_cursor_position() {
        // Hotspot (2,3) pt at (10,10) puts the top-left at (8,7).
        assert_eq!(sprite_origin(10.0, 10.0, Hotspot { x: 2.0, y: 3.0 }, 1.0), (8.0, 7.0));
        let (w, h) = (32, 32);
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &solid(4, 4, [255, 0, 0, 255]), SizePt { w: 4.0, h: 4.0 }, Hotspot { x: 2.0, y: 3.0 }, 10.0, 10.0, 1.0);
        assert_eq!(px(&f, w, 8, 7), RED);
        assert_eq!(px(&f, w, 7, 7), BG);
        assert_eq!(px(&f, w, 8, 6), BG);
        assert_eq!(px(&f, w, 11, 10), RED);
        assert_eq!(px(&f, w, 12, 10), BG);
    }

    #[test]
    fn size_and_hotspot_scale_with_pixels_per_point() {
        // 2 px/pt (e.g. display scale 1 x zoom 2): a 4pt sprite is 8px, hotspot offset doubles.
        assert_eq!(sprite_origin(20.0, 20.0, Hotspot { x: 2.0, y: 3.0 }, 2.0), (16.0, 14.0));
        let (w, h) = (48, 48);
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &solid(4, 4, [255, 0, 0, 255]), SizePt { w: 4.0, h: 4.0 }, ZERO, 20.0, 20.0, 2.0);
        assert_eq!(px(&f, w, 20, 20), RED);
        assert_eq!(px(&f, w, 27, 27), RED);
        assert_eq!(px(&f, w, 28, 27), BG);
        assert_eq!(px(&f, w, 27, 28), BG);
    }

    #[test]
    fn large_sprite_downscales_without_changing_its_color() {
        // A 10x-resolution source (like the macOS arrow) drawn at 1/10 size stays solid.
        let (w, h) = (32, 32);
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &solid(80, 80, [0, 0, 255, 255]), SizePt { w: 8.0, h: 8.0 }, ZERO, 10.0, 10.0, 1.0);
        for (x, y) in [(10, 10), (13, 14), (17, 17)] {
            let p = px(&f, w, x, y);
            assert!(p[0] <= 1 && p[1] <= 1 && p[2] >= 254, "({x},{y}) = {p:?}");
        }
        assert_eq!(px(&f, w, 18, 18), BG);
    }

    #[test]
    fn half_transparent_pixels_blend_with_the_frame() {
        let (w, h) = (16, 16);
        let mut f = frame(w, h);
        // 50% white over gray 100 -> 177.5.
        draw_sprite(&mut f, w, h, &solid(4, 4, [255, 255, 255, 128]), SizePt { w: 4.0, h: 4.0 }, ZERO, 4.0, 4.0, 1.0);
        let p = px(&f, w, 5, 5);
        assert!(p.iter().all(|&c| (176..=179).contains(&c)), "{p:?}");
    }

    #[test]
    fn transparent_parts_leave_the_frame_alone() {
        // Left half opaque red, right half fully transparent.
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for y in 0..4 {
            for x in 0..2 {
                rgba[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let (w, h) = (16, 16);
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &Sprite::from_rgba8(4, 4, &rgba), SizePt { w: 4.0, h: 4.0 }, ZERO, 4.0, 4.0, 1.0);
        assert_eq!(px(&f, w, 4, 5), RED);
        assert_eq!(px(&f, w, 5, 5), RED);
        assert_eq!(px(&f, w, 6, 5), BG);
        assert_eq!(px(&f, w, 7, 5), BG);
    }

    #[test]
    fn clipping_at_every_edge_does_not_panic_or_spill() {
        let (w, h) = (16, 16);
        let s = solid(8, 8, [255, 0, 0, 255]);
        let size = SizePt { w: 8.0, h: 8.0 };
        for (ox, oy) in [(-100.0, 5.0), (5.0, -100.0), (100.0, 5.0), (5.0, 100.0), (-4.0, -4.0), (12.0, 12.0)] {
            let mut f = frame(w, h);
            draw_sprite(&mut f, w, h, &s, size, ZERO, ox, oy, 1.0);
            assert_eq!(f.len(), w * h * 3);
        }
        let mut f = frame(w, h);
        draw_sprite(&mut f, w, h, &s, size, ZERO, -4.0, -4.0, 1.0);
        assert_eq!(px(&f, w, 3, 3), RED);
        assert_eq!(px(&f, w, 4, 4), BG);
    }

    #[test]
    fn png_with_alpha_decodes_to_premultiplied_pixels() {
        let path = std::env::temp_dir().join(format!("capturra_sprite_test_{}.png", std::process::id()));
        {
            let file = std::fs::File::create(&path).unwrap();
            let mut enc = png::Encoder::new(file, 2, 1);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&[255, 0, 0, 255, 0, 255, 0, 0]).unwrap();
        }
        let s = Sprite::load(&path).unwrap();
        assert_eq!((s.w, s.h), (2, 1));
        assert_eq!(s.px[0], [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.px[1], [0.0, 0.0, 0.0, 0.0], "a fully transparent pixel has no color");
    }

    #[test]
    fn missing_file_is_reported_not_panicked() {
        assert!(Sprite::load(Path::new("/nonexistent/cursor.png")).is_err());
    }
}

//! The Studio's "frame": the recording sits inside a padded canvas with a gradient background,
//! rounded corners and a soft drop shadow. The background and shadow never change, so they are
//! baked once; each video frame then only copies the picture into place.
//!
//! The numbers follow the Studio preview (`stage.ts`): padding is a percentage of the canvas's
//! shorter side; radius and shadow are in "preview pixels" for a picture about 900 px wide, so they
//! are scaled to the real picture width.

/// How wide the Studio preview's picture is, which the radius and shadow numbers assume.
const PREVIEW_WIDTH: f64 = 900.0;

/// A decoded picture (8-bit RGB), used as a background.
#[derive(Debug, PartialEq)]
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>,
}

impl Picture {
    /// Decode a PNG (any colour type; transparency is dropped, 16-bit is reduced to 8).
    pub fn from_png(bytes: &[u8]) -> Result<Picture, String> {
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = dec.read_info().map_err(|e| format!("not a PNG picture: {e}"))?;
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).map_err(|e| format!("could not read the picture: {e}"))?;
        let data = &buf[..info.buffer_size()];
        let (w, h) = (info.width as usize, info.height as usize);
        if w == 0 || h == 0 {
            return Err("the picture has no pixels".to_string());
        }
        let rgb: Vec<u8> = match info.color_type {
            png::ColorType::Rgb => data.to_vec(),
            png::ColorType::Rgba => data.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect(),
            png::ColorType::Grayscale => data.iter().flat_map(|&g| [g, g, g]).collect(),
            png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0]]).collect(),
            png::ColorType::Indexed => return Err("indexed PNG was not expanded".to_string()),
        };
        Ok(Picture { w, h, rgb })
    }
}

/// What is drawn behind the recording.
#[derive(Debug, Clone, PartialEq)]
pub enum Backdrop {
    /// From the top-left colour to the bottom-right one (the same colour twice is a flat colour).
    Gradient([u8; 3], [u8; 3]),
    /// A picture, scaled to cover the canvas and cropped at the edges, never stretched.
    Image(std::sync::Arc<Picture>),
}

/// The part of a `pic_w` x `pic_h` picture that covers a `canvas_w` x `canvas_h` canvas without
/// stretching, as a normalised rectangle centred on the picture.
pub fn cover_rect(pic_w: usize, pic_h: usize, canvas_w: usize, canvas_h: usize) -> capturra_core::Rect {
    let (pa, ca) = (pic_w as f64 / pic_h as f64, canvas_w as f64 / canvas_h as f64);
    if pa > ca {
        let w = ca / pa; // the picture is wider than the canvas: trim the sides
        capturra_core::Rect { x: (1.0 - w) / 2.0, y: 0.0, w, h: 1.0 }
    } else {
        let h = pa / ca; // taller: trim the top and bottom
        capturra_core::Rect { x: 0.0, y: (1.0 - h) / 2.0, w: 1.0, h }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrameStyle {
    /// Canvas width / height. `None` = the same shape as the recording.
    pub aspect: Option<f64>,
    pub backdrop: Backdrop,
    /// Padding on each side, % of the canvas's shorter side.
    pub padding_pct: f64,
    /// Corner radius in preview pixels.
    pub radius: f64,
    /// 0..=1.
    pub shadow: f64,
}

/// Where the picture goes on the canvas. Every size is even (video encoders need that).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub canvas_w: usize,
    pub canvas_h: usize,
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

fn even(v: f64) -> usize {
    ((v.round().max(2.0)) as usize) & !1
}

/// Lay out a `src_w` x `src_h` picture on a canvas `canvas_w` wide (the height follows the aspect).
pub fn layout(style: &FrameStyle, src_w: usize, src_h: usize, canvas_w: usize) -> Layout {
    let src_ar = src_w as f64 / src_h as f64;
    let ar = style.aspect.unwrap_or(src_ar);
    let cw = canvas_w & !1;
    let ch = even(cw as f64 / ar);
    let pad = style.padding_pct.clamp(0.0, 40.0) / 100.0 * cw.min(ch) as f64;
    let (avail_w, avail_h) = ((cw as f64 - 2.0 * pad).max(2.0), (ch as f64 - 2.0 * pad).max(2.0));
    let (mut w, mut h) = (avail_w, avail_w / src_ar);
    if h > avail_h {
        h = avail_h;
        w = h * src_ar;
    }
    let (w, h) = (even(w), even(h));
    Layout { canvas_w: cw, canvas_h: ch, x: ((cw - w) / 2) & !1, y: ((ch - h) / 2) & !1, w, h }
}

/// Coverage (0..=255) of a `w` x `h` rounded rectangle, antialiased on the corner arcs.
pub fn rounded_mask(w: usize, h: usize, radius: f64) -> Vec<u8> {
    let r = radius.clamp(0.0, (w.min(h) / 2) as f64);
    let mut m = vec![255u8; w * h];
    if r <= 0.0 {
        return m;
    }
    let corner = r.ceil() as usize;
    for y in 0..h.min(corner).max(0) {
        for x in 0..w.min(corner) {
            // distance of this pixel's centre from the arc's centre, for each of the four corners
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let d = ((r - px).powi(2) + (r - py).powi(2)).sqrt();
            let a = if px < r && py < r { (r - d + 0.5).clamp(0.0, 1.0) } else { 1.0 };
            let v = (a * 255.0).round() as u8;
            m[y * w + x] = v;
            m[y * w + (w - 1 - x)] = v;
            m[(h - 1 - y) * w + x] = v;
            m[(h - 1 - y) * w + (w - 1 - x)] = v;
        }
    }
    m
}

/// Three passes of a box blur ~ a gaussian of the given sigma (in pixels), in place.
fn blur(buf: &mut [f32], w: usize, h: usize, sigma: f64) {
    let radius = ((sigma * 1.2).round() as usize).max(1);
    let span = 2 * radius + 1;
    let mut tmp = vec![0f32; buf.len()];
    for _ in 0..3 {
        for y in 0..h {
            let row = &buf[y * w..(y + 1) * w];
            let out = &mut tmp[y * w..(y + 1) * w];
            let mut sum: f32 = (0..=radius.min(w - 1)).map(|i| row[i]).sum();
            for x in 0..w {
                out[x] = sum / span as f32;
                let add = x + radius + 1;
                if add < w {
                    sum += row[add];
                }
                if x >= radius {
                    sum -= row[x - radius];
                }
            }
        }
        for x in 0..w {
            let mut sum: f32 = (0..=radius.min(h - 1)).map(|i| tmp[i * w + x]).sum();
            for y in 0..h {
                buf[y * w + x] = sum / span as f32;
                let add = y + radius + 1;
                if add < h {
                    sum += tmp[add * w + x];
                }
                if y >= radius {
                    sum -= tmp[(y - radius) * w + x];
                }
            }
        }
    }
}

pub struct Compositor {
    pub layout: Layout,
    /// RGB canvas: gradient with the shadow already darkened into it.
    background: Vec<u8>,
    mask: Vec<u8>,
}

impl Compositor {
    pub fn new(style: &FrameStyle, layout: Layout) -> Self {
        let Layout { canvas_w: cw, canvas_h: ch, x, y, w, h } = layout;
        let k = w as f64 / PREVIEW_WIDTH;
        let radius = style.radius * k;
        let mask = rounded_mask(w, h, radius);

        let mut bg = vec![0u8; cw * ch * 3];
        match &style.backdrop {
            // 135 degrees: top-left colour to bottom-right
            Backdrop::Gradient(c1, c2) => {
                for py in 0..ch {
                    for px in 0..cw {
                        let t = ((px as f64 / cw as f64) + (py as f64 / ch as f64)) / 2.0;
                        for c in 0..3 {
                            bg[(py * cw + px) * 3 + c] = (c1[c] as f64 + (c2[c] as f64 - c1[c] as f64) * t).round() as u8;
                        }
                    }
                }
            }
            Backdrop::Image(pic) => {
                let rect = cover_rect(pic.w, pic.h, cw, ch);
                crate::resample::crop_resize(&pic.rgb, pic.w, pic.h, rect, &mut bg, cw, ch);
            }
        }

        // shadow: the rounded rectangle, moved down and blurred, darkening the gradient
        let s = style.shadow.clamp(0.0, 1.0);
        if s > 0.0 {
            let (dy, blur_px, alpha) = ((6.0 + s * 22.0) * k, (14.0 + s * 50.0) * k, 0.12 + s * 0.5);
            let mut a = vec![0f32; cw * ch];
            for my in 0..h {
                for mx in 0..w {
                    let (px, py) = (x + mx, y + my + dy.round() as usize);
                    if px < cw && py < ch {
                        a[py * cw + px] = mask[my * w + mx] as f32 / 255.0;
                    }
                }
            }
            blur(&mut a, cw, ch, blur_px / 2.0);
            for i in 0..cw * ch {
                let f = 1.0 - a[i] * alpha as f32;
                for c in 0..3 {
                    bg[i * 3 + c] = (bg[i * 3 + c] as f32 * f).round() as u8;
                }
            }
        }
        Self { layout, background: bg, mask }
    }

    /// Put `picture` (`layout.w * layout.h` RGB) on the canvas (`canvas_w * canvas_h` RGB).
    pub fn compose(&self, picture: &[u8], canvas: &mut [u8]) {
        let Layout { canvas_w: cw, x, y, w, h, .. } = self.layout;
        canvas.copy_from_slice(&self.background);
        for row in 0..h {
            let src = &picture[row * w * 3..(row + 1) * w * 3];
            let dst_start = ((y + row) * cw + x) * 3;
            let m = &self.mask[row * w..(row + 1) * w];
            if m.iter().all(|&v| v == 255) {
                canvas[dst_start..dst_start + w * 3].copy_from_slice(src);
                continue;
            }
            for col in 0..w {
                let a = m[col] as u32;
                if a == 0 {
                    continue;
                }
                for c in 0..3 {
                    let i = dst_start + col * 3 + c;
                    canvas[i] = ((src[col * 3 + c] as u32 * a + canvas[i] as u32 * (255 - a)) / 255) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> FrameStyle {
        FrameStyle {
            aspect: Some(16.0 / 9.0),
            backdrop: Backdrop::Gradient([255, 0, 0], [0, 0, 255]),
            padding_pct: 6.0,
            radius: 14.0,
            shadow: 0.5,
        }
    }

    #[test]
    fn the_picture_fits_inside_the_padding_and_keeps_its_shape() {
        let l = layout(&style(), 2048, 1280, 1920);
        assert_eq!((l.canvas_w, l.canvas_h), (1920, 1080));
        assert!(l.w.is_multiple_of(2) && l.h.is_multiple_of(2) && l.x.is_multiple_of(2) && l.y.is_multiple_of(2));
        let pad = (0.06 * 1080.0) as usize;
        assert!(l.x >= pad - 2 && l.y >= pad - 2 && l.x + l.w <= 1920 - pad + 2 && l.y + l.h <= 1080 - pad + 2);
        assert!((l.w as f64 / l.h as f64 - 1.6).abs() < 0.01, "{l:?}");
        // a tall canvas is limited by width instead
        let t = layout(&FrameStyle { aspect: Some(9.0 / 16.0), ..style() }, 2048, 1280, 1080);
        assert_eq!((t.canvas_w, t.canvas_h), (1080, 1920));
        assert!(t.w + 2 * (0.06 * 1080.0) as usize <= 1080 + 4);
    }

    #[test]
    fn source_aspect_and_no_padding_fill_the_canvas() {
        let l = layout(&FrameStyle { aspect: None, padding_pct: 0.0, ..style() }, 2048, 1280, 1600);
        assert_eq!((l.canvas_w, l.canvas_h, l.x, l.y, l.w, l.h), (1600, 1000, 0, 0, 1600, 1000));
    }

    #[test]
    fn corners_are_cut_and_the_middle_is_solid() {
        let m = rounded_mask(100, 60, 20.0);
        assert_eq!(m[30 * 100 + 50], 255);
        assert_eq!(m[0], 0);
        assert_eq!(m[99], 0);
        assert_eq!(m[59 * 100], 0);
        assert_eq!(m[59 * 100 + 99], 0);
        assert_eq!(m[30 * 100], 255, "the straight edge is not cut");
        let edge = m[6 * 100 + 5]; // on the arc: partly covered
        assert!(edge > 0 && edge < 255, "{edge}");
        assert!(rounded_mask(100, 60, 0.0).iter().all(|&v| v == 255));
        assert!(rounded_mask(10, 10, 500.0).contains(&255), "an oversized radius is limited, not a blank picture");
    }

    #[test]
    fn a_picture_covers_the_canvas_by_cropping_the_long_side_never_stretching() {
        // wider than the canvas (2:1 into 16:9): the sides are trimmed, full height kept
        let r = cover_rect(2000, 1000, 1600, 900);
        assert!((r.w - (16.0 / 9.0) / 2.0).abs() < 1e-9 && r.h == 1.0 && r.y == 0.0 && (r.x - (1.0 - r.w) / 2.0).abs() < 1e-9);
        // taller than the canvas (1:1 into 16:9): the top and bottom are trimmed
        let r = cover_rect(1000, 1000, 1600, 900);
        assert!((r.h - 9.0 / 16.0).abs() < 1e-9 && r.w == 1.0 && r.x == 0.0 && (r.y - (1.0 - r.h) / 2.0).abs() < 1e-9);
        // the same shape is used whole
        let r = cover_rect(1600, 900, 320, 180);
        assert_eq!((r.x, r.y, r.w, r.h), (0.0, 0.0, 1.0, 1.0));
    }

    #[test]
    fn an_image_background_is_drawn_cropped_to_fill_and_still_gets_a_shadow() {
        // 4 x 4 picture: top half red, bottom half blue, on a 16:9 canvas -> only the middle band
        // is used, so the canvas shows red above and blue below the centre line
        let mut rgb = Vec::new();
        for y in 0..4 {
            for _ in 0..4 {
                rgb.extend_from_slice(if y < 2 { &[255u8, 0, 0] } else { &[0u8, 0, 255] });
            }
        }
        let pic = std::sync::Arc::new(Picture { w: 4, h: 4, rgb });
        let s = FrameStyle { backdrop: Backdrop::Image(pic.clone()), shadow: 0.0, ..style() };
        let l = layout(&s, 1600, 900, 320);
        let c = Compositor::new(&s, l);
        let px = |x: usize, y: usize| {
            let i = (y * l.canvas_w + x) * 3;
            [c.background[i], c.background[i + 1], c.background[i + 2]]
        };
        assert!(px(2, 4)[0] > 200 && px(2, 4)[2] < 60, "{:?}", px(2, 4));
        assert!(px(2, l.canvas_h - 5)[2] > 200 && px(2, l.canvas_h - 5)[0] < 60);
        // the shadow still darkens the image next to the picture
        let shadowed = Compositor::new(&FrameStyle { backdrop: Backdrop::Image(pic), shadow: 1.0, ..style() }, l);
        let at = ((l.y + l.h + 2) * l.canvas_w + l.x + l.w / 2) * 3;
        let sum = |v: &[u8]| v[at..at + 3].iter().map(|&x| x as u32).sum::<u32>();
        assert!(sum(&shadowed.background) < sum(&c.background), "{} vs {}", sum(&shadowed.background), sum(&c.background));
    }

    #[test]
    fn png_pictures_decode_to_rgb_whatever_their_colour_type() {
        fn encode(color: png::ColorType, data: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(&mut out, 2, 1);
            enc.set_color(color);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header().unwrap().write_image_data(data).unwrap();
            out
        }
        let rgb = Picture::from_png(&encode(png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!((rgb.w, rgb.h, rgb.rgb), (2, 1, vec![1, 2, 3, 4, 5, 6]));
        let rgba = Picture::from_png(&encode(png::ColorType::Rgba, &[1, 2, 3, 0, 4, 5, 6, 255])).unwrap();
        assert_eq!(rgba.rgb, vec![1, 2, 3, 4, 5, 6]);
        let gray = Picture::from_png(&encode(png::ColorType::Grayscale, &[9, 200])).unwrap();
        assert_eq!(gray.rgb, vec![9, 9, 9, 200, 200, 200]);
        assert!(Picture::from_png(b"not a png").is_err());
    }

    #[test]
    fn the_background_runs_from_the_first_colour_to_the_second() {
        let s = FrameStyle { shadow: 0.0, ..style() };
        let l = layout(&s, 1600, 900, 320);
        let c = Compositor::new(&s, l);
        let px = |x: usize, y: usize| {
            let i = (y * l.canvas_w + x) * 3;
            [c.background[i], c.background[i + 1], c.background[i + 2]]
        };
        let (tl, br) = (px(0, 0), px(l.canvas_w - 1, l.canvas_h - 1));
        assert!(tl[0] > 250 && tl[2] < 5, "{tl:?}");
        assert!(br[0] < 5 && br[2] > 250, "{br:?}");
    }

    #[test]
    fn the_shadow_darkens_around_the_picture_most_below_it_and_never_brightens() {
        let flat = FrameStyle { backdrop: Backdrop::Gradient([200, 200, 200], [200, 200, 200]), ..style() };
        let l = layout(&flat, 1600, 900, 640);
        let with = Compositor::new(&flat, l);
        let without = Compositor::new(&FrameStyle { shadow: 0.0, ..flat }, l);
        assert!(with.background.iter().zip(&without.background).all(|(a, b)| a <= b));
        let at = |c: &Compositor, y: usize| c.background[(y * l.canvas_w + l.canvas_w / 2) * 3] as i32;
        let (above, below) = (l.y - 4, l.y + l.h + 4);
        let dark_above = at(&without, above) - at(&with, above);
        let dark_below = at(&without, below) - at(&with, below);
        assert!(dark_below > 0 && dark_below >= dark_above, "above {dark_above}, below {dark_below}");
        let dark_edge = at(&without, 0) - at(&with, 0);
        assert!(dark_edge < dark_below, "the shadow fades with distance: edge {dark_edge}, just below {dark_below}");
    }

    #[test]
    fn composing_puts_the_picture_in_place_and_leaves_the_rounded_corners_to_the_background() {
        let s = FrameStyle { shadow: 0.0, radius: 60.0, ..style() };
        let l = layout(&s, 1600, 900, 640);
        let c = Compositor::new(&s, l);
        let picture = vec![7u8; l.w * l.h * 3];
        let mut canvas = vec![0u8; l.canvas_w * l.canvas_h * 3];
        c.compose(&picture, &mut canvas);
        let at = |x: usize, y: usize| canvas[(y * l.canvas_w + x) * 3..(y * l.canvas_w + x) * 3 + 3].to_vec();
        assert_eq!(at(l.x + l.w / 2, l.y + l.h / 2), vec![7, 7, 7]);
        assert_ne!(at(l.x, l.y), vec![7, 7, 7], "the corner pixel shows the background");
        assert_eq!(at(l.x + l.w / 2, l.y), vec![7, 7, 7], "the straight top edge is picture");
        assert_eq!(at(1, 1), c.background[(l.canvas_w + 1) * 3..(l.canvas_w + 1) * 3 + 3].to_vec());
    }
}

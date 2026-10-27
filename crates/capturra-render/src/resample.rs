//! High-quality crop-and-resize for the exporter.
//!
//! The picture is cropped from the ORIGINAL full-resolution frame and resampled straight to the output
//! size with a Lanczos filter, in one step. (It used to shrink the frame to the output size first and
//! then crop and enlarge that smaller copy with bilinear sampling, so a zoomed shot was resampled twice
//! and came out soft and blocky.) Lanczos stays sharp when enlarging and, because the filter widens
//! when shrinking, averages properly instead of aliasing. The work is split across the CPU cores.

use capturra_core::Rect;
use std::f64::consts::PI;
use std::thread;

/// Lobes of the Lanczos window: 3 is the usual balance of sharpness and ringing.
const LOBES: f64 = 3.0;

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        (PI * x).sin() / (PI * x)
    }
}

fn lanczos(x: f64) -> f64 {
    if x.abs() >= LOBES {
        0.0
    } else {
        sinc(x) * sinc(x / LOBES)
    }
}

/// The filter taps for one axis: for every output pixel, which source pixels to read and how much of each.
struct Taps {
    /// Source index of each output pixel's first tap (may lie outside the image: edges are replicated).
    first: Vec<isize>,
    /// `per` weights for each output pixel, normalised to sum to 1.
    weights: Vec<f32>,
    per: usize,
}

/// Taps that map `src_span` source pixels starting at `src_start` onto `dst_len` output pixels.
fn axis(src_start: f64, src_span: f64, dst_len: usize) -> Taps {
    let scale = src_span / dst_len as f64; // source pixels per output pixel
    let filter = scale.max(1.0); // widen the filter when shrinking, so detail is averaged and not skipped
    let support = LOBES * filter;
    let per = (2.0 * support).ceil() as usize + 2;
    let mut first = Vec::with_capacity(dst_len);
    let mut weights = vec![0f32; dst_len * per];
    for i in 0..dst_len {
        let centre = src_start + (i as f64 + 0.5) * scale; // pixel k covers [k, k + 1)
        let lo = (centre - support).floor() as isize;
        first.push(lo);
        let row = &mut weights[i * per..(i + 1) * per];
        let mut sum = 0.0;
        for (j, w) in row.iter_mut().enumerate() {
            let v = lanczos(((lo + j as isize) as f64 + 0.5 - centre) / filter);
            *w = v as f32;
            sum += v;
        }
        let norm = if sum.abs() > 1e-12 { (1.0 / sum) as f32 } else { 1.0 };
        row.iter_mut().for_each(|w| *w *= norm);
    }
    Taps { first, weights, per }
}

fn threads_for(rows: usize) -> usize {
    thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8).min(rows.max(1))
}

/// Reusable scratch space, so a long export does not allocate a large buffer for every frame.
#[derive(Default)]
pub struct Resampler {
    tmp: Vec<f32>,
}

impl Resampler {
    /// Crop `r` (normalised 0..=1 of the source) out of `src` (`sw` x `sh`, RGB) and resample it to
    /// fill `dst` (`dw` x `dh`, RGB).
    pub fn crop_resize(&mut self, src: &[u8], sw: usize, sh: usize, r: Rect, dst: &mut [u8], dw: usize, dh: usize) {
        assert_eq!(src.len(), sw * sh * 3, "source is not sw x sh RGB");
        assert_eq!(dst.len(), dw * dh * 3, "destination is not dw x dh RGB");
        if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
            return;
        }
        let hx = axis(r.x * sw as f64, r.w * sw as f64, dw);
        let vy = axis(r.y * sh as f64, r.h * sh as f64, dh);

        // Only the source rows the vertical filter will read are processed horizontally.
        let clamp_row = |y: isize| y.clamp(0, sh as isize - 1) as usize;
        let y_first = vy.first.iter().map(|&y| clamp_row(y)).min().unwrap_or(0);
        let y_last = vy.first.iter().map(|&y| clamp_row(y + vy.per as isize - 1)).max().unwrap_or(0);
        let rows = y_last - y_first + 1;
        let stride = dw * 3;
        self.tmp.resize(rows * stride, 0.0);

        // Pass 1: horizontal. Each source row becomes a row of `dw` output-width pixels. Taps that stay inside
        // the row (nearly all of them) are read without clamping.
        let chunk = rows.div_ceil(threads_for(rows));
        thread::scope(|scope| {
            for (n, out) in self.tmp.chunks_mut(chunk * stride).enumerate() {
                let hx = &hx;
                scope.spawn(move || {
                    for (k, line) in out.chunks_mut(stride).enumerate() {
                        let y = y_first + n * chunk + k;
                        let row = &src[y * sw * 3..(y + 1) * sw * 3];
                        for (dx, px) in line.chunks_exact_mut(3).enumerate() {
                            let taps = &hx.weights[dx * hx.per..(dx + 1) * hx.per];
                            let lo = hx.first[dx];
                            let (mut a, mut b, mut c) = (0f32, 0f32, 0f32);
                            if lo >= 0 && lo as usize + hx.per <= sw {
                                let span = &row[lo as usize * 3..(lo as usize + hx.per) * 3];
                                for (w, p) in taps.iter().zip(span.chunks_exact(3)) {
                                    a += w * p[0] as f32;
                                    b += w * p[1] as f32;
                                    c += w * p[2] as f32;
                                }
                            } else {
                                for (j, &w) in taps.iter().enumerate() {
                                    let sx = (lo + j as isize).clamp(0, sw as isize - 1) as usize * 3;
                                    a += w * row[sx] as f32;
                                    b += w * row[sx + 1] as f32;
                                    c += w * row[sx + 2] as f32;
                                }
                            }
                            px[0] = a;
                            px[1] = b;
                            px[2] = c;
                        }
                    }
                });
            }
        });

        // Pass 2: vertical, into the output. One whole row at a time: each tap adds a weighted source row to
        // the accumulator, which walks memory in order (and vectorises), instead of hopping a row apart per pixel.
        let tmp = &self.tmp;
        let chunk = dh.div_ceil(threads_for(dh));
        thread::scope(|scope| {
            for (n, out) in dst.chunks_mut(chunk * stride).enumerate() {
                let vy = &vy;
                scope.spawn(move || {
                    let mut acc = vec![0f32; stride];
                    for (k, line) in out.chunks_mut(stride).enumerate() {
                        let dy = n * chunk + k;
                        let taps = &vy.weights[dy * vy.per..(dy + 1) * vy.per];
                        let lo = vy.first[dy];
                        acc.iter_mut().for_each(|a| *a = 0.0);
                        for (j, &w) in taps.iter().enumerate() {
                            let y = clamp_row(lo + j as isize) - y_first;
                            for (a, &s) in acc.iter_mut().zip(&tmp[y * stride..(y + 1) * stride]) {
                                *a += w * s;
                            }
                        }
                        for (px, &a) in line.iter_mut().zip(&acc) {
                            *px = (a + 0.5).clamp(0.0, 255.0) as u8;
                        }
                    }
                });
            }
        });
    }
}

/// One-off form of [`Resampler::crop_resize`].
pub fn crop_resize(src: &[u8], sw: usize, sh: usize, r: Rect, dst: &mut [u8], dw: usize, dh: usize) {
    Resampler::default().crop_resize(src, sw, sh, r, dst, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: Rect = Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let f = &f;
        (0..h).flat_map(move |y| (0..w).flat_map(move |x| f(x, y))).collect()
    }
    fn px(buf: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 3;
        [buf[i], buf[i + 1], buf[i + 2]]
    }

    /// The previous implementation, kept as the yardstick: the optimised one must give the same picture.
    fn reference_crop_resize(src: &[u8], sw: usize, sh: usize, r: Rect, dst: &mut [u8], dw: usize, dh: usize) {
        let hx = axis(r.x * sw as f64, r.w * sw as f64, dw);
        let vy = axis(r.y * sh as f64, r.h * sh as f64, dh);
        let clamp_row = |y: isize| y.clamp(0, sh as isize - 1) as usize;
        let mut tmp = vec![0f32; sh * dw * 3];
        for y in 0..sh {
            for dx in 0..dw {
                let taps = &hx.weights[dx * hx.per..(dx + 1) * hx.per];
                for c in 0..3 {
                    let mut acc = 0f32;
                    for (j, &w) in taps.iter().enumerate() {
                        let sx = (hx.first[dx] + j as isize).clamp(0, sw as isize - 1) as usize;
                        acc += w * src[(y * sw + sx) * 3 + c] as f32;
                    }
                    tmp[(y * dw + dx) * 3 + c] = acc;
                }
            }
        }
        for dy in 0..dh {
            let taps = &vy.weights[dy * vy.per..(dy + 1) * vy.per];
            for col in 0..dw * 3 {
                let mut sum = 0f32;
                for (j, &w) in taps.iter().enumerate() {
                    sum += w * tmp[clamp_row(vy.first[dy] + j as isize) * dw * 3 + col];
                }
                dst[dy * dw * 3 + col] = (sum + 0.5).clamp(0.0, 255.0) as u8;
            }
        }
    }

    /// The implementation before the loops were rewritten (threaded, but clamping every tap and walking the
    /// vertical pass a column at a time), kept only so the speed test can say how much faster the new one is.
    fn previous_threaded(src: &[u8], sw: usize, sh: usize, r: Rect, dst: &mut [u8], dw: usize, dh: usize) {
        let hx = axis(r.x * sw as f64, r.w * sw as f64, dw);
        let vy = axis(r.y * sh as f64, r.h * sh as f64, dh);
        let clamp_row = |y: isize| y.clamp(0, sh as isize - 1) as usize;
        let y_first = vy.first.iter().map(|&y| clamp_row(y)).min().unwrap_or(0);
        let y_last = vy.first.iter().map(|&y| clamp_row(y + vy.per as isize - 1)).max().unwrap_or(0);
        let rows = y_last - y_first + 1;
        let stride = dw * 3;
        let mut tmp = vec![0f32; rows * stride];
        let chunk = rows.div_ceil(threads_for(rows));
        thread::scope(|scope| {
            for (n, out) in tmp.chunks_mut(chunk * stride).enumerate() {
                let hx = &hx;
                scope.spawn(move || {
                    for (k, line) in out.chunks_mut(stride).enumerate() {
                        let y = y_first + n * chunk + k;
                        let row = &src[y * sw * 3..(y + 1) * sw * 3];
                        for dx in 0..dw {
                            let taps = &hx.weights[dx * hx.per..(dx + 1) * hx.per];
                            let lo = hx.first[dx];
                            let (mut a, mut b, mut c) = (0f32, 0f32, 0f32);
                            for (j, &w) in taps.iter().enumerate() {
                                let sx = (lo + j as isize).clamp(0, sw as isize - 1) as usize * 3;
                                a += w * row[sx] as f32;
                                b += w * row[sx + 1] as f32;
                                c += w * row[sx + 2] as f32;
                            }
                            line[dx * 3] = a;
                            line[dx * 3 + 1] = b;
                            line[dx * 3 + 2] = c;
                        }
                    }
                });
            }
        });
        let tmp = &tmp;
        let chunk = dh.div_ceil(threads_for(dh));
        thread::scope(|scope| {
            for (n, out) in dst.chunks_mut(chunk * stride).enumerate() {
                let vy = &vy;
                scope.spawn(move || {
                    for (k, line) in out.chunks_mut(stride).enumerate() {
                        let dy = n * chunk + k;
                        let taps = &vy.weights[dy * vy.per..(dy + 1) * vy.per];
                        let lo = vy.first[dy];
                        for (col, px) in line.iter_mut().enumerate() {
                            let mut sum = 0f32;
                            for (j, &w) in taps.iter().enumerate() {
                                let y = clamp_row(lo + j as isize) - y_first;
                                sum += w * tmp[y * stride + col];
                            }
                            *px = (sum + 0.5).clamp(0.0, 255.0) as u8;
                        }
                    }
                });
            }
        });
    }

    /// A busy, noisy picture exercises every tap and every edge.
    fn noisy(w: usize, h: usize) -> Vec<u8> {
        let mut x = 0x2545F491u32;
        (0..w * h * 3)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x >> 8) as u8
            })
            .collect()
    }

    #[test]
    fn the_optimised_resampler_gives_the_same_picture_as_the_reference() {
        let src = noisy(173, 97);
        for (dw, dh, r) in [
            (173, 97, FULL),
            (160, 90, FULL),
            (320, 180, FULL),
            (97, 53, Rect { x: 0.0, y: 0.0, w: 0.5, h: 0.5 }),
            (200, 120, Rect { x: 0.55, y: 0.45, w: 0.45, h: 0.55 }),
            (64, 64, Rect { x: 0.3, y: 0.1, w: 0.2, h: 0.8 }),
        ] {
            let (mut a, mut b) = (vec![0u8; dw * dh * 3], vec![0u8; dw * dh * 3]);
            crop_resize(&src, 173, 97, r, &mut a, dw, dh);
            reference_crop_resize(&src, 173, 97, r, &mut b, dw, dh);
            let worst = a.iter().zip(&b).map(|(x, y)| (*x as i32 - *y as i32).abs()).max().unwrap();
            assert!(worst <= 1, "{dw}x{dh} {r:?}: worst difference {worst}");
        }
    }

    /// Timing, not a pass/fail check: `cargo test --release -p capturra-render resampler_speed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn resampler_speed() {
        let src = noisy(2048, 1280);
        let mut dst = vec![0u8; 1920 * 1080 * 3];
        let mut r = Resampler::default();
        for (name, rect) in [("full frame", FULL), ("2x zoom", Rect { x: 0.25, y: 0.25, w: 0.5, h: 0.5 })] {
            r.crop_resize(&src, 2048, 1280, rect, &mut dst, 1920, 1080); // warm up
            let t = std::time::Instant::now();
            for _ in 0..10 {
                r.crop_resize(&src, 2048, 1280, rect, &mut dst, 1920, 1080);
            }
            let new = t.elapsed().as_secs_f64() * 100.0;
            previous_threaded(&src, 2048, 1280, rect, &mut dst, 1920, 1080);
            let t = std::time::Instant::now();
            for _ in 0..10 {
                previous_threaded(&src, 2048, 1280, rect, &mut dst, 1920, 1080);
            }
            let old = t.elapsed().as_secs_f64() * 100.0;
            eprintln!("{name}: {new:.1} ms per frame now, {old:.1} ms before the rewrite ({:.1}x faster)", old / new);
        }
    }

    #[test]
    fn the_same_size_and_the_whole_frame_changes_nothing() {
        let (w, h) = (23, 11);
        let src = image(w, h, |x, y| [(x * 11 % 251) as u8, (y * 23 % 251) as u8, ((x * y) % 251) as u8]);
        let mut dst = vec![0u8; src.len()];
        crop_resize(&src, w, h, FULL, &mut dst, w, h);
        let worst = src.iter().zip(&dst).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 1, "worst difference {worst}");
    }

    #[test]
    fn a_flat_colour_stays_exactly_that_colour_whatever_the_scale_or_crop() {
        let src = image(64, 40, |_, _| [200, 90, 17]);
        for (dw, dh, r) in [
            (128, 80, FULL),
            (30, 19, FULL),
            (200, 100, Rect { x: 0.3, y: 0.2, w: 0.4, h: 0.5 }),
            (64, 40, Rect { x: 0.0, y: 0.0, w: 0.25, h: 0.25 }),
        ] {
            let mut dst = vec![0u8; dw * dh * 3];
            crop_resize(&src, 64, 40, r, &mut dst, dw, dh);
            assert!(dst.chunks(3).all(|p| p == [200, 90, 17]), "{dw}x{dh} {r:?} changed a flat colour");
        }
    }

    #[test]
    fn a_crop_lands_on_the_right_part_of_the_source() {
        // Left half black, right half white; crop one half and enlarge it 4x. The filter looks a few
        // source pixels past the crop edge (that keeps zoomed edges seamless), so the first output
        // columns beside the old edge may blend a little; everything inside the crop must be clean.
        let (w, h) = (32, 16);
        let src = image(w, h, |x, _| if x >= w / 2 { [255; 3] } else { [0; 3] });
        let mut dst = vec![0u8; 64 * 32 * 3];
        crop_resize(&src, w, h, Rect { x: 0.5, y: 0.0, w: 0.5, h: 1.0 }, &mut dst, 64, 32);
        for y in 0..32 {
            for x in 12..64 {
                assert!(px(&dst, 64, x, y)[0] >= 250, "white half, pixel ({x},{y}) = {}", px(&dst, 64, x, y)[0]);
            }
        }
        crop_resize(&src, w, h, Rect { x: 0.0, y: 0.0, w: 0.5, h: 1.0 }, &mut dst, 64, 32);
        for y in 0..32 {
            for x in 0..52 {
                assert!(px(&dst, 64, x, y)[0] <= 5, "black half, pixel ({x},{y}) = {}", px(&dst, 64, x, y)[0]);
            }
        }
    }

    #[test]
    fn a_horizontal_ramp_is_reproduced_at_the_right_positions_when_enlarged() {
        let (w, h) = (100, 4);
        let src = image(w, h, |x, _| [(x * 255 / (w - 1)) as u8; 3]);
        // enlarge the middle fifth (x 40..60) to 200 px wide
        let mut dst = vec![0u8; 200 * 4 * 3];
        crop_resize(&src, w, h, Rect { x: 0.4, y: 0.0, w: 0.2, h: 1.0 }, &mut dst, 200, 4);
        // output pixel i centre is source x = 40 + (i + .5) / 10; the ramp there is that x * 255/99
        for i in [10usize, 60, 100, 150, 190] {
            let expect = ((40.0 + (i as f64 + 0.5) / 10.0 - 0.5) * 255.0 / 99.0).round() as i32;
            let got = px(&dst, 200, i, 1)[0] as i32;
            assert!((got - expect).abs() <= 3, "x={i}: got {got}, expected about {expect}");
        }
    }

    #[test]
    fn shrinking_a_fine_checkerboard_averages_it_instead_of_aliasing() {
        let src = image(64, 64, |x, y| if (x + y) % 2 == 0 { [255; 3] } else { [0; 3] });
        let mut dst = vec![0u8; 16 * 16 * 3];
        crop_resize(&src, 64, 64, FULL, &mut dst, 16, 16);
        // the middle of the picture (away from the edge) should be mid-grey, not black or white
        for y in 4..12 {
            for x in 4..12 {
                let v = px(&dst, 16, x, y)[0] as i32;
                assert!((v - 128).abs() <= 12, "({x},{y}) = {v}: a 4x shrink of a 1px checkerboard should average to grey");
            }
        }
    }

    #[test]
    fn a_hard_edge_stays_crisp_when_enlarged() {
        // 2x enlargement of a vertical black/white edge: the transition must be at most a few pixels wide
        let src = image(32, 8, |x, _| if x < 16 { [0; 3] } else { [255; 3] });
        let mut dst = vec![0u8; 64 * 16 * 3];
        crop_resize(&src, 32, 8, FULL, &mut dst, 64, 16);
        let row: Vec<i32> = (0..64).map(|x| px(&dst, 64, x, 8)[0] as i32).collect();
        let soft = row.iter().filter(|&&v| v > 20 && v < 235).count();
        assert!(soft <= 3, "{soft} pixels sit mid-way across the edge: {row:?}");
        assert!(row[10] <= 10 && row[54] >= 245, "away from the edge it is pure black / white");
    }

    #[test]
    fn working_buffers_are_reused_and_results_do_not_depend_on_what_ran_before() {
        let a = image(40, 30, |x, y| [(x * 6) as u8, (y * 8) as u8, 77]);
        let big = image(120, 90, |x, y| [(x % 255) as u8, (y % 255) as u8, 3]);
        let mut r = Resampler::default();
        let mut first = vec![0u8; 50 * 40 * 3];
        r.crop_resize(&a, 40, 30, FULL, &mut first, 50, 40);
        let mut dst_big = vec![0u8; 20 * 20 * 3];
        r.crop_resize(&big, 120, 90, Rect { x: 0.1, y: 0.1, w: 0.6, h: 0.6 }, &mut dst_big, 20, 20);
        let mut again = vec![0u8; 50 * 40 * 3];
        r.crop_resize(&a, 40, 30, FULL, &mut again, 50, 40);
        assert_eq!(first, again);
    }
}

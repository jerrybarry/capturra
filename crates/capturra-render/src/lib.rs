//! The exporter. Decodes the recording with ffmpeg, crops and zooms every frame along the zoom timeline with a
//! Lanczos resampler, redraws the cursor (the real macOS cursor sprites, click ripples, the hand over clickable
//! things) from the recorded event data, puts the picture on a framed canvas (gradient, wallpaper or colour,
//! padding, rounded corners, shadow), and encodes with x264 or the hardware encoder. The sound is carried over
//! with the Audio tab's changes applied.
//!
//! It runs on the CPU, split across the cores. `CAPTURRA_RENDER_PROFILE=1` prints where a render spends its time.
//! A GPU compositor is possible later; the data flow (timeline -> viewport per frame) would stay the same.
#![allow(clippy::too_many_arguments)] // the drawing helpers take a buffer, its size, a position and a style

use anyhow::{bail, Context, Result};
use capturra_core::{CursorTimeline, CursorTrack, EventLog, InputEvent, Rect, ZoomTimeline};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod frame;
mod resample;
mod sprite;

pub use frame::{cover_rect, Backdrop, FrameStyle, Picture};
pub use resample::{crop_resize, Resampler};

/// How the redrawn cursor looks. The default is what the preview has always drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CursorStyle {
    /// Multiplier on the cursor's size (1.0 = its real size in the recording).
    pub size: f64,
    /// Time for the smoothed cursor to cover half the distance to the real one.
    pub half_life_ms: f64,
    /// Expanding ring where a click lands.
    pub ripple: bool,
    /// The cursor shrinks slightly while the button is held.
    pub squish: bool,
    /// Show the pointing hand while the pointer is over something clickable (needs a recording that noted
    /// it; older ones have nothing to go by and show the cursor as recorded).
    pub hand: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self { size: 1.0, half_life_ms: 60.0, ripple: true, squish: false, hand: true }
    }
}

/// How long after a press the cursor stays squished.
const SQUISH_MS: u64 = 140;
const SQUISH_SCALE: f64 = 0.86;

/// Whether a button press began within the last `SQUISH_MS` before `t_ms`.
fn is_pressed(downs: &[u64], t_ms: u64) -> bool {
    downs.iter().any(|&d| d <= t_ms && t_ms < d + SQUISH_MS)
}

pub struct PreviewOptions {
    /// Output width in pixels. 0 = same as the source video, which is what you want:
    /// zooming then crops real full-resolution pixels instead of stretching a shrunken copy.
    pub width: u32,
    pub fps: u32,
    pub draw_cursor: bool,
    pub cursor: CursorStyle,
    /// Draw the recording inside a padded, rounded, shadowed canvas. `width` is then the canvas's
    /// width. `None` = the picture fills the whole output, as the preview always did.
    pub frame: Option<FrameStyle>,
    /// Directory the log's cursor image files are relative to (the one holding `events.json`).
    /// `None`, or a log without cursor images, draws the fallback circle.
    pub assets_dir: Option<PathBuf>,
    /// Render only `(start_ms, end_ms)` of the video (video time). Events and the zoom timeline
    /// stay on the full-video clock; the output starts at `start_ms`.
    pub trim: Option<(u64, u64)>,
    /// An ffmpeg audio filter chain for the recording's sound (enhance, volume, fades...), applied after
    /// the trim, so times in it count from the start of the output. `None` = the sound as recorded.
    pub audio_filter: Option<String>,
    /// Leave the sound out of the output.
    pub mute_audio: bool,
    /// Which encoder makes the video. `VideoToolbox` becomes x264 if the hardware encoder is not there.
    pub encoder: VideoEncoder,
    /// Called with the fraction done (0..=1) about once per percent.
    pub on_progress: Option<Box<dyn Fn(f64) + Send + Sync>>,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            width: 0,
            fps: 30,
            draw_cursor: true,
            cursor: CursorStyle::default(),
            frame: None,
            assets_dir: None,
            trim: None,
            audio_filter: None,
            mute_audio: false,
            encoder: VideoEncoder::X264,
            on_progress: None,
        }
    }
}

/// Frames a render of `[start_ms, end_ms)` at `fps` produces, never zero (it is a divisor).
fn expected_frames(start_ms: u64, end_ms: u64, fps: u32) -> u64 {
    (end_ms.saturating_sub(start_ms) * fps as u64 / 1000).max(1)
}

fn probe_size(input: &Path) -> Result<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
        .arg(input)
        .output()
        .context("failed to run ffprobe (is ffmpeg installed? `brew install ffmpeg`)")?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split(',');
    let w = it.next().and_then(|v| v.parse().ok());
    let h = it.next().and_then(|v| v.parse().ok());
    match (w, h) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => bail!("could not read video size from ffprobe output: {s:?}"),
    }
}

/// How the finished video is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoEncoder {
    /// x264, medium preset, CRF 14: the smallest file for the quality, and the slowest.
    #[default]
    X264,
    /// The Mac's hardware H.264 encoder (VideoToolbox): several times faster, about 3x the file size at the
    /// same quality. Falls back to x264 where it is not available.
    VideoToolbox,
}

/// The `ffmpeg` output arguments for `encoder` at a canvas size and frame rate.
pub fn encoder_args(encoder: VideoEncoder, canvas_w: usize, canvas_h: usize, fps: u32) -> Vec<String> {
    let tail = ["-pix_fmt", "yuv420p", "-movflags", "+faststart"];
    let mut args: Vec<String> = match encoder {
        VideoEncoder::X264 => ["-c:v", "libx264", "-preset", "medium", "-crf", "14"].iter().map(|s| s.to_string()).collect(),
        VideoEncoder::VideoToolbox => {
            // ~0.45 bits per pixel matches x264 CRF 14 on screen recordings (SSIM 0.9987 against 0.9991) at 28 Mbps for 1080p30
            let kbps = ((canvas_w * canvas_h) as f64 * fps as f64 * 0.45 / 1000.0).clamp(6_000.0, 80_000.0) as u32;
            ["-c:v", "h264_videotoolbox", "-allow_sw", "0", "-b:v"].iter().map(|s| s.to_string()).chain([format!("{kbps}k")]).collect()
        }
    };
    args.extend(tail.iter().map(|s| s.to_string()));
    args
}

/// Whether this Mac can really encode a `w` x `h` picture with VideoToolbox. Asked by encoding a few frames
/// of that exact size (cached per size): the encoder being listed does not mean the hardware is there, and
/// it refuses some sizes outright (480 x 270 fails where 640 x 360 works).
pub fn hardware_encoder_available(w: usize, h: usize) -> bool {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(usize, usize), bool>>> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(&known) = cache.lock().unwrap().get(&(w, h)) {
        return known;
    }
    let works = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            &format!("color=c=black:s={w}x{h}:r=10:d=0.4"),
            "-c:v",
            "h264_videotoolbox",
            "-allow_sw",
            "0",
            "-b:v",
            "3M",
            "-f",
            "null",
            "-",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    cache.lock().unwrap().insert((w, h), works);
    works
}

/// Whether the recording has a sound track (system audio, when it was recorded with `--audio`).
fn has_audio(input: &Path) -> bool {
    Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
        .arg(input)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "audio")
        .unwrap_or(false)
}

/// Length of the video's first stream in milliseconds, as ffprobe reports it (the visible length:
/// the encoder's hidden warm-up samples are not counted).
pub fn probe_duration_ms(input: &Path) -> Result<u64> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=duration", "-of", "csv=p=0"])
        .arg(input)
        .output()
        .context("failed to run ffprobe (is ffmpeg installed? `brew install ffmpeg`)")?;
    let s = String::from_utf8_lossy(&out.stdout);
    match s.trim().parse::<f64>() {
        Ok(secs) if secs.is_finite() && secs >= 0.0 => Ok((secs * 1000.0).round() as u64),
        _ => bail!("could not read the video duration from ffprobe output: {s:?}"),
    }
}

pub fn render_preview(input: &Path, log: &EventLog, timeline: &ZoomTimeline, out: &Path, opts: &PreviewOptions) -> Result<()> {
    let (sw, sh) = probe_size(input)?;
    let out_w = if opts.width == 0 { sw } else { opts.width };
    // The picture (what is cropped, zoomed and drawn on) and the output canvas. They are the same
    // size unless a frame puts the picture inside a padded canvas.
    let (w, h, canvas_w, canvas_h, compositor) = match &opts.frame {
        Some(style) => {
            let l = frame::layout(style, sw as usize, sh as usize, out_w as usize);
            (l.w, l.h, l.canvas_w, l.canvas_h, Some(frame::Compositor::new(style, l)))
        }
        None => {
            let w = (out_w & !1) as usize;
            let h = ((out_w as f64 * sh as f64 / sw as f64).round() as usize) & !1;
            (w, h, w, h, None)
        }
    };
    let fps = opts.fps;
    let (start_ms, end_ms) = match opts.trim {
        Some((a, b)) if b > a => (a, b),
        Some((a, b)) => bail!("trim end ({b} ms) must be after its start ({a} ms)"),
        None => (0, probe_duration_ms(input)?),
    };
    let total_frames = expected_frames(start_ms, end_ms, fps);
    let mut last_pct = u64::MAX;

    let mut dec = Command::new("ffmpeg");
    dec.args(["-v", "error"]);
    if opts.trim.is_some() {
        dec.args(["-ss", &format!("{:.3}", start_ms as f64 / 1000.0), "-t", &format!("{:.3}", (end_ms - start_ms) as f64 / 1000.0)]);
    }
    let mut dec = dec
        .arg("-i")
        .arg(input)
        // the frames come out at the recording's own size: the crop and resize to the output happen once, from the original pixels
        .args(["-vf", &format!("fps={fps}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdout(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg decoder")?;
    let mut enc = Command::new("ffmpeg");
    enc.args([
        "-y",
        "-v",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        &format!("{canvas_w}x{canvas_h}"),
        "-r",
        &fps.to_string(),
        "-i",
        "-",
    ]);
    if !opts.mute_audio && has_audio(input) {
        // the sound of the same range as the picture, copied from the recording
        enc.args(["-ss", &format!("{:.3}", start_ms as f64 / 1000.0), "-t", &format!("{:.3}", (end_ms - start_ms) as f64 / 1000.0), "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-b:a", "192k", "-af"])
            .arg(match &opts.audio_filter {
                Some(chain) => format!("{chain},apad"),
                None => "apad".to_string(),
            })
            .arg("-shortest");
    }
    let encoder = match opts.encoder {
        VideoEncoder::VideoToolbox if !hardware_encoder_available(canvas_w, canvas_h) => {
            eprintln!("note: the hardware encoder cannot encode {canvas_w}x{canvas_h} here, so the video is encoded with x264 instead");
            VideoEncoder::X264
        }
        other => other,
    };
    let mut enc = enc
        .args(encoder_args(encoder, canvas_w, canvas_h, fps))
        .arg(out)
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to start ffmpeg encoder (needs libx264)")?;

    let mut dec_out = dec.stdout.take().unwrap();
    let mut enc_in = enc.stdin.take().unwrap();

    let track = CursorTrack::build(log, log.duration_ms() + 2000, 4, opts.cursor.half_life_ms);
    let clicks: Vec<(u64, f64, f64)> = log
        .sorted()
        .into_iter()
        .filter_map(|e| match e {
            // a click outside the captured area has nothing to ripple on
            InputEvent::Down { t_ms, x, y, .. } if log.contains(x, y) => {
                let (nx, ny) = log.normalize(x, y);
                Some((t_ms, nx, ny))
            }
            _ => None,
        })
        .collect();

    let downs: Vec<u64> = clicks.iter().map(|c| c.0).collect();
    let style = opts.cursor;
    let sprites = match &opts.assets_dir {
        Some(dir) => sprite::load_all(log, dir),
        None => Default::default(),
    };
    let cursor_timeline = CursorTimeline::new(log);
    // Output pixels per point at zoom 1: the video's pixels-per-point, scaled to the output size.
    let px_per_pt = log.display.scale * (w as f64 / sw as f64);

    let (sw_px, sh_px) = (sw as usize, sh as usize);
    let mut src = vec![0u8; sw_px * sh_px * 3];
    let mut dst = vec![0u8; w * h * 3];
    let mut resampler = Resampler::default();
    let mut canvas = vec![0u8; canvas_w * canvas_h * 3];
    let mut idx: u64 = 0;
    let mut prof = Profile::default();
    let started = std::time::Instant::now();

    loop {
        let mark = std::time::Instant::now();
        match dec_out.read_exact(&mut src) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        prof.read += mark.elapsed();
        let t_ms = start_ms + idx * 1000 / fps as u64;
        let vp = timeline.sample(t_ms);
        let rect = vp.rect();
        let mark = std::time::Instant::now();
        resampler.crop_resize(&src, sw_px, sh_px, rect, &mut dst, w, h);
        prof.resample += mark.elapsed();

        let mark = std::time::Instant::now();
        if opts.draw_cursor {
            let squish = if style.squish && is_pressed(&downs, t_ms) { SQUISH_SCALE } else { 1.0 };
            let base = w as f64 * 0.009 * vp.scale * style.size * squish;
            for &(tc, cx, cy) in clicks.iter().filter(|_| style.ripple) {
                if t_ms >= tc && t_ms < tc + 450 {
                    let p = (t_ms - tc) as f64 / 450.0;
                    let (ox, oy) = to_out(cx, cy, rect, w, h);
                    draw_ring(&mut dst, w, h, ox, oy, base * (1.0 + 3.0 * p), 2.5, [80, 160, 255], (1.0 - p) as f32);
                }
            }
            let (nx, ny) = track.at(t_ms);
            let (ox, oy) = to_out(nx, ny, rect, w, h);
            let image = cursor_timeline.styled_at(t_ms, style.hand).and_then(|c| sprites.get(&c.id).map(|s| (c, s)));
            // A pointer outside the captured area is not in the video: draw no cursor at all,
            // rather than one pinned to the nearest edge.
            if track.visible_at(t_ms) {
                match image {
                    Some((c, s)) => {
                        sprite::draw_sprite(&mut dst, w, h, s, c.size, c.hotspot, ox, oy, px_per_pt * vp.scale * style.size * squish);
                    }
                    None => {
                        draw_disc(&mut dst, w, h, ox, oy, base + 2.0, [0, 0, 0], 0.9);
                        draw_disc(&mut dst, w, h, ox, oy, base, [255, 255, 255], 1.0);
                    }
                }
            }
        }

        prof.cursor += mark.elapsed();

        match &compositor {
            Some(c) => {
                let mark = std::time::Instant::now();
                c.compose(&dst, &mut canvas);
                prof.compose += mark.elapsed();
                let mark = std::time::Instant::now();
                enc_in.write_all(&canvas)?;
                prof.write += mark.elapsed();
            }
            None => {
                let mark = std::time::Instant::now();
                enc_in.write_all(&dst)?;
                prof.write += mark.elapsed();
            }
        }
        idx += 1;
        if let Some(report) = &opts.on_progress {
            let pct = (idx * 100 / total_frames).min(99);
            if pct != last_pct {
                last_pct = pct;
                report(pct as f64 / 100.0);
            }
        }
    }

    drop(enc_in);
    dec.wait()?;
    let status = enc.wait()?;
    if !status.success() {
        bail!("ffmpeg encoder exited with {status}");
    }
    if std::env::var_os("CAPTURRA_RENDER_PROFILE").is_some() {
        eprintln!("{}", prof.summary(idx, fps, started.elapsed()));
    }
    Ok(())
}

/// Where the render loop spends its time (printed when `CAPTURRA_RENDER_PROFILE` is set).
#[derive(Default)]
struct Profile {
    /// Waiting for the decoder's next frame.
    read: std::time::Duration,
    resample: std::time::Duration,
    /// Drawing the pointer, ripples and clicks onto the frame.
    cursor: std::time::Duration,
    /// Placing the picture on the framed canvas.
    compose: std::time::Duration,
    /// Handing the frame to the encoder (blocks while the encoder is busy).
    write: std::time::Duration,
}

impl Profile {
    fn summary(&self, frames: u64, fps: u32, total: std::time::Duration) -> String {
        let video_s = frames as f64 / fps.max(1) as f64;
        let pct = |d: std::time::Duration| 100.0 * d.as_secs_f64() / total.as_secs_f64().max(1e-9);
        format!(
            "render: {frames} frames ({video_s:.1} s of video) in {:.1} s = {:.2} s of work per second of video\n  \
             waiting for the decoder {:>5.1}%  resample {:>5.1}%  pointer {:>5.1}%  frame/background {:>5.1}%  waiting for the encoder {:>5.1}%",
            total.as_secs_f64(),
            total.as_secs_f64() / video_s.max(1e-9),
            pct(self.read),
            pct(self.resample),
            pct(self.cursor),
            pct(self.compose),
            pct(self.write),
        )
    }
}

fn to_out(nx: f64, ny: f64, r: Rect, w: usize, h: usize) -> (f64, f64) {
    ((nx - r.x) / r.w * w as f64, (ny - r.y) / r.h * h as f64)
}

fn blend(buf: &mut [u8], w: usize, x: usize, y: usize, color: [u8; 3], a: f32) {
    let i = (y * w + x) * 3;
    for c in 0..3 {
        buf[i + c] = (buf[i + c] as f32 * (1.0 - a) + color[c] as f32 * a) as u8;
    }
}

fn draw_disc(buf: &mut [u8], w: usize, h: usize, cx: f64, cy: f64, r: f64, color: [u8; 3], a: f32) {
    let (x0, x1) = ((cx - r - 1.0).max(0.0) as usize, ((cx + r + 1.0) as usize).min(w.saturating_sub(1)));
    let (y0, y1) = ((cy - r - 1.0).max(0.0) as usize, ((cy + r + 1.0) as usize).min(h.saturating_sub(1)));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            if d <= r {
                blend(buf, w, x, y, color, a);
            }
        }
    }
}

fn draw_ring(buf: &mut [u8], w: usize, h: usize, cx: f64, cy: f64, r: f64, thick: f64, color: [u8; 3], a: f32) {
    let m = r + thick + 1.0;
    let (x0, x1) = ((cx - m).max(0.0) as usize, ((cx + m) as usize).min(w.saturating_sub(1)));
    let (y0, y1) = ((cy - m).max(0.0) as usize, ((cy + m) as usize).min(h.saturating_sub(1)));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            if (d - r).abs() <= thick / 2.0 {
                blend(buf, w, x, y, color, a);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn squish_lasts_only_a_moment_after_a_press() {
        let downs = [1000, 5000];
        assert!(!is_pressed(&downs, 999));
        assert!(is_pressed(&downs, 1000) && is_pressed(&downs, 1139));
        assert!(!is_pressed(&downs, 1140) && !is_pressed(&downs, 3000));
        assert!(is_pressed(&downs, 5050));
        assert!(!is_pressed(&[], 10));
    }

    #[test]
    fn the_default_cursor_style_is_what_the_preview_always_drew() {
        assert_eq!(CursorStyle::default(), CursorStyle { size: 1.0, half_life_ms: 60.0, ripple: true, squish: false, hand: true });
    }

    #[test]
    fn expected_frames_counts_the_trimmed_span_and_is_never_zero() {
        assert_eq!(expected_frames(0, 2000, 30), 60);
        assert_eq!(expected_frames(1000, 3500, 30), 75);
        assert_eq!(expected_frames(500, 500, 30), 1);
        assert_eq!(expected_frames(900, 100, 30), 1);
    }

    #[test]
    fn cursor_is_drawn_inside_the_area_and_absent_when_the_pointer_is_outside() {
        use capturra_core::{DisplayInfo, ZoomConfig};
        let dir = std::env::temp_dir().join(format!("capturra-hide-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("src.mp4"), dir.join("out.mp4"));
        let ok = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=1", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .expect("ffmpeg is needed for tests")
            .success();
        assert!(ok);
        // Pointer in the middle until 500 ms, then 50 points left of the area, where it stays.
        let mut log = EventLog::new(DisplayInfo { width: 640.0, height: 360.0, scale: 1.0 });
        log.events.push(InputEvent::Move { t_ms: 0, x: 320.0, y: 180.0 });
        log.events.push(InputEvent::Move { t_ms: 500, x: -50.0, y: 180.0 });
        let tl = ZoomTimeline::new(&log, ZoomConfig::default());
        render_preview(
            &src,
            &log,
            &tl,
            &out,
            &PreviewOptions { width: 0, fps: 30, draw_cursor: true, assets_dir: None, ..Default::default() },
        )
        .unwrap();

        let raw = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&out)
            .args(["-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .unwrap()
            .stdout;
        let frame = |i: usize| &raw[i * 640 * 360..(i + 1) * 640 * 360];
        let bright = |f: &[u8]| f.iter().filter(|&&p| p > 200).count();
        assert_eq!(raw.len() % (640 * 360), 0);
        assert!(bright(frame(6)) > 20, "cursor should be drawn at 0.2 s (inside)");
        for i in 20..29 {
            assert_eq!(bright(frame(i)), 0, "frame {i}: nothing may be drawn while the pointer is outside");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A recording with sound keeps it through a render, cut to the same range as the picture.
    #[test]
    fn a_render_keeps_the_recordings_sound_cut_to_the_trim() {
        use capturra_core::{DisplayInfo, ZoomConfig};
        let dir = std::env::temp_dir().join(format!("capturra-sound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("src.mp4"), dir.join("out.mp4"));
        assert!(Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=640x360:r=30:d=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3"
            ])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&src)
            .status()
            .expect("ffmpeg is needed for tests")
            .success());
        let log = EventLog::new(DisplayInfo { width: 640.0, height: 360.0, scale: 1.0 });
        let tl = ZoomTimeline::new(&log, ZoomConfig::default());
        let opts_no_trim = || PreviewOptions { width: 0, fps: 30, draw_cursor: false, assets_dir: None, ..Default::default() };
        let opts = PreviewOptions { trim: Some((1000, 2000)), ..opts_no_trim() };
        render_preview(&src, &log, &tl, &out, &opts).unwrap();
        assert!(has_audio(&out), "the render lost the sound");
        let dur = |sel: &str| -> f64 {
            let o = Command::new("ffprobe")
                .args(["-v", "error", "-select_streams", sel, "-show_entries", "stream=duration", "-of", "csv=p=0"])
                .arg(&out)
                .output()
                .unwrap();
            String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
        };
        assert!((dur("v:0") - 1.0).abs() < 0.1, "video {}", dur("v:0"));
        assert!((dur("a:0") - 1.0).abs() < 0.1, "audio {}", dur("a:0"));
        // the Audio tab's filter chain is applied after the trim, and "remove sound" leaves none
        let quiet = dir.join("quiet.mp4");
        render_preview(&src, &log, &tl, &quiet, &PreviewOptions { audio_filter: Some("volume=-20dB".into()), ..opts_no_trim() }).unwrap();
        let level = |f: &Path| -> f64 {
            let o = Command::new("ffmpeg")
                .args(["-nostats", "-i"])
                .arg(f)
                .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
                .output()
                .unwrap();
            let log = String::from_utf8_lossy(&o.stderr).to_string();
            log.lines()
                .find_map(|l| l.split("mean_volume: ").nth(1).map(|r| r.split_whitespace().next().unwrap().parse().unwrap()))
                .unwrap()
        };
        let plain = dir.join("plain.mp4");
        render_preview(&src, &log, &tl, &plain, &opts_no_trim()).unwrap();
        assert!((level(&plain) - level(&quiet) - 20.0).abs() < 1.5, "{} dB vs {} dB", level(&plain), level(&quiet));
        let silent_out = dir.join("mute.mp4");
        render_preview(&src, &log, &tl, &silent_out, &PreviewOptions { mute_audio: true, ..opts_no_trim() }).unwrap();
        assert!(!has_audio(&silent_out), "mute_audio must leave no sound track");
        // and a recording without sound still renders without any
        let silent = dir.join("silent.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=1", "-pix_fmt", "yuv420p"])
            .arg(&silent)
            .status()
            .unwrap()
            .success());
        let out2 = dir.join("out2.mp4");
        render_preview(
            &silent,
            &log,
            &tl,
            &out2,
            &PreviewOptions { width: 0, fps: 30, draw_cursor: false, assets_dir: None, ..Default::default() },
        )
        .unwrap();
        assert!(!has_audio(&out2));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The pointing hand replaces the arrow while the pointer is over something clickable, and only
    /// when the style says so.
    #[test]
    fn the_hand_shows_over_clickable_things_when_the_style_asks_for_it() {
        use capturra_core::{CursorImage, DisplayInfo, Hotspot, SizePt, ZoomConfig};
        let dir = std::env::temp_dir().join(format!("capturra-hand-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("cursors")).unwrap();
        let png = |name: &str, colour: &str| {
            assert!(Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi", "-i", &format!("color=c={colour}:s=24x24,format=rgba"), "-frames:v", "1"])
                .arg(dir.join("cursors").join(name))
                .status()
                .expect("ffmpeg is needed for tests")
                .success());
        };
        png("0.png", "red");
        png("1.png", "lime");
        let src = dir.join("src.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=640x360:r=30:d=1.2", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .unwrap()
            .success());
        let image = |id: u32, role: Option<&str>| CursorImage {
            id,
            file: format!("cursors/{id}.png"),
            hotspot: Hotspot { x: 0.0, y: 0.0 },
            scale: 1.0,
            size: SizePt { w: 24.0, h: 24.0 },
            role: role.map(str::to_string),
        };
        let mut log = EventLog::new(DisplayInfo { width: 640.0, height: 360.0, scale: 1.0 });
        log.cursors = vec![image(0, None), image(1, Some(CursorImage::POINTING_HAND))];
        log.events = vec![
            InputEvent::Move { t_ms: 0, x: 320.0, y: 180.0 },
            InputEvent::CursorChange { t_ms: 0, id: 0 },
            InputEvent::Hover { t_ms: 500, clickable: true },
        ];
        let tl = ZoomTimeline::new(&log, ZoomConfig::default());
        let render = |hand: bool, name: &str| {
            let out = dir.join(name);
            let style = CursorStyle { ripple: false, squish: false, hand, ..Default::default() };
            render_preview(
                &src,
                &log,
                &tl,
                &out,
                &PreviewOptions {
                    width: 0,
                    fps: 30,
                    draw_cursor: true,
                    cursor: style,
                    assets_dir: Some(dir.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
            let raw = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&out)
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
                .output()
                .unwrap()
                .stdout;
            // (red pixels, green pixels) in the box where the cursor sits, for one frame
            move |frame: usize| {
                let f = &raw[frame * 640 * 360 * 3..(frame + 1) * 640 * 360 * 3];
                let (mut red, mut green) = (0, 0);
                for y in 182..202 {
                    for x in 322..342 {
                        let p = &f[(y * 640 + x) * 3..(y * 640 + x) * 3 + 3];
                        if p[0] > 150 && p[1] < 90 {
                            red += 1;
                        }
                        if p[1] > 150 && p[0] < 90 {
                            green += 1;
                        }
                    }
                }
                (red, green)
            }
        };
        let with = render(true, "with.mp4");
        assert!(with(6).0 > 200 && with(6).1 == 0, "before the hover the arrow shows: {:?}", with(6));
        assert!(with(26).1 > 200 && with(26).0 == 0, "over something clickable the hand shows: {:?}", with(26));
        let without = render(false, "without.mp4");
        assert!(without(26).0 > 200 && without(26).1 == 0, "switched off, the recorded cursor stays: {:?}", without(26));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_encoder_arguments_name_the_encoder_and_scale_the_hardware_bitrate_with_the_picture() {
        let x264 = encoder_args(VideoEncoder::X264, 1920, 1080, 30);
        assert!(x264.windows(2).any(|w| w == ["-c:v", "libx264"]) && x264.windows(2).any(|w| w == ["-crf", "14"]));
        let kbps = |w, h, fps| -> u32 {
            let a = encoder_args(VideoEncoder::VideoToolbox, w, h, fps);
            assert!(a.windows(2).any(|p| p == ["-c:v", "h264_videotoolbox"]) && a.windows(2).any(|p| p == ["-allow_sw", "0"]), "{a:?}");
            let i = a.iter().position(|x| x == "-b:v").unwrap();
            a[i + 1].trim_end_matches('k').parse().unwrap()
        };
        let hd = kbps(1920, 1080, 30);
        assert!((27_000..29_500).contains(&hd), "1080p30 is about 28 Mbps: {hd}");
        assert!(kbps(1920, 1080, 60) > hd * 19 / 10, "twice the frames, about twice the bitrate");
        assert_eq!(kbps(320, 180, 10), 6_000, "a tiny picture still gets a sane floor");
        assert_eq!(kbps(7680, 4320, 60), 80_000, "and a huge one a ceiling");
        for a in [&x264, &encoder_args(VideoEncoder::VideoToolbox, 640, 360, 30)] {
            assert!(a.windows(2).any(|p| p == ["-pix_fmt", "yuv420p"]) && a.contains(&"+faststart".to_string()));
        }
    }

    /// The hardware encoder makes a valid, playable video of the right size where it exists, and asking for it
    /// where it does not still gives a video (x264).
    #[test]
    fn a_render_with_the_hardware_encoder_is_a_valid_video_or_quietly_x264() {
        use capturra_core::{DisplayInfo, ZoomConfig};
        let dir = std::env::temp_dir().join(format!("capturra-hw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, out) = (dir.join("src.mp4"), dir.join("out.mp4"));
        assert!(Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=s=640x360:r=30:d=1", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .expect("ffmpeg is needed for tests")
            .success());
        let log = EventLog::new(DisplayInfo { width: 640.0, height: 360.0, scale: 1.0 });
        let tl = ZoomTimeline::new(&log, ZoomConfig::default());
        let opts = PreviewOptions { width: 0, fps: 30, draw_cursor: false, encoder: VideoEncoder::VideoToolbox, ..Default::default() };
        render_preview(&src, &log, &tl, &out, &opts).unwrap();
        let codec = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,width,height,nb_frames", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&codec.stdout).trim(),
            "h264,640,360,30",
            "hardware available for 640x360: {}",
            hardware_encoder_available(640, 360)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn probe_duration_reports_the_visible_length() {
        let dir = std::env::temp_dir().join(format!("capturra-dur-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=64x64:r=10:d=2", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .expect("ffmpeg is needed for tests")
            .success();
        assert!(ok);
        let ms = probe_duration_ms(&path).unwrap();
        assert!((1900..=2100).contains(&ms), "{ms}");
        std::fs::remove_dir_all(&dir).ok();
    }

    use super::*;
}

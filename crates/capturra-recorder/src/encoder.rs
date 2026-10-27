use crate::clock::Clock;
use anyhow::{bail, Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Smallest frame the VideoToolbox hardware encoder is known to accept. Measured on a
/// Mac: 320x180 fails with "Invalid argument", 640x360 and 1280x720 work. The true
/// minimum may be lower; anything under this uses software x264, which accepts any size.
const VIDEOTOOLBOX_MIN: (u32, u32) = (640, 360);

/// Blank frames written before the first real one so ffmpeg starts and opens its encoder up
/// front. Measured: the encoder opens (~350 ms with VideoToolbox) when the first frame reaches
/// it, and ffmpeg reads ahead two frames before that write blocks, so the stall lands on the
/// third write. Four covers it with a frame to spare.
pub const WARMUP_FRAMES: u32 = 4;

/// A write slower than this is listed individually in the end-of-run profile.
pub const SLOW_WRITE_MS: u64 = 50;
/// At most this many slow writes are kept in full; the rest are only counted.
const MAX_SLOW_LISTED: usize = 40;

/// Pick the H.264 encoder: hardware on macOS when the frame is big enough, else x264.
pub fn codec_for(width: u32, height: u32) -> &'static str {
    if cfg!(target_os = "macos") && width >= VIDEOTOOLBOX_MIN.0 && height >= VIDEOTOOLBOX_MIN.1 {
        "h264_videotoolbox"
    } else {
        "libx264"
    }
}

/// Target bitrate for a frame size, in bits per second. 35 Mbps at 2048x1280, scaled with the
/// pixel count so smaller recordings don't waste bits, within 4..=80 Mbps. Screen content with
/// sharp text needs far more than the 12 Mbps this used to use.
pub fn bitrate_for(width: u32, height: u32) -> u64 {
    const REF_PIXELS: u64 = 2048 * 1280;
    const REF_BPS: u64 = 35_000_000;
    let pixels = width as u64 * height as u64;
    (REF_BPS * pixels / REF_PIXELS).clamp(4_000_000, 80_000_000)
}

/// Decides how many constant-rate video slots are owed at a given moment.
///
/// Slot `i` sits at `i * 1000 / fps` ms after the video origin. A slot is owed once its
/// time is strictly before the instant we are catching up to, so a frame that arrives at
/// time `t` first has every earlier slot filled with the previous picture. Integer math
/// only: `i * 1000 < t * fps`, so there is no float rounding drift over long sessions.
#[derive(Debug)]
pub struct FramePacer {
    fps: u64,
    emitted: u64,
}

impl FramePacer {
    pub fn new(fps: u32) -> Self {
        Self { fps: fps.max(1) as u64, emitted: 0 }
    }

    /// Slots with time `< rel_ms` that have not been handed out yet. Marks them as emitted.
    pub fn take_until(&mut self, rel_ms: u64) -> u64 {
        let owed_total = (rel_ms * self.fps).div_ceil(1000);
        let owed = owed_total.saturating_sub(self.emitted);
        self.emitted += owed;
        owed
    }

    pub fn emitted(&self) -> u64 {
        self.emitted
    }
}

/// How far `Encoder::fill_idle` stays behind the clock: longer than one slot at 30 fps and than
/// the gap between a frame being stamped and being queued.
pub const IDLE_FILL_MARGIN_MS: u64 = 60;

/// Upper edges (ms) of the pipe-write latency histogram; one more bucket catches the rest.
pub const WRITE_BUCKETS_MS: [u64; 6] = [1, 5, 10, 20, 50, 100];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// The very first slot of the recording.
    First,
    /// A slot showing a captured frame for the first time.
    Real,
    /// A slot repeating the previous picture to fill an idle gap.
    Duplicate,
}

/// One slow pipe write, with enough context to see what the pump was doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlowWrite {
    /// Shared-clock time when the write started (0 if no clock was set).
    pub clock_ms: u64,
    /// Which video slot it was.
    pub slot: u64,
    pub took: Duration,
    /// Frames waiting in the capture queue when the write started and when it ended.
    pub queue_before: usize,
    pub queue_after: usize,
    pub kind: WriteKind,
    /// This write was number `burst_index` (1-based) of `burst_len` slots written in one
    /// catch-up. A long burst is how an idle gap gets flushed.
    pub burst_index: u64,
    pub burst_len: u64,
    /// The burst was the padding written when the recording ended.
    pub at_finish: bool,
}

/// What the pipe to ffmpeg did over a recording. A "slot" is one constant-rate video frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EncoderStats {
    /// Slots written: the number of frames in the video (warm-up frames not included).
    pub frames_written: u64,
    /// Slots that showed a captured frame for the first time.
    pub real_writes: u64,
    /// Slots that repeated the previous picture to fill an idle gap.
    pub duplicate_writes: u64,
    /// Captured frames replaced by a newer one before any slot showed them (capture ran faster
    /// than the output rate).
    pub superseded: u64,
    pub write_total: Duration,
    pub write_real: Duration,
    pub write_duplicate: Duration,
    pub write_max: Duration,
    /// Writes that took longer than one slot (`1/fps`), i.e. the encoder fell behind real time.
    pub writes_over_slot: u64,
    pub slot: Duration,
    /// Writes per latency bucket, see `WRITE_BUCKETS_MS`; the last entry is "100 ms or more".
    pub write_histogram: [u64; WRITE_BUCKETS_MS.len() + 1],
    /// Every write over `SLOW_WRITE_MS`, in order (capped; see `slow_writes_total`).
    pub slow_writes: Vec<SlowWrite>,
    pub slow_writes_total: u64,
    /// Time `Encoder::new` spent starting ffmpeg and warming up its encoder.
    pub warmup_total: Duration,
    /// The slowest warm-up write: the encoder-open stall, moved off the recording.
    pub warmup_max_write: Duration,
}

impl EncoderStats {
    fn record_write(&mut self, took: Duration, kind: WriteKind) {
        self.frames_written += 1;
        if kind == WriteKind::Duplicate {
            self.duplicate_writes += 1;
            self.write_duplicate += took;
        } else {
            self.real_writes += 1;
            self.write_real += took;
        }
        self.write_total += took;
        self.write_max = self.write_max.max(took);
        if took > self.slot {
            self.writes_over_slot += 1;
        }
        let ms = took.as_millis() as u64;
        let bucket = WRITE_BUCKETS_MS.iter().position(|&edge| ms < edge).unwrap_or(WRITE_BUCKETS_MS.len());
        self.write_histogram[bucket] += 1;
    }
}

/// Tests that measure real encoders run one at a time: two VideoToolbox sessions starting at
/// once slow each other down and make timing assertions flaky. Each `Encoder` holds this for its
/// lifetime in test builds.
#[cfg(test)]
static ENCODER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Pipes raw BGRA frames into ffmpeg at a constant frame rate.
///
/// Callers stamp each frame from the shared [`crate::Clock`]. The first frame defines video
/// time zero (`origin_ms`). From then on the encoder owns the timeline: ffmpeg is told the
/// input rate and sees no timestamps of its own, so frame N is exactly N/fps seconds in.
/// When the screen is idle and no new frame arrives, the previous frame is written again to
/// fill the gap, and `finish` pads to the end of the recording.
///
/// `new` also warms ffmpeg up (see `WARMUP_FRAMES`): the blank frames are stamped before time
/// zero, so they are encoded, which opens the encoder, but the MP4 edit list hides them. They
/// are in the file as hidden samples, not in the video a player shows.
///
/// On macOS this uses the VideoToolbox hardware encoder. Replace with direct
/// VideoToolbox calls (or AVAssetWriter) when you want to drop the ffmpeg dependency.
pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
    out: PathBuf,
    width: u32,
    height: u32,
    frame_len: usize,
    pacer: FramePacer,
    origin_ms: Option<u64>,
    /// Most recent picture, repeated to fill gaps.
    latest: Vec<u8>,
    /// Whether any slot has shown `latest` yet; later slots showing it are duplicates.
    latest_used: bool,
    stats: EncoderStats,
    clock: Option<Clock>,
    queue_depth: Option<Arc<AtomicUsize>>,
    #[cfg(test)]
    _serial: std::sync::MutexGuard<'static, ()>,
}

pub(crate) fn ffmpeg_bin() -> &'static str {
    static BIN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        if Command::new("ffmpeg").arg("-version").output().is_ok() {
            "ffmpeg"
        } else if std::path::Path::new("/opt/homebrew/bin/ffmpeg").exists() {
            "/opt/homebrew/bin/ffmpeg"
        } else if std::path::Path::new("/usr/local/bin/ffmpeg").exists() {
            "/usr/local/bin/ffmpeg"
        } else if std::path::Path::new("/opt/local/bin/ffmpeg").exists() {
            "/opt/local/bin/ffmpeg"
        } else {
            "ffmpeg"
        }
    })
}

impl Encoder {
    /// Starts ffmpeg and returns once its encoder has been opened by warm-up frames, so the
    /// start-up stall happens here, before the recording clock starts.
    pub fn new(out: &Path, width: u32, height: u32, fps: u32) -> Result<Self> {
        let codec = codec_for(width, height);
        // Frames before time zero get negative timestamps; passthrough keeps them (the default
        // constant-rate mode would drop them) and the MP4 muxer hides them with an edit list.
        let vf = format!("setpts=PTS-{WARMUP_FRAMES}/({fps}*TB),scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p");
        let child = Command::new(ffmpeg_bin())
            .args(["-y", "-v", "error"])
            .args(["-f", "rawvideo", "-pix_fmt", "bgra", "-s", &format!("{width}x{height}")])
            .args(["-framerate", &fps.to_string(), "-i", "-"])
            .args(["-fps_mode", "passthrough", "-vf", &vf])
            .args(["-c:v", codec, "-b:v", &bitrate_for(width, height).to_string(), "-movflags", "+faststart"])
            .arg(out)
            .stdin(Stdio::piped())
            .spawn()
            .context("failed to start ffmpeg (install it with `brew install ffmpeg`)")?;
        Self::with_child(child, out, width, height, fps)
    }

    /// Wrap an already-spawned consumer of raw frames and warm it up. `new` passes ffmpeg; tests
    /// pass a stand-in whose reading they control.
    fn with_child(mut child: Child, out: &Path, width: u32, height: u32, fps: u32) -> Result<Self> {
        let stdin = child.stdin.take();
        let frame_len = (width as usize) * (height as usize) * 4;
        let stats = EncoderStats { slot: Duration::from_micros(1_000_000 / fps.max(1) as u64), ..Default::default() };
        let mut enc = Self {
            child,
            stdin,
            out: out.to_path_buf(),
            width,
            height,
            frame_len,
            pacer: FramePacer::new(fps),
            origin_ms: None,
            latest: Vec::new(),
            latest_used: false,
            stats,
            clock: None,
            queue_depth: None,
            #[cfg(test)]
            _serial: ENCODER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
        };
        enc.warm_up().context("ffmpeg did not accept the warm-up frames (is the encoder available for this frame size?)")?;
        Ok(enc)
    }

    fn warm_up(&mut self) -> Result<()> {
        let started = Instant::now();
        let blank = vec![0u8; self.frame_len];
        let stdin = self.stdin.as_mut().context("encoder already finished")?;
        for _ in 0..WARMUP_FRAMES {
            let t = Instant::now();
            stdin.write_all(&blank)?;
            self.stats.warmup_max_write = self.stats.warmup_max_write.max(t.elapsed());
        }
        self.stats.warmup_total = started.elapsed();
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Time slow writes on the shared clock (otherwise they are stamped 0).
    pub fn set_clock(&mut self, clock: Clock) {
        self.clock = Some(clock);
    }

    /// Where to read the capture queue's depth from, for the slow-write log.
    pub fn set_queue_depth(&mut self, depth: Arc<AtomicUsize>) {
        self.queue_depth = Some(depth);
    }

    pub fn warmup_report(&self) -> (Duration, Duration) {
        (self.stats.warmup_total, self.stats.warmup_max_write)
    }

    /// Shared-clock time of video time zero, once the first frame has been pushed.
    pub fn origin_ms(&self) -> Option<u64> {
        self.origin_ms
    }

    /// Offer a frame captured at `clock_ms` on the shared clock. Frames must arrive in
    /// non-decreasing time order; an out-of-order one is treated as arriving "now".
    pub fn push_frame(&mut self, clock_ms: u64, bgra: &[u8]) -> Result<()> {
        if bgra.len() != self.frame_len {
            bail!("frame size mismatch: got {} bytes, expected {} (row padding / stride? see capture.rs)", bgra.len(), self.frame_len);
        }
        let origin = *self.origin_ms.get_or_insert(clock_ms);
        let rel_ms = clock_ms.saturating_sub(origin);
        // Everything up to this frame's arrival still shows the previous picture.
        self.fill_until(rel_ms, false)?;
        if !self.latest.is_empty() && !self.latest_used {
            self.stats.superseded += 1;
        }
        self.latest.clear();
        self.latest.extend_from_slice(bgra);
        self.latest_used = false;
        Ok(())
    }

    /// Write the slots owed while the screen is idle, a few at a time, instead of all at once
    /// when the next frame finally arrives (a long gap would otherwise be one burst of ~15 ms
    /// writes that overflows the capture queue). Call it when nothing is waiting in the queue.
    /// Stays `IDLE_FILL_MARGIN_MS` behind the clock so a frame stamped just before this call but
    /// not yet queued still replaces the picture for its own slots.
    pub fn fill_idle(&mut self, clock_ms: u64) -> Result<()> {
        let Some(origin) = self.origin_ms else { return Ok(()) };
        let rel_ms = clock_ms.saturating_sub(origin).saturating_sub(IDLE_FILL_MARGIN_MS);
        self.fill_until(rel_ms, false)
    }

    /// Pad with the last picture so the video runs to `end_clock_ms`, then close ffmpeg.
    pub fn finish(mut self, end_clock_ms: u64) -> Result<EncoderStats> {
        if let Some(origin) = self.origin_ms {
            self.fill_until(end_clock_ms.saturating_sub(origin), true)?;
        }
        if !self.latest.is_empty() && !self.latest_used {
            self.stats.superseded += 1; // arrived in the last slot's worth of time, never shown
        }
        drop(self.stdin.take());
        let status = self.child.wait()?;
        if !status.success() {
            bail!("ffmpeg exited with {status}");
        }
        Ok(self.stats)
    }

    /// Stop ffmpeg and delete its output. For a recording that never got a frame.
    pub fn abort(mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.out);
    }

    fn fill_until(&mut self, rel_ms: u64, at_finish: bool) -> Result<()> {
        let first_slot = self.pacer.emitted();
        let owed = self.pacer.take_until(rel_ms);
        if owed == 0 {
            return Ok(());
        }
        // Slots are only owed after the first push (origin is set there), so `latest` is filled.
        let stdin = self.stdin.as_mut().context("encoder already finished")?;
        for i in 0..owed {
            let depth = || self.queue_depth.as_ref().map_or(0, |d| d.load(Ordering::SeqCst));
            let clock_ms = self.clock.map_or(0, |c| c.now_ms());
            let queue_before = depth();
            let started = Instant::now();
            stdin.write_all(&self.latest)?;
            let took = started.elapsed();
            let duplicate = std::mem::replace(&mut self.latest_used, true);
            let kind = if self.stats.frames_written == 0 {
                WriteKind::First
            } else if duplicate {
                WriteKind::Duplicate
            } else {
                WriteKind::Real
            };
            if took >= Duration::from_millis(SLOW_WRITE_MS) {
                self.stats.slow_writes_total += 1;
                if self.stats.slow_writes.len() < MAX_SLOW_LISTED {
                    self.stats.slow_writes.push(SlowWrite {
                        clock_ms,
                        slot: first_slot + i,
                        took,
                        queue_before,
                        queue_after: depth(),
                        kind,
                        burst_index: i + 1,
                        burst_len: owed,
                        at_finish,
                    });
                }
            }
            self.stats.record_write(took, kind);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg").arg("-version").output().is_ok() && Command::new("ffprobe").arg("-version").output().is_ok()
    }

    #[test]
    fn pacer_fills_gap_before_a_late_frame() {
        let mut p = FramePacer::new(30);
        // Slots at 0, 33.3, 66.7 ms are before t=100ms; the slot at exactly 100ms is not.
        assert_eq!(p.take_until(100), 3);
        assert_eq!(p.take_until(100), 0);
        // 1000ms is exactly 30 slots from the origin.
        assert_eq!(p.take_until(1000), 27);
        assert_eq!(p.emitted(), 30);
    }

    #[test]
    fn pacer_does_not_drift_over_an_hour() {
        let mut p = FramePacer::new(60);
        let mut total = 0;
        // Irregular 7ms steps for an hour, then catch up to the exact end.
        for t in (0..=3_600_000u64).step_by(7) {
            total += p.take_until(t);
        }
        total += p.take_until(3_600_000);
        assert_eq!(total, p.emitted());
        assert_eq!(p.emitted(), 216_000);
    }

    #[test]
    fn pacer_zero_time_owes_nothing() {
        let mut p = FramePacer::new(60);
        assert_eq!(p.take_until(0), 0);
        // One millisecond in, slot 0 (at 0ms) is owed.
        assert_eq!(p.take_until(1), 1);
    }

    #[test]
    fn bitrate_is_35_mbps_at_2048x1280_and_scales_within_bounds() {
        assert_eq!(bitrate_for(2048, 1280), 35_000_000);
        // Half the pixels, half the bits.
        assert_eq!(bitrate_for(1024, 1280), 17_500_000);
        // Tiny frames hit the floor, huge ones the ceiling.
        assert_eq!(bitrate_for(320, 180), 4_000_000);
        assert_eq!(bitrate_for(7680, 4320), 80_000_000);
        assert!((30_000_000..=40_000_000).contains(&bitrate_for(2048, 1280)));
    }

    #[test]
    fn codec_choice_by_frame_size() {
        assert_eq!(codec_for(320, 180), "libx264");
        assert_eq!(codec_for(640, 359), "libx264");
        if cfg!(target_os = "macos") {
            assert_eq!(codec_for(640, 360), "h264_videotoolbox");
            assert_eq!(codec_for(2048, 1280), "h264_videotoolbox");
        } else {
            assert_eq!(codec_for(2048, 1280), "libx264");
        }
    }

    fn probe(path: &std::path::Path, entry: &str) -> String {
        let out = Command::new("ffprobe")
            .args(["-v", "error", "-count_frames", "-show_entries", entry, "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Mean gray level (0..255) of the frame at `at_s` seconds.
    fn gray_at(path: &std::path::Path, at_s: f64, w: u32, h: u32) -> f64 {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(path)
            .args(["-ss", &at_s.to_string(), "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .unwrap();
        assert_eq!(out.stdout.len(), (w * h) as usize, "decoded frame at {at_s}s has wrong size");
        out.stdout.iter().map(|&b| b as f64).sum::<f64>() / out.stdout.len() as f64
    }

    /// Dark frame at t=0, bright frame at t=100ms, then silence until the recording ends at
    /// 2000ms. The video must be exactly 2.0s, and must show the bright frame through the gap.
    fn idle_gap_is_filled(w: u32, h: u32, name: &str) {
        let out = std::env::temp_dir().join(name);
        let mut enc = Encoder::new(&out, w, h, 30).unwrap();
        let dark = vec![40u8; (w * h * 4) as usize];
        let bright = vec![200u8; (w * h * 4) as usize];
        // Clock values are offset by 5000 on purpose: video time zero is the first frame.
        enc.push_frame(5000, &dark).unwrap();
        enc.push_frame(5100, &bright).unwrap();
        assert_eq!(enc.origin_ms(), Some(5000));
        let stats = enc.finish(7000).unwrap();
        assert_eq!(stats.frames_written, 60, "{w}x{h}: 2.0s at 30fps is 60 frames");
        // dark: 1 real + 2 duplicates before 100ms; bright: 1 real + 56 duplicates to the end.
        assert_eq!((stats.real_writes, stats.duplicate_writes, stats.superseded), (2, 58, 0));

        assert_eq!(probe(&out, "stream=nb_read_frames"), "60", "{w}x{h}");
        let dur: f64 = probe(&out, "format=duration").parse().unwrap();
        assert!((dur - 2.0).abs() < 0.05, "{w}x{h}: duration {dur}, wanted 2.0");

        let first = gray_at(&out, 0.0, w, h);
        let before_switch = gray_at(&out, 0.05, w, h);
        let after_switch = gray_at(&out, 0.2, w, h);
        let in_gap = gray_at(&out, 1.5, w, h);
        let last = gray_at(&out, 1.95, w, h);
        eprintln!("{w}x{h}: gray 0.0s={first:.0} 0.05s={before_switch:.0} 0.2s={after_switch:.0} 1.5s={in_gap:.0} 1.95s={last:.0}");
        for (label, v, want) in [
            ("t=0", first, 40.0),
            ("t=0.05", before_switch, 40.0),
            ("t=0.2", after_switch, 200.0),
            ("t=1.5 (idle gap)", in_gap, 200.0),
            ("t=1.95 (padded tail)", last, 200.0),
        ] {
            assert!((v - want).abs() < 25.0, "{w}x{h} {label}: gray {v}, wanted ~{want}");
        }
    }

    /// A long idle gap is written as it passes, so the frame ending it owes only a few slots.
    #[test]
    fn idle_time_is_filled_as_it_passes_not_in_one_burst() {
        if !ffmpeg_available() {
            return;
        }
        let out = std::env::temp_dir().join("capturra_idle_fill.mp4");
        let mut enc = Encoder::new(&out, 320, 180, 30).unwrap();
        let dark = vec![40u8; 320 * 180 * 4];
        let bright = vec![200u8; 320 * 180 * 4];
        enc.fill_idle(5000).unwrap(); // before the first frame: nothing is owed
        assert_eq!(enc.stats.frames_written, 0);
        enc.push_frame(5000, &dark).unwrap();
        // The loop ticks every ~20 ms while the screen is idle.
        for t in (5000..7000).step_by(20) {
            enc.fill_idle(t).unwrap();
        }
        let written = enc.stats.frames_written;
        // The last tick is at 1980 ms; minus the 60 ms margin, slots with time < 1920 ms: 58.
        assert_eq!(written, 58, "slots written while idle");
        enc.push_frame(7000, &bright).unwrap();
        let burst = enc.stats.frames_written - written;
        assert_eq!(burst, 2, "the frame ending the gap still owed a burst of {burst} slots, wanted 2");
        let stats = enc.finish(7100).unwrap();
        assert_eq!(stats.frames_written, 63, "2.1 s at 30 fps is 63 frames, none lost or doubled");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn idle_gap_is_filled_small() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg/ffprobe not installed; skipping");
            return;
        }
        idle_gap_is_filled(320, 180, "capturra_gap_small.mp4");
    }

    #[test]
    fn idle_gap_is_filled_large() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg/ffprobe not installed; skipping");
            return;
        }
        // On macOS this exercises the VideoToolbox hardware path.
        idle_gap_is_filled(1280, 720, "capturra_gap_large.mp4");
    }

    /// The warm-up frames are black; the real frames here are gray 200. Check the video shows
    /// none of the former, counts none of them, and that the encoder-open stall was paid in
    /// `new`, not on a recorded write.
    fn warm_up_is_hidden_and_absorbs_the_start_stall(w: u32, h: u32, name: &str) {
        let out = std::env::temp_dir().join(name);
        let mut enc = Encoder::new(&out, w, h, 30).unwrap();
        let (warm_total, warm_slowest) = enc.warmup_report();
        let real = vec![200u8; (w * h * 4) as usize];
        // 30 real slots, a new picture in each (distinct clock times 34 ms apart).
        for i in 0..31u64 {
            enc.push_frame(1000 + i * 34, &real).unwrap();
        }
        let stats = enc.finish(1000 + 30 * 34).unwrap();
        eprintln!(
            "{w}x{h}: warm-up {} ms (slowest write {} ms); recorded writes: max {} ms over {} slots",
            warm_total.as_millis(),
            warm_slowest.as_millis(),
            stats.write_max.as_millis(),
            stats.frames_written
        );

        // Warm-up frames are not counted as frames of the video...
        // 1020 ms at 30 fps is 31 slots; the 4 warm-up frames are on top of that in the pipe.
        assert_eq!(stats.frames_written, 31);
        let visible: u64 = probe(&out, "stream=nb_read_frames").parse().unwrap();
        assert_eq!(visible, stats.frames_written, "{w}x{h}: decoder shows warm-up frames");
        // ...nor shown: the first picture is the real one, and the stream starts at zero.
        let first = gray_at(&out, 0.0, w, h);
        assert!((first - 200.0).abs() < 25.0, "{w}x{h}: first frame gray {first}, the black warm-up is showing");
        let start: f64 = probe(&out, "stream=start_time").parse().unwrap();
        assert!(start.abs() < 0.01, "{w}x{h}: stream start_time {start}");

        // The stall must not land on a recorded write. Without warm-up it measured ~340-380 ms.
        assert!(
            stats.write_max < Duration::from_millis(150),
            "{w}x{h}: a recorded write took {:?}; the encoder-open stall was not absorbed by warm-up",
            stats.write_max
        );
        assert!(stats.slow_writes.is_empty(), "{:?}", stats.slow_writes);
        if cfg!(target_os = "macos") && codec_for(w, h) == "h264_videotoolbox" {
            // And it really happened during warm-up, so the assertion above is not vacuous.
            assert!(
                warm_slowest > Duration::from_millis(150),
                "VideoToolbox opened without a visible stall ({warm_slowest:?}); is the warm-up still needed?"
            );
        }
    }

    #[test]
    fn warm_up_is_hidden_and_absorbs_the_start_stall_small() {
        if !ffmpeg_available() {
            return;
        }
        warm_up_is_hidden_and_absorbs_the_start_stall(320, 180, "capturra_warm_small.mp4");
    }

    #[test]
    fn warm_up_is_hidden_and_absorbs_the_start_stall_large() {
        if !ffmpeg_available() {
            return;
        }
        warm_up_is_hidden_and_absorbs_the_start_stall(1280, 720, "capturra_warm_large.mp4");
    }

    #[test]
    fn a_slow_write_is_logged_with_its_clock_time_queue_depth_and_kind() {
        let (w, h) = (320u32, 180u32);
        let frame_len = (w * h * 4) as usize;
        // A consumer that reads the 4 warm-up frames, the first real slot and one duplicate, then
        // stalls halfway into the next slot for 250 ms before reading on.
        let stall_at = 6 * frame_len + frame_len / 2;
        let child = Command::new("sh")
            .args(["-c", &format!("head -c {stall_at} >/dev/null; sleep 0.25; cat >/dev/null")])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut enc = Encoder::with_child(child, Path::new("unused.mp4"), w, h, 30).unwrap();
        let clock = Clock::start();
        enc.set_clock(clock);
        enc.set_queue_depth(Arc::new(AtomicUsize::new(5)));
        let f = vec![1u8; frame_len];
        enc.push_frame(0, &f).unwrap();
        // 100 ms later: slots 0, 1, 2 are owed (one real, two duplicates, one catch-up of 3).
        enc.push_frame(100, &f).unwrap();
        let stats = enc.finish(100).unwrap();

        assert_eq!(stats.frames_written, 3);
        assert_eq!(stats.slow_writes_total, 1, "{:?}", stats.slow_writes);
        let slow = &stats.slow_writes[0];
        assert_eq!(slow.kind, WriteKind::Duplicate);
        assert_eq!((slow.slot, slow.burst_index, slow.burst_len), (2, 3, 3));
        assert!(!slow.at_finish);
        assert!(slow.took >= Duration::from_millis(150), "{:?}", slow.took);
        assert_eq!((slow.queue_before, slow.queue_after), (5, 5));
        assert!(slow.clock_ms < 5_000, "clock {}", slow.clock_ms);
        // The two fast writes are not listed, but all three are counted and histogrammed.
        assert_eq!(stats.write_histogram.iter().sum::<u64>(), 3);
        assert_eq!(stats.writes_over_slot, 1);
    }

    #[test]
    fn a_slow_first_write_and_end_padding_are_labeled() {
        let (w, h) = (320u32, 180u32);
        let frame_len = (w * h * 4) as usize;
        // Stall right after the warm-up, so the very first recorded slot blocks.
        let child = Command::new("sh")
            .args(["-c", &format!("head -c {} >/dev/null; sleep 0.25; cat >/dev/null", 4 * frame_len + frame_len / 2)])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut enc = Encoder::with_child(child, Path::new("unused.mp4"), w, h, 30).unwrap();
        let f = vec![1u8; frame_len];
        enc.push_frame(0, &f).unwrap();
        enc.push_frame(40, &f).unwrap(); // slots 0, 1 owed: slot 0 is the first write
        let stats = enc.finish(40).unwrap();
        assert_eq!(stats.slow_writes.len(), 1, "{:?}", stats.slow_writes);
        assert_eq!(stats.slow_writes[0].kind, WriteKind::First);
        assert_eq!(stats.slow_writes[0].slot, 0);
        assert_eq!(stats.slow_writes[0].queue_before, 0, "no probe set means depth 0");

        // And padding at the end of the recording is marked as such.
        let child = Command::new("sh")
            .args(["-c", &format!("head -c {} >/dev/null; sleep 0.25; cat >/dev/null", 5 * frame_len + frame_len / 2)])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut enc = Encoder::with_child(child, Path::new("unused.mp4"), w, h, 30).unwrap();
        enc.push_frame(0, &f).unwrap();
        // Nothing arrives; the recording ends 100 ms later: slot 0 real, slots 1 and 2 padding.
        let stats = enc.finish(100).unwrap();
        let pad = stats.slow_writes.iter().find(|s| s.at_finish).expect("a slow padding write");
        assert_eq!(pad.kind, WriteKind::Duplicate);
        assert_eq!((pad.burst_index, pad.burst_len), (2, 3));
    }

    #[test]
    fn abort_stops_ffmpeg_and_removes_the_file() {
        if !ffmpeg_available() {
            return;
        }
        let out = std::env::temp_dir().join("capturra_abort.mp4");
        let enc = Encoder::new(&out, 320, 180, 30).unwrap();
        enc.abort();
        assert!(!out.exists());
    }

    #[test]
    fn counts_real_duplicate_and_superseded_writes_and_times_them() {
        if !ffmpeg_available() {
            return;
        }
        let out = std::env::temp_dir().join("capturra_stats_test.mp4");
        let (w, h) = (320, 180);
        let mut enc = Encoder::new(&out, w, h, 30).unwrap();
        let f = vec![7u8; (w * h * 4) as usize];
        enc.push_frame(0, &f).unwrap(); // A
        enc.push_frame(10, &f).unwrap(); // B: slot 0 (at 0ms) is now owed and shows A -> A real
        enc.push_frame(20, &f).unwrap(); // C: no slot falls in (10,20], so B was never shown
        enc.push_frame(200, &f).unwrap(); // D: slots 1..=5 owed, C real + 4 duplicates
        let stats = enc.finish(300).unwrap(); // slots 6..=8: D real + 2 duplicates
        assert_eq!(stats.frames_written, 9, "300ms at 30fps");
        assert_eq!((stats.real_writes, stats.duplicate_writes, stats.superseded), (3, 6, 1), "{stats:?}");
        // Every write is in exactly one histogram bucket, and totals add up.
        assert_eq!(stats.write_histogram.iter().sum::<u64>(), stats.frames_written);
        assert_eq!(stats.write_real + stats.write_duplicate, stats.write_total);
        assert!(stats.write_max <= stats.write_total);
        assert_eq!(stats.slot, Duration::from_micros(33_333));
    }

    #[test]
    fn a_frame_never_shown_before_the_end_is_counted_as_superseded_at_finish() {
        if !ffmpeg_available() {
            return;
        }
        let out = std::env::temp_dir().join("capturra_stats_tail_test.mp4");
        let mut enc = Encoder::new(&out, 320, 180, 30).unwrap();
        let f = vec![7u8; 320 * 180 * 4];
        enc.push_frame(0, &f).unwrap(); // A
        enc.push_frame(100, &f).unwrap(); // B: slots 0..=2 show A (1 real + 2 duplicates)
                                          // The recording ends at exactly 100ms: no slot is left for B.
        let stats = enc.finish(100).unwrap();
        assert_eq!((stats.frames_written, stats.real_writes, stats.duplicate_writes, stats.superseded), (3, 1, 2, 1));
    }

    #[test]
    fn wrong_size_frame_is_rejected() {
        if !ffmpeg_available() {
            return;
        }
        let out = std::env::temp_dir().join("capturra_bad_size.mp4");
        let mut enc = Encoder::new(&out, 320, 180, 30).unwrap();
        assert!(enc.push_frame(0, &[0u8; 10]).is_err());
    }
}

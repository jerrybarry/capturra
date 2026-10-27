//! The receive side of a recording: pulls timestamped frames off a channel and feeds the
//! encoder. Platform-independent so the stop/timeout behavior can be tested without a screen.

use crate::clock::Clock;
use crate::encoder::{Encoder, EncoderStats, SlowWrite, WriteKind, SLOW_WRITE_MS, WRITE_BUCKETS_MS};
use anyhow::{bail, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::Duration;

/// How often the loop wakes to check for a stop request when no frame arrives.
const POLL: Duration = Duration::from_millis(20);

/// A tightly packed BGRA frame stamped on the shared clock when it arrived.
pub struct TimedFrame {
    pub t_ms: u64,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Depth counters for the queue between the capture thread and the encoder.
#[derive(Default)]
struct QueueStats {
    /// Shared with the encoder so it can note the depth next to each slow write.
    depth: Arc<AtomicUsize>,
    max: AtomicUsize,
    /// Sum of the depth seen at each receive, and how many receives, for the mean.
    depth_sum: AtomicU64,
    receives: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QueueReport {
    pub capacity: usize,
    pub max_depth: usize,
    /// Mean number of frames waiting at the moment the encoder side took one.
    pub mean_depth: f64,
}

/// Sending half: a bounded, non-blocking channel that tracks how full it gets.
#[derive(Clone)]
pub struct FrameSender {
    tx: SyncSender<TimedFrame>,
    stats: Arc<QueueStats>,
}

pub struct FrameReceiver {
    rx: mpsc::Receiver<TimedFrame>,
    stats: Arc<QueueStats>,
    capacity: usize,
}

/// A queue holding at most `capacity` frames; a send into a full queue fails instead of waiting.
pub fn channel(capacity: usize) -> (FrameSender, FrameReceiver) {
    let (tx, rx) = mpsc::sync_channel(capacity);
    let stats = Arc::new(QueueStats::default());
    (FrameSender { tx, stats: stats.clone() }, FrameReceiver { rx, stats, capacity })
}

impl FrameSender {
    pub fn try_send(&self, frame: TimedFrame) -> Result<(), TrySendError<TimedFrame>> {
        // Count before sending so the receiver can never decrement past zero.
        let depth = self.stats.depth.fetch_add(1, Ordering::SeqCst) + 1;
        match self.tx.try_send(frame) {
            Ok(()) => {
                self.stats.max.fetch_max(depth, Ordering::SeqCst);
                Ok(())
            }
            Err(e) => {
                self.stats.depth.fetch_sub(1, Ordering::SeqCst);
                Err(e)
            }
        }
    }
}

impl FrameReceiver {
    fn recv_timeout(&self, d: Duration) -> Result<TimedFrame, RecvTimeoutError> {
        let frame = self.rx.recv_timeout(d)?;
        let seen = self.stats.depth.fetch_sub(1, Ordering::SeqCst);
        self.stats.depth_sum.fetch_add(seen as u64, Ordering::Relaxed);
        self.stats.receives.fetch_add(1, Ordering::Relaxed);
        Ok(frame)
    }

    /// Live queue depth, for the encoder's slow-write log.
    pub fn depth_probe(&self) -> Arc<AtomicUsize> {
        self.stats.depth.clone()
    }

    pub fn report(&self) -> QueueReport {
        let n = self.stats.receives.load(Ordering::Relaxed);
        QueueReport {
            capacity: self.capacity,
            max_depth: self.stats.max.load(Ordering::SeqCst),
            mean_depth: if n == 0 { 0.0 } else { self.stats.depth_sum.load(Ordering::Relaxed) as f64 / n as f64 },
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct VideoInfo {
    /// Shared-clock time of video time zero (the first frame).
    pub origin_ms: u64,
    pub width: u32,
    /// Clock time from the first frame to the end of the recording.
    pub span_ms: u64,
    pub stats: EncoderStats,
}

fn describe_slow_write(w: &SlowWrite, queue_capacity: usize, fps: u32) -> String {
    let what = match w.kind {
        WriteKind::First => "first write",
        WriteKind::Real => "real frame",
        WriteKind::Duplicate if w.at_finish => "end-padding duplicate",
        WriteKind::Duplicate => "idle-gap flush duplicate",
    };
    let burst = if w.burst_len > 1 {
        let gap = w.burst_len as f64 / fps.max(1) as f64;
        format!(", {} of {} in one catch-up (a {gap:.2} s gap)", w.burst_index, w.burst_len)
    } else {
        String::new()
    };
    format!(
        "clock {:>6} ms (video {:.2} s): {:>4.0} ms, queue {}->{}/{queue_capacity}, {what}{burst}",
        w.clock_ms,
        w.slot as f64 / fps.max(1) as f64,
        w.took.as_secs_f64() * 1000.0,
        w.queue_before,
        w.queue_after,
    )
}

/// A few lines describing where the pipe to ffmpeg spent its time, for the end-of-run summary.
pub fn profile_summary(info: &VideoInfo, queue: &QueueReport, dropped: u64, fps: u32) -> String {
    let st = &info.stats;
    let pct = |part: f64, whole: f64| if whole > 0.0 { 100.0 * part / whole } else { 0.0 };
    let avg_ms = |total: Duration, n: u64| if n == 0 { 0.0 } else { total.as_secs_f64() * 1000.0 / n as f64 };
    let span = info.span_ms as f64 / 1000.0;
    let mut hist = String::new();
    for (i, n) in st.write_histogram.iter().enumerate() {
        let label = match WRITE_BUCKETS_MS.get(i) {
            Some(edge) => format!("<{edge}ms"),
            None => format!(">={}ms", WRITE_BUCKETS_MS[WRITE_BUCKETS_MS.len() - 1]),
        };
        hist.push_str(&format!(" {label}:{n}"));
    }
    let mut slow = String::new();
    if st.slow_writes_total > 0 {
        slow.push_str(&format!(
            "\n  writes over {SLOW_WRITE_MS} ms: {} (clock = shared recording clock at write start)",
            st.slow_writes_total
        ));
        for w in &st.slow_writes {
            slow.push_str(&format!("\n    {}", describe_slow_write(w, queue.capacity, fps)));
        }
        let hidden = st.slow_writes_total as usize - st.slow_writes.len();
        if hidden > 0 {
            slow.push_str(&format!("\n    ... and {hidden} more"));
        }
    } else {
        slow.push_str(&format!("\n  writes over {SLOW_WRITE_MS} ms: none"));
    }
    format!(
        "pipe profile ({fps} fps, {span:.1} s):\n\
         \x20 warm-up:        ffmpeg and its encoder were ready after {:.0} ms, before the clock started (slowest warm-up write {:.0} ms)\n\
         \x20 slots written: {} = {} real + {} duplicate ({:.0}% duplicate); {} captured frame(s) replaced before use\n\
         \x20 write time:    {:.1} s total = {:.0}% of the recording; real {:.1} s ({:.1} ms each), duplicate {:.1} s ({:.1} ms each)\n\
         \x20 per write:     max {:.0} ms; {} of {} longer than one {:.1} ms slot\n\
         \x20 distribution: {hist}\n\
         \x20 queue:         max depth {}/{}, mean {:.1} when taken; {} dropped{slow}",
        st.warmup_total.as_secs_f64() * 1000.0,
        st.warmup_max_write.as_secs_f64() * 1000.0,
        st.frames_written,
        st.real_writes,
        st.duplicate_writes,
        pct(st.duplicate_writes as f64, st.frames_written as f64),
        st.superseded,
        st.write_total.as_secs_f64(),
        pct(st.write_total.as_secs_f64(), span),
        st.write_real.as_secs_f64(),
        avg_ms(st.write_real, st.real_writes),
        st.write_duplicate.as_secs_f64(),
        avg_ms(st.write_duplicate, st.duplicate_writes),
        st.write_max.as_secs_f64() * 1000.0,
        st.writes_over_slot,
        st.frames_written,
        st.slot.as_secs_f64() * 1000.0,
        queue.max_depth,
        queue.capacity,
        queue.mean_depth,
        dropped,
    )
}

/// Run until `stop` is set, `duration` elapses on `clock`, or the sender hangs up.
///
/// Never blocks longer than `POLL` waiting for a frame, so a stop request is honored even
/// when the screen is idle and nothing arrives. `encoder` must already be warmed up (see
/// `Encoder::new`) and sized for the incoming frames. `on_first_frame` fires once, with the
/// first frame, before it is encoded. Returns `None`, deleting the output, if no frame
/// ever arrived.
pub fn run(
    rx: &FrameReceiver,
    clock: Clock,
    stop: &AtomicBool,
    duration: Option<Duration>,
    mut encoder: Encoder,
    mut on_first_frame: impl FnMut(&TimedFrame),
) -> Result<Option<VideoInfo>> {
    encoder.set_clock(clock);
    encoder.set_queue_depth(rx.depth_probe());
    let (width, height) = (encoder.width(), encoder.height());
    let end_ms = loop {
        let now = clock.now_ms();
        if stop.load(Ordering::SeqCst) || duration.is_some_and(|d| now >= d.as_millis() as u64) {
            break now;
        }
        match rx.recv_timeout(POLL) {
            Ok(frame) => {
                if (frame.width, frame.height) != (width, height) {
                    bail!("captured frame is {}x{} but the encoder was set up for {}x{}", frame.width, frame.height, width, height);
                }
                if encoder.origin_ms().is_none() {
                    on_first_frame(&frame);
                }
                encoder.push_frame(frame.t_ms, &frame.data)?;
            }
            Err(RecvTimeoutError::Timeout) => encoder.fill_idle(clock.now_ms())?,
            Err(RecvTimeoutError::Disconnected) => break clock.now_ms(),
        }
    };
    let Some(origin_ms) = encoder.origin_ms() else {
        encoder.abort();
        return Ok(None);
    };
    // The video runs to the end of the recording, not to the last frame that happened to arrive.
    let stats = encoder.finish(end_ms)?;
    Ok(Some(VideoInfo { origin_ms, width, span_ms: end_ms.saturating_sub(origin_ms), stats }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    fn ffprobe_available() -> bool {
        std::process::Command::new("ffprobe").arg("-version").output().is_ok()
            && std::process::Command::new("ffmpeg").arg("-version").output().is_ok()
    }

    fn duration_of(path: &Path) -> f64 {
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
    }

    /// A warmed-up encoder, as `capture.rs` builds one before the clock starts.
    fn encoder(name: &str, w: u32, h: u32) -> (Encoder, std::path::PathBuf) {
        let out = std::env::temp_dir().join(name);
        (Encoder::new(&out, w, h, 30).unwrap(), out)
    }

    #[test]
    fn stop_is_honored_when_no_frame_ever_arrives() {
        if !ffprobe_available() {
            return;
        }
        let (enc, out_path) = encoder("capturra_pump_stop.mp4", 320, 180);
        let (_tx, rx) = channel(4); // sender stays alive and silent: a fully idle screen
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let out = run(&rx, Clock::start(), &stop, None, enc, |_| {}).unwrap();
        let took = started.elapsed();
        assert_eq!(out, None);
        assert!(!out_path.exists(), "a recording with no frames must not leave a video behind");
        assert!(took >= Duration::from_millis(150), "returned before stop: {took:?}");
        assert!(took < Duration::from_millis(1000), "stop took {took:?} with no frames arriving");
    }

    #[test]
    fn duration_ends_the_recording_even_with_no_frames() {
        if !ffprobe_available() {
            return;
        }
        let (enc, _) = encoder("capturra_pump_duration.mp4", 320, 180);
        let (_tx, rx) = channel(4);
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        let out = run(&rx, Clock::start(), &stop, Some(Duration::from_millis(200)), enc, |_| {}).unwrap();
        assert_eq!(out, None);
        assert!(started.elapsed() < Duration::from_millis(1000));
    }

    #[test]
    fn video_runs_to_the_end_after_the_last_frame_and_origin_is_first_frame() {
        if !ffprobe_available() {
            eprintln!("ffmpeg/ffprobe not installed; skipping");
            return;
        }
        // Warm up first, as in a real recording: the clock starts only once ffmpeg is ready.
        let (enc, out) = encoder("capturra_pump_test.mp4", 320, 180);
        let clock = Clock::start();
        let (tx, rx) = channel(4);
        let sender = std::thread::spawn(move || {
            // The first frame arrives 300ms into the session (capture startup), then one more
            // 200ms later, then the screen goes idle.
            std::thread::sleep(Duration::from_millis(300));
            let (w, h) = (320u32, 180u32);
            tx.try_send(TimedFrame { t_ms: clock.now_ms(), width: w, height: h, data: vec![10; (w * h * 4) as usize] }).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            tx.try_send(TimedFrame { t_ms: clock.now_ms(), width: w, height: h, data: vec![250; (w * h * 4) as usize] }).unwrap();
            tx // keep the sender alive: idle, not disconnected
        });
        let stop = AtomicBool::new(false);
        let info = run(&rx, clock, &stop, Some(Duration::from_millis(2300)), enc, |_| {}).unwrap().unwrap();
        let _tx = sender.join().unwrap();

        // Video time zero is where the first frame landed, not where the session began.
        assert!((300..400).contains(&info.origin_ms), "origin {}", info.origin_ms);
        // The recording ran to ~2300ms on the clock, so the video is ~2300 - origin long, even
        // though the last frame arrived ~500ms in.
        let want = (2300 - info.origin_ms) as f64 / 1000.0;
        let dur = duration_of(&out);
        eprintln!("origin {}ms, {} frames, duration {dur:.3}s, wanted ~{want:.3}s", info.origin_ms, info.stats.frames_written);
        assert!((dur - want).abs() < 0.15, "duration {dur}, wanted {want}");
    }

    fn frame() -> TimedFrame {
        TimedFrame { t_ms: 0, width: 1, height: 1, data: vec![0; 4] }
    }

    #[test]
    fn queue_reports_max_and_mean_depth_and_refuses_when_full() {
        let (tx, rx) = channel(3);
        for _ in 0..3 {
            tx.try_send(frame()).unwrap();
        }
        assert!(matches!(tx.try_send(frame()), Err(TrySendError::Full(_))), "bounded queue must refuse the 4th");
        // The refused send must not leave a phantom frame in the depth count.
        for _ in 0..3 {
            rx.recv_timeout(Duration::from_millis(10)).unwrap();
        }
        let r = rx.report();
        assert_eq!((r.capacity, r.max_depth), (3, 3));
        // Depth seen at each receive was 3, 2, 1.
        assert!((r.mean_depth - 2.0).abs() < 1e-9, "{r:?}");
        // Back to empty: another send/receive keeps the depth count sane (no underflow).
        tx.try_send(frame()).unwrap();
        rx.recv_timeout(Duration::from_millis(10)).unwrap();
        assert_eq!(rx.report().max_depth, 3);
    }

    #[test]
    fn empty_queue_reports_zero_mean() {
        let (_tx, rx) = channel(2);
        assert_eq!(rx.report(), QueueReport { capacity: 2, max_depth: 0, mean_depth: 0.0 });
    }

    #[test]
    fn profile_summary_states_the_duplicate_share_and_queue() {
        let mut stats = EncoderStats { slot: Duration::from_micros(33_333), ..Default::default() };
        stats.frames_written = 430;
        stats.real_writes = 230;
        stats.duplicate_writes = 200;
        stats.superseded = 3;
        stats.write_total = Duration::from_secs(10);
        stats.write_real = Duration::from_secs(6);
        stats.write_duplicate = Duration::from_secs(4);
        stats.write_max = Duration::from_millis(140);
        stats.writes_over_slot = 52;
        stats.warmup_total = Duration::from_millis(612);
        stats.warmup_max_write = Duration::from_millis(349);
        stats.slow_writes_total = 3;
        stats.slow_writes = vec![
            SlowWrite {
                clock_ms: 1043,
                slot: 0,
                took: Duration::from_millis(345),
                queue_before: 1,
                queue_after: 8,
                kind: WriteKind::First,
                burst_index: 1,
                burst_len: 1,
                at_finish: false,
            },
            SlowWrite {
                clock_ms: 5210,
                slot: 120,
                took: Duration::from_millis(120),
                queue_before: 3,
                queue_after: 8,
                kind: WriteKind::Duplicate,
                burst_index: 12,
                burst_len: 30,
                at_finish: false,
            },
            SlowWrite {
                clock_ms: 14990,
                slot: 420,
                took: Duration::from_millis(70),
                queue_before: 0,
                queue_after: 0,
                kind: WriteKind::Duplicate,
                burst_index: 2,
                burst_len: 3,
                at_finish: true,
            },
        ];
        stats.slow_writes_total = 5; // two more than are listed
        let info = VideoInfo { origin_ms: 0, width: 2048, span_ms: 14_300, stats };
        let q = QueueReport { capacity: 8, max_depth: 8, mean_depth: 4.25 };
        let text = profile_summary(&info, &q, 19, 30);
        for want in [
            "430 = 230 real + 200 duplicate (47% duplicate)",
            "3 captured frame(s) replaced before use",
            "10.0 s total = 70% of the recording",
            "real 6.0 s (26.1 ms each), duplicate 4.0 s (20.0 ms each)",
            "max 140 ms; 52 of 430 longer than one 33.3 ms slot",
            "max depth 8/8, mean 4.2 when taken; 19 dropped",
            "ready after 612 ms, before the clock started (slowest warm-up write 349 ms)",
            "writes over 50 ms: 5",
            "clock   1043 ms (video 0.00 s):  345 ms, queue 1->8/8, first write",
            "clock   5210 ms (video 4.00 s):  120 ms, queue 3->8/8, idle-gap flush duplicate, 12 of 30 in one catch-up (a 1.00 s gap)",
            "clock  14990 ms (video 14.00 s):   70 ms, queue 0->0/8, end-padding duplicate, 2 of 3 in one catch-up (a 0.10 s gap)",
            "... and 2 more",
        ] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    #[test]
    fn profile_summary_handles_an_empty_recording_without_dividing_by_zero() {
        let info = VideoInfo { origin_ms: 0, width: 1, span_ms: 0, stats: EncoderStats::default() };
        let text = profile_summary(&info, &QueueReport { capacity: 8, max_depth: 0, mean_depth: 0.0 }, 0, 30);
        assert!(text.contains("0 = 0 real + 0 duplicate (0% duplicate)"), "{text}");
        assert!(text.contains("writes over 50 ms: none"), "{text}");
        assert!(!text.contains("NaN") && !text.contains("inf"), "{text}");
    }

    #[test]
    fn a_frame_of_a_different_size_than_the_encoder_is_a_loud_error() {
        if !ffprobe_available() {
            return;
        }
        let (enc, _) = encoder("capturra_pump_mismatch.mp4", 320, 180);
        let (tx, rx) = channel(4);
        tx.try_send(TimedFrame { t_ms: 0, width: 640, height: 360, data: vec![0; 640 * 360 * 4] }).unwrap();
        let stop = AtomicBool::new(false);
        let err = run(&rx, Clock::start(), &stop, Some(Duration::from_millis(500)), enc, |_| {}).unwrap_err();
        assert!(err.to_string().contains("640x360") && err.to_string().contains("320x180"), "{err}");
    }
}

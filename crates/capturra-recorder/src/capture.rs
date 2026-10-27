//! ScreenCaptureKit capture via the `scap` crate. THIS IS THE ONLY FILE THAT
//! TOUCHES `scap`. It was written against scap 0.0.8 and has NOT been compiled
//! on a Mac yet (it was authored on Linux); if the API has drifted, fix it here
//! and nowhere else.

use crate::audio::{self, AudioInput, AudioRecorder, AudioSender};
use crate::clock::{self, Clock};
use crate::cursor::{self, CursorOutput};
use crate::cursor_macos::NsCursorSource;
use crate::encoder::Encoder;
use crate::focus_macos::AxFocusSource;
use crate::geometry::{self, OriginTrack, PtRect, Source};
use crate::hover_macos::AxHoverSource;
use crate::input::{self, InputLogger};
use crate::mic_macos::{self, Mic, MicSummary};
use crate::pump::{self, FrameSender, TimedFrame};
use crate::sources::{self, DisplayGeom};
use crate::{RecordOptions, RecordingPaths};
use anyhow::{anyhow, bail, Result};
use capturra_core::{DisplayInfo, EventLog};
use scap::capturer::{Area, Capturer, Options, Point, Resolution, Size};
use scap::frame::{Frame, FrameType};
use scap::Target;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// What `record` captures, resolved against this Mac's displays and windows.
struct Resolved {
    /// One line for the log: what is being recorded.
    label: String,
    target: Target,
    /// Part of the target to capture (region captures); `None` = all of it.
    crop: Option<Area>,
    /// The captured area in GLOBAL points at the start. Event coordinates are mapped relative to
    /// its top-left corner; a window's origin is re-sampled while recording.
    area: PtRect,
    window: Option<u32>,
    /// Native pixel size of the display holding the area (display and region captures).
    native_px: Option<(u64, u64)>,
}

fn display_target(id: u32) -> Result<Target> {
    scap::get_all_targets()
        .into_iter()
        .find(|t| matches!(t, Target::Display(d) if d.id == id))
        .ok_or_else(|| anyhow!("the capture library does not list display {id}"))
}

fn resolve(source: Source) -> Result<Resolved> {
    let displays = sources::displays()?;
    let pick = |n: usize| -> Result<DisplayGeom> {
        displays.get(n).cloned().ok_or_else(|| {
            anyhow!(
                "--display {n}: this Mac has {} display(s), numbered 0 to {}. Run `capturra record --list`.",
                displays.len(),
                displays.len() - 1
            )
        })
    };
    let describe =
        |n: usize, d: &DisplayGeom| format!("display {n} ({} x {} points at ({}, {}))", d.bounds.w, d.bounds.h, d.bounds.x, d.bounds.y);
    match source {
        Source::Display(n) => {
            let d = pick(n)?;
            Ok(Resolved {
                label: describe(n, &d),
                target: display_target(d.id)?,
                crop: None,
                area: d.bounds,
                window: None,
                native_px: Some(d.pixels),
            })
        }
        Source::Region { display, rect } => {
            let d = pick(display)?;
            let r = geometry::fit_region(rect, d.bounds.w, d.bounds.h).map_err(|e| anyhow!("--region: {e}"))?;
            Ok(Resolved {
                label: format!("region {},{} {} x {} points of {}", r.x, r.y, r.w, r.h, describe(display, &d)),
                target: display_target(d.id)?,
                crop: Some(Area { origin: Point { x: r.x, y: r.y }, size: Size { width: r.w, height: r.h } }),
                area: PtRect::new(d.bounds.x + r.x, d.bounds.y + r.y, r.w, r.h),
                window: None,
                native_px: Some(d.pixels),
            })
        }
        Source::Window(id) => {
            let g = sources::window_geom(id)
                .ok_or_else(|| anyhow!("--window {id}: no such window. Run `capturra record --list` for current ids."))?;
            if !g.on_screen {
                bail!("--window {id} ({} - {:?}) is not on screen (minimized, hidden, or on another Space)", g.owner, g.title);
            }
            let target = scap::get_all_targets().into_iter().find(|t| matches!(t, Target::Window(w) if w.id == id)).ok_or_else(|| {
                anyhow!(
                    "--window {id}: the capture library does not list this window (it lists only windows that \
                         have a title)"
                )
            })?;
            Ok(Resolved {
                label: format!(
                    "window {id}: {} - {:?}, {} x {} points at ({}, {})",
                    g.owner, g.title, g.bounds.w, g.bounds.h, g.bounds.x, g.bounds.y
                ),
                target,
                crop: None,
                area: g.bounds,
                window: Some(id),
                native_px: None,
            })
        }
    }
}

/// Displays and windows that can be recorded, as data.
pub fn source_list() -> Result<crate::SourceList> {
    use crate::{DisplayItem, SourceList, WindowItem};
    if !scap::has_permission() {
        bail!("Screen Recording permission is not granted to this terminal app, so displays and windows cannot be listed. Enable it in System Settings > Privacy & Security > Screen Recording, restart the terminal, then re-run.");
    }
    let targets = scap::get_all_targets();
    let display_name = |id: u32| -> String {
        targets
            .iter()
            .find_map(|t| match t {
                Target::Display(d) if d.id == id => Some(d.title.clone()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let capturable = |id: u32| targets.iter().any(|t| matches!(t, Target::Window(w) if w.id == id));
    let displays = sources::displays()?
        .iter()
        .enumerate()
        .map(|(index, d)| DisplayItem {
            index,
            name: display_name(d.id),
            x: d.bounds.x,
            y: d.bounds.y,
            width: d.bounds.w,
            height: d.bounds.h,
            pixel_width: d.pixels.0,
            pixel_height: d.pixels.1,
            main: d.is_main,
            sharpness: d.sharpness,
        })
        .collect();
    let mut list = SourceList { displays, windows: Vec::new(), untitled_hidden: 0 };
    for w in sources::windows()? {
        if !capturable(w.id) {
            list.untitled_hidden += 1;
            continue;
        }
        list.windows.push(WindowItem {
            id: w.id,
            app: w.owner,
            title: w.title,
            x: w.bounds.x,
            y: w.bounds.y,
            width: w.bounds.w,
            height: w.bounds.h,
        });
    }
    Ok(list)
}

pub fn permissions() -> crate::Permissions {
    let (accessibility, input_monitoring) = input::status();
    crate::Permissions {
        screen_recording: scap::has_permission(),
        accessibility,
        input_monitoring,
        responsible_app: input::responsible_app(),
    }
}

/// The text printed by `capturra record --list`.
pub fn list_sources() -> Result<String> {
    use std::fmt::Write;
    let list = source_list()?;
    let mut out = String::new();
    writeln!(out, "Displays (--display <n>; --region is relative to the chosen display, in points):")?;
    for d in &list.displays {
        writeln!(
            out,
            "  {}  {} x {} pt at ({}, {}), {} x {} px{}  {}",
            d.index,
            d.width,
            d.height,
            d.x,
            d.y,
            d.pixel_width,
            d.pixel_height,
            if d.main { ", main" } else { "" },
            d.name
        )?;
        if d.sharpness == crate::quality::Sharpness::LowResolutionMode {
            writeln!(out, "      note: this display is in a low-resolution mode, so recordings look soft. A Retina mode (System Settings > Displays) records at twice the pixels.")?;
        }
    }
    writeln!(out, "\nWindows on screen, front to back (--window <id>):")?;
    writeln!(out, "  {:>7}  {:<18} {:<44} {:>11}  position", "id", "app", "title", "size (pt)")?;
    let clip = |s: &str, n: usize| -> String {
        if s.chars().count() <= n {
            s.to_string()
        } else {
            format!("{}...", s.chars().take(n - 3).collect::<String>())
        }
    };
    for w in &list.windows {
        writeln!(
            out,
            "  {:>7}  {:<18} {:<44} {:>11}  ({}, {})",
            w.id,
            clip(&w.app, 18),
            clip(&w.title, 44),
            format!("{} x {}", w.width, w.height),
            w.x,
            w.y
        )?;
    }
    if list.untitled_hidden > 0 {
        writeln!(
            out,
            "  ({} untitled window(s) not shown: the capture library can only record windows that have a title)",
            list.untitled_hidden
        )?;
    }
    Ok(out)
}

pub fn record(opts: RecordOptions) -> Result<RecordingPaths> {
    if !scap::is_supported() {
        bail!("screen capture needs macOS 12.3 or newer");
    }
    if !scap::has_permission() && !scap::request_permission() {
        bail!("Screen Recording permission denied. Enable it in System Settings > Privacy & Security > Screen Recording, then re-run.");
    }
    if !opts.allow_no_input {
        input::check_permissions()?;
    }
    let Resolved { label, target, crop, area, window, native_px } = resolve(opts.source)?;
    eprintln!("source: {label}");
    std::fs::create_dir_all(&opts.out_dir)?;
    let video_path = opts.out_dir.join("recording.mp4");
    let events_path = opts.out_dir.join("events.json");
    // A stale events.json from an earlier run must not pass for this recording's.
    match std::fs::remove_file(&events_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }

    let (pts_w, pts_h) = (area.w, area.h);

    // Old sprites from an earlier run must not pass for this recording's.
    match std::fs::remove_dir_all(opts.out_dir.join("cursors")) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let capture_stop = Arc::new(AtomicBool::new(false));
    // Whatever way we leave this function, tell the capture thread to wind down.
    let _guard = StopOnDrop(capture_stop.clone());
    let stats = Arc::new(CaptureStats::default());
    let capture_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // System audio goes to its own file, written by its own thread, and is added to the video at the end.
    let audio_path = opts.out_dir.join("audio.f32");
    let (audio_rec, audio_tx) = match opts.system_audio {
        true => {
            let (rec, tx) = AudioRecorder::start(&audio_path, audio::SAMPLE_RATE)?;
            (Some(rec), Some(tx))
        }
        false => (None, None),
    };
    // The microphone: check the permission, open the device (it records nothing until the clock starts)
    // and give it its own file. Anything wrong here stops the recording before it starts.
    let mic_path = opts.out_dir.join("mic.f32");
    let (mic, mic_rec, mic_tx) = if opts.microphone {
        let status = mic_macos::request_access();
        if let Some(problem) = mic_macos::access_problem(status, &input::responsible_app()) {
            bail!("{problem}");
        }
        let mic = Mic::open()?;
        eprintln!("microphone: {} ({} Hz, {} channel(s))", mic.device, mic.rate, mic.channels);
        let (rec, tx) = AudioRecorder::start(&mic_path, mic.rate)?;
        (Some(mic), Some(rec), Some(tx))
    } else {
        (None, None, None)
    };
    let (frame_tx, frame_rx) = pump::channel(FRAME_QUEUE);
    let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(u32, u32), String>>();
    let (go_tx, go_rx) = mpsc::channel::<Clock>();
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let thread = {
        let (stop, stats, error) = (capture_stop.clone(), stats.clone(), capture_error.clone());
        let (fps, hide_own) = (opts.fps, opts.hide_own_windows);
        let audio_tx = audio_tx.clone();
        std::thread::Builder::new().name("capturra-capture".into()).spawn(move || {
            capture_thread(fps, target, crop, hide_own, audio_tx, go_rx, frame_tx, ready_tx, stop, stats, error);
            let _ = done_tx.send(());
        })?
    };
    // The capture thread builds the capturer but does not start it yet; it tells us the frame size.
    let (frame_w, frame_h) = match ready_rx.recv() {
        Ok(Ok(size)) => size,
        Ok(Err(msg)) => bail!("{msg}"),
        Err(_) => bail!("capture thread died before starting"),
    };

    // Start ffmpeg and open its encoder BEFORE the recording clock exists. Opening the encoder
    // stalls the pipe for ~350 ms; paid here, it cannot make the capture queue overflow.
    let encoder = Encoder::new(&video_path, frame_w, frame_h, opts.fps)?;
    let (warm, slowest) = encoder.warmup_report();
    eprintln!(
        "encoder: {frame_w}x{frame_h} ready after {:.0} ms (slowest warm-up write {:.0} ms); starting the recording clock",
        warm.as_secs_f64() * 1000.0,
        slowest.as_secs_f64() * 1000.0
    );

    // Countdown with the encoder already warm and the capturer built but not started, so "go" is
    // exactly when the clock starts. Ctrl+C during it cancels cleanly.
    let notify = |e: crate::RecordEvent| {
        if let Some(sink) = &opts.on_event {
            sink(e);
        }
    };
    if let Err(e) = countdown(opts.countdown_secs, &opts.stop, &|n| notify(crate::RecordEvent::Countdown(n))) {
        encoder.abort();
        return Err(e);
    }
    let clock = Clock::start();
    if let (Some(mic), Some(tx)) = (&mic, mic_tx) {
        mic.arm(clock, tx);
    }
    notify(crate::RecordEvent::Started);
    let input = if opts.allow_no_input { None } else { Some(InputLogger::start(clock)) };
    let cursor_stop = Arc::new(AtomicBool::new(false));
    let _cursor_guard = StopOnDrop(cursor_stop.clone());
    // The cursor source is the one line to change if NSCursor.currentSystemCursor goes away.
    let cursor_thread = cursor::spawn(|| NsCursorSource, &opts.out_dir, clock, cursor_stop.clone())?;
    // Where typing happens: the focused text field's frame (needs Accessibility, like the input events).
    // Whether the pointer is over something clickable (for the hand pointer in the finished video).
    let hover_thread = if opts.allow_no_input { None } else { Some(crate::hover::spawn(AxHoverSource::new, clock, cursor_stop.clone())?) };
    let focus_thread = if opts.allow_no_input { None } else { Some(crate::focus::spawn(AxFocusSource::new, clock, cursor_stop.clone())?) };
    // A window can be dragged while recording; follow it so the cursor stays on the right pixel.
    let tracker = match window {
        Some(id) => Some(spawn_window_tracker(id, (area.x, area.y, area.w, area.h), clock, cursor_stop.clone())?),
        None => None,
    };
    go_tx.send(clock).map_err(|_| anyhow!("capture thread exited before capture could start"))?;

    let info = pump::run(&frame_rx, clock, &opts.stop, opts.duration, encoder, |f| {
        let native = match native_px {
            Some((nw, nh)) => format!("; its native pixels = {nw} x {nh}"),
            None => String::new(),
        };
        eprintln!(
            "captured frame = {} x {} px for an area of {pts_w} x {pts_h} points{native}; frame/points = {:.3}",
            f.width,
            f.height,
            f.width as f64 / pts_w
        );
    });

    let stopped_at_ms = clock.now_ms();
    let mic_summary = mic.map(|m| (m.rate, m.stop()));

    // Ask the capture thread to stop and give it a moment. It checks the flag after every
    // frame; if the stream has gone completely quiet it is still blocked inside scap's
    // get_next_frame(), which has no timeout, so we cannot join it without hanging.
    capture_stop.store(true, Ordering::SeqCst);
    match done_rx.recv_timeout(THREAD_EXIT_GRACE) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            if thread.join().is_err() {
                bail!("capture thread panicked");
            }
        }
        Err(RecvTimeoutError::Timeout) => eprintln!(
            "note: the capture thread did not exit within {THREAD_EXIT_GRACE:?} (no frame arrived to wake it); \
             it is left running and ends with the process"
        ),
    }

    eprintln!(
        "frames: {} received, {} empty, {} skipped (size mismatch), {} dropped (encoder behind)",
        stats.seen.load(Ordering::Relaxed),
        stats.empty.load(Ordering::Relaxed),
        stats.skipped.load(Ordering::Relaxed),
        stats.dropped.load(Ordering::Relaxed),
    );
    let (stamped, fallbacks) = (stats.stamped.load(Ordering::Relaxed), stats.stamp_fallbacks.load(Ordering::Relaxed));
    eprintln!(
        "timestamps: {stamped} frames placed by their ScreenCaptureKit capture time, {fallbacks} by arrival time; \
         delivery lag mean {} ms, max {} ms",
        stats.delivery_lag_sum_ms.load(Ordering::Relaxed).checked_div(stamped).unwrap_or(0),
        stats.delivery_lag_max_ms.load(Ordering::Relaxed),
    );
    if let Some(msg) = capture_error.lock().unwrap().take() {
        bail!("{msg}");
    }
    let info = info?.ok_or_else(|| anyhow!("no frames were captured"))?;
    eprintln!("video: {} frames written at {} fps, video time zero = clock {} ms", info.stats.frames_written, opts.fps, info.origin_ms);
    if audio_rec.is_some() || mic_rec.is_some() {
        let mic = mic_rec.zip(mic_summary);
        add_audio(audio_rec, mic, &video_path, stopped_at_ms, info.origin_ms);
    }
    eprintln!("{}", pump::profile_summary(&info, &frame_rx.report(), stats.dropped.load(Ordering::Relaxed), opts.fps));

    let mut log = EventLog::new(DisplayInfo { width: pts_w, height: pts_h, scale: info.width as f64 / pts_w });
    log.video_offset_ms = info.origin_ms;
    let mut raw_events = input.map(|i| i.snapshot()).unwrap_or_default();
    let burst = geometry::collapse_start_burst(&mut raw_events);
    if burst > 0 {
        eprintln!("note: collapsed {burst} queued pointer move(s) delivered at the very start into one");
    }
    if opts.drop_stop_click.load(Ordering::SeqCst) {
        let n = geometry::drop_stop_click(&mut raw_events, stopped_at_ms, geometry::STOP_CLICK_WINDOW_MS);
        if n > 0 {
            eprintln!("note: dropped the click that stopped the recording ({n} event(s))");
        }
    }
    // Judge "zero events" on real input only; cursor changes are not input.
    if raw_events.is_empty() && !opts.allow_no_input {
        eprintln!(
            "\n!!! WARNING: ZERO INPUT EVENTS WERE RECORDED !!!\n\
             The video was saved to {} but events.json was NOT written, because a recording \
             without events cannot drive the cursor or zoom.\n\
             Likely causes: Input Monitoring / Accessibility not granted to the launching app, \
             or the mouse and keyboard were not used during the recording.\n",
            video_path.display()
        );
        bail!("recording finished with zero input events; events.json not written");
    }

    cursor_stop.store(true, Ordering::SeqCst);
    if let Some(handle) = focus_thread {
        let out = handle.join().map_err(|_| anyhow!("focus thread panicked"))?;
        if out.unavailable {
            eprintln!(
                "note: the focused text field could not be read (no Accessibility permission), so typing zooms to the pointer instead"
            );
        } else {
            eprintln!("focus: {} read(s), {} change event(s)", out.stats.reads, out.stats.changes);
        }
        raw_events.extend(out.events);
    }
    let mut hover_seen = false;
    if let Some(handle) = hover_thread {
        let out = handle.join().map_err(|_| anyhow!("hover thread panicked"))?;
        if out.unavailable {
            eprintln!(
                "note: the element under the pointer could not be read (no Accessibility permission), so the pointer stays as recorded"
            );
        } else {
            eprintln!(
                "hover: {} read(s), {} over something clickable, {} change event(s)",
                out.stats.reads, out.stats.clickable_reads, out.stats.changes
            );
        }
        hover_seen = out.stats.clickable_reads > 0;
        raw_events.extend(out.events);
    }
    // Input events are in global points on the recording clock. Express them relative to the
    // captured area, following the window if it moved.
    let origin = match tracker {
        Some(t) => {
            let out = t.join().map_err(|_| anyhow!("window tracker thread panicked"))?;
            if out.missing > 0 {
                eprintln!(
                    "WARNING: window {} could not be found on {} position check(s); it was closed or hidden during the recording",
                    window.unwrap_or(0),
                    out.missing
                );
            }
            if let Some((t_ms, w, h)) = out.resized {
                eprintln!(
                    "WARNING: the window was resized to {w} x {h} points at {:.1} s (it started at {pts_w} x {pts_h}). \
                     The video keeps its starting size, so the picture and the cursor may no longer line up after that point.",
                    t_ms as f64 / 1000.0
                );
            }
            OriginTrack::from_samples(out.samples).unwrap_or_else(|| OriginTrack::fixed(area.x, area.y))
        }
        None => OriginTrack::fixed(area.x, area.y),
    };
    if origin.moved() {
        eprintln!("note: the window moved during the recording; event positions follow it");
    }
    let mapped = geometry::map_events(&raw_events, &origin, (pts_w, pts_h));
    if mapped.dropped_outside > 0 || mapped.moves_outside > 0 {
        eprintln!(
            "outside the captured area: {} click/release event(s) dropped, {} pointer move(s) kept (drawn at the edge)",
            mapped.dropped_outside, mapped.moves_outside
        );
    }
    log.events = mapped.events;
    let CursorOutput { table, events, stats } =
        cursor_thread.join().map_err(|_| anyhow!("cursor thread panicked"))?.map_err(|e| anyhow!("cursor capture failed: {e:#}"))?;
    eprintln!("cursor: {} samples, {} nil, {} distinct image(s), {} change event(s)", stats.samples, stats.nil, table.len(), events.len());
    if table.is_empty() {
        eprintln!(
            "\nWARNING: no cursor image could be read ({} nil samples), so events.json has an empty \
             cursors table and the preview will draw a plain circle instead of the real cursor.\n",
            stats.nil
        );
    }
    log.cursors = table;
    // The hand to show over clickable things: the system's own artwork, saved with the recording.
    if hover_seen {
        match &opts.hand_cursor {
            Some(hand) => {
                let id = log.cursors.iter().map(|c| c.id + 1).max().unwrap_or(0);
                std::fs::write(opts.out_dir.join("cursors").join(format!("{id}.png")), &hand.png)?;
                let scale = if hand.scale > 0.0 { hand.scale } else { 1.0 };
                log.cursors.push(capturra_core::CursorImage {
                    id,
                    file: format!("cursors/{id}.png"),
                    hotspot: capturra_core::Hotspot { x: hand.hotspot.0, y: hand.hotspot.1 },
                    scale,
                    size: capturra_core::SizePt { w: hand.width_px as f64 / scale, h: hand.height_px as f64 / scale },
                    role: Some(capturra_core::CursorImage::POINTING_HAND.to_string()),
                });
            }
            None => eprintln!("note: the hand pointer's artwork was not available (it can only be read on the app's main thread), so the video will not show the hand over clickable things"),
        }
    }
    log.events.extend(events);
    // The input log and the cursor changes come from different threads; merge into time order.
    log.sort_events();
    // The input logger keeps running a little after the last video frame; events past the end of
    // the video can never be shown, so they do not go into the file.
    let video_log = log.to_video_time();
    let total = video_log.events.len();
    let video_log = video_log.clipped_to(info.span_ms);
    if video_log.events.len() < total {
        eprintln!(
            "events: {} event(s) after the last video frame ({:.2} s) were not written",
            total - video_log.events.len(),
            info.span_ms as f64 / 1000.0
        );
    }
    std::fs::write(&events_path, video_log.to_json()?)?;

    Ok(RecordingPaths { video: video_path, events: events_path })
}

/// Frames waiting for the encoder. Bounded so a slow encoder drops (and counts) frames
/// instead of growing memory: ~10MB each at 2048x1280.
const FRAME_QUEUE: usize = 8;
/// How long to wait for the capture thread to exit after a stop request.
const THREAD_EXIT_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct CaptureStats {
    seen: AtomicU64,
    empty: AtomicU64,
    skipped: AtomicU64,
    dropped: AtomicU64,
    /// Frames placed on the clock by their ScreenCaptureKit timestamp, and frames that fell back
    /// to arrival time because the timestamp made no sense.
    stamped: AtomicU64,
    stamp_fallbacks: AtomicU64,
    /// Largest and summed gap between capture and arrival (ms): how late frames were delivered.
    delivery_lag_max_ms: AtomicU64,
    delivery_lag_sum_ms: AtomicU64,
}

struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Capturra's own on-screen windows as capture exclusions; `None` if they cannot be listed (the
/// recording then proceeds without exclusions, as it always did, and says so).
fn own_windows_to_exclude() -> Option<Vec<Target>> {
    match sources::own_window_ids() {
        Ok(ids) if !ids.is_empty() => {
            Some(ids.into_iter().map(|id| Target::Window(scap::Window { id, title: String::new(), raw_handle: id })).collect())
        }
        Ok(_) => None,
        Err(e) => {
            eprintln!("note: could not list Capturra's own windows, so they may appear in the recording: {e:#}");
            None
        }
    }
}

/// Owns the scap `Capturer` (built here so it never has to be `Send`). It reports the frame size,
/// waits for the go-ahead carrying the shared clock, then forwards each frame, stamped from that
/// clock the moment it arrives, to the encoder side. With `hide_own_windows`, Capturra's own windows
/// (Studio, recording bar, popover, menu-bar timer) are left out of a display or region capture.
fn capture_thread(
    fps: u32,
    target: Target,
    crop: Option<Area>,
    hide_own_windows: bool,
    audio: Option<AudioSender>,
    go: mpsc::Receiver<Clock>,
    frames: FrameSender,
    ready: mpsc::Sender<std::result::Result<(u32, u32), String>>,
    stop: Arc<AtomicBool>,
    stats: Arc<CaptureStats>,
    error: Arc<Mutex<Option<String>>>,
) {
    let options = Options {
        fps,
        // The cursor is NOT baked into the video. We redraw it from event data.
        show_cursor: false,
        show_highlight: false,
        output_type: FrameType::BGRAFrame,
        output_resolution: Resolution::Captured,
        target: Some(target),
        crop_area: crop,
        // Capturra's own windows (the Studio with its recording bar, the popover, the menu-bar timer) stay
        // out of display and region recordings. Only a display capture can exclude windows; a window
        // recording shows just that window anyway.
        excluded_targets: if hide_own_windows { own_windows_to_exclude() } else { None },
        // ...and the whole app, so windows created mid-recording stay out too.
        exclude_current_app: hide_own_windows,
        captures_audio: audio.is_some(),
    };
    let mut capturer = match Capturer::build(options) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(format!("could not start capture: {e:?}")));
            return;
        }
    };
    let [width, height] = capturer.get_output_frame_size();
    let _ = ready.send(Ok((width, height)));
    // Hold off until the encoder is warm and the clock exists; if the main thread gave up,
    // the sender is dropped and we leave without ever starting the stream.
    let Ok(clock) = go.recv() else { return };
    capturer.start_capture();
    let mut last_t_ms = 0u64;
    let mut warned_rate = false;

    while !stop.load(Ordering::SeqCst) {
        let frame = match capturer.get_next_frame() {
            Ok(f) => f,
            Err(e) => {
                *error.lock().unwrap() = Some(format!("capture stream ended: {e:?}"));
                break;
            }
        };
        // Read the arrival time before any copying so it reflects arrival, not our processing.
        let arrival_ms = clock.now_ms();
        let f = match frame {
            Frame::BGRA(f) => f,
            Frame::Audio(a) => {
                if let Some(tx) = &audio {
                    if a.sample_rate != audio::SAMPLE_RATE && !warned_rate {
                        warned_rate = true;
                        eprintln!("warning: system audio arrives at {} Hz, expected {}", a.sample_rate, audio::SAMPLE_RATE);
                    }
                    // Place by the chunk's own timestamp (sample-accurate); without a usable one, the chunk
                    // ended when it arrived.
                    let (_, stamped) = clock::pts_to_clock_ms(a.display_time, clock.host_origin_ns(), arrival_ms);
                    let start_ns = if stamped {
                        a.display_time as i64 - clock.host_origin_ns() as i64
                    } else {
                        let len_ns = a.channels[0].len() as i64 * 1_000_000_000 / a.sample_rate.max(1) as i64;
                        arrival_ms as i64 * 1_000_000 - len_ns
                    };
                    tx.send(start_ns, a.channels);
                }
                continue;
            }
            _ => continue,
        };
        let seen = stats.seen.fetch_add(1, Ordering::Relaxed) + 1;
        if seen <= 5 {
            eprintln!("frame {seen}: width={} height={} data.len()={}", f.width, f.height, f.data.len());
        }
        // scap reports idle ticks as zero-sized frames; they carry no picture.
        if f.data.is_empty() {
            stats.empty.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        // Place the frame where ScreenCaptureKit captured it, not where it reached us: delivery can
        // lag behind the capture by tens of ms and by a lot when the machine is busy.
        let (pts_ms, from_pts) = clock::pts_to_clock_ms(f.display_time, clock.host_origin_ns(), arrival_ms);
        // Never step back: the encoder wants non-decreasing times.
        let t_ms = pts_ms.max(last_t_ms);
        last_t_ms = t_ms;
        if from_pts {
            let lag = arrival_ms.saturating_sub(t_ms);
            stats.stamped.fetch_add(1, Ordering::Relaxed);
            stats.delivery_lag_sum_ms.fetch_add(lag, Ordering::Relaxed);
            stats.delivery_lag_max_ms.fetch_max(lag, Ordering::Relaxed);
        } else {
            stats.stamp_fallbacks.fetch_add(1, Ordering::Relaxed);
        }
        let (w, h) = (f.width as usize, f.height as usize);
        let tight = w * 4;
        let data = if f.data.len() == tight * h {
            f.data
        } else if h > 0 && f.data.len() > tight * h && f.data.len() % h == 0 {
            // Rows are padded: copy them out without the padding.
            let stride = f.data.len() / h;
            let mut buf = Vec::with_capacity(tight * h);
            for row in f.data.chunks_exact(stride) {
                buf.extend_from_slice(&row[..tight]);
            }
            buf
        } else {
            stats.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        match frames.try_send(TimedFrame { t_ms, width: w as u32, height: h as u32, data }) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
    }
    capturer.stop_capture();
}

/// Finish the audio tracks (system audio and/or the microphone) and add them to the video as one sound
/// track. A failure here never costs the recording: the video stays as it is, the raw audio is kept next to
/// it, and the problem is said loudly.
fn add_audio(
    system: Option<AudioRecorder>,
    mic: Option<(AudioRecorder, (u32, MicSummary))>,
    video: &std::path::Path,
    end_ms: u64,
    video_origin_ms: u64,
) {
    // (what it is, file, rate, tempo)
    let mut tracks: Vec<(&str, std::path::PathBuf, u32, f64)> = Vec::new();
    if let Some(rec) = system {
        match rec.finish(end_ms) {
            Ok((raw, stats)) if stats.chunks > 0 => {
                eprintln!(
                    "system audio: {} chunks, {} gaps filled with silence ({} ms, longest {} ms), {} ms cut where chunks overlapped",
                    stats.chunks, stats.gaps, stats.silence_ms, stats.largest_gap_ms, stats.overlap_cut_ms
                );
                tracks.push(("system audio", raw, audio::SAMPLE_RATE, 1.0));
            }
            Ok((raw, _)) => {
                eprintln!(
                    "WARNING: no system audio was received, so the video has none. Check that macOS is 13 or newer \
                     and that something was playing."
                );
                let _ = std::fs::remove_file(&raw);
            }
            Err(e) => eprintln!("WARNING: the system audio could not be written, so the video has none: {e:#}"),
        }
    }
    if let Some((rec, (rate, summary))) = mic {
        match rec.finish(end_ms) {
            Ok((raw, _)) if summary.frames > 0 => {
                let secs = summary.frames as f64 / rate as f64;
                eprintln!(
                    "microphone: {secs:.1} s of audio at {rate} Hz{}",
                    if (summary.tempo - 1.0).abs() > 1e-9 {
                        format!("; its clock ran {:+.0} ppm off, corrected", (summary.tempo - 1.0) * 1e6)
                    } else {
                        String::new()
                    }
                );
                tracks.push(("microphone", raw, rate, summary.tempo));
            }
            Ok((raw, _)) => {
                eprintln!(
                    "WARNING: the microphone delivered no sound, so the video has none from it. If you just allowed \
                     access, quit and reopen the app and record again."
                );
                let _ = std::fs::remove_file(&raw);
            }
            Err(e) => eprintln!("WARNING: the microphone audio could not be written, so the video has none from it: {e:#}"),
        }
    }
    if tracks.is_empty() {
        return;
    }
    let inputs: Vec<AudioInput> = tracks.iter().map(|(_, path, rate, tempo)| AudioInput { path, rate: *rate, tempo: *tempo }).collect();
    match audio::mux(video, &inputs, video_origin_ms) {
        Ok(()) => {
            for (_, path, _, _) in &tracks {
                let _ = std::fs::remove_file(path);
            }
            eprintln!("audio: added to {}", video.display());
        }
        Err(e) => {
            eprintln!(
                "WARNING: the audio could not be added to the video ({e:#}). The video is saved without sound; the raw audio is kept:"
            );
            for (what, path, rate, _) in &tracks {
                eprintln!("  {what}: {} (stereo f32le, {rate} Hz)", path.display());
            }
        }
    }
}

/// Print "3... 2... 1..." (one second apart) before the recording clock starts, calling `tick`
/// with the seconds left. Returns an error if `stop` is set (Ctrl+C, a UI's Cancel) before it
/// finishes.
fn countdown(secs: u32, stop: &AtomicBool, tick: &dyn Fn(u32)) -> Result<()> {
    if secs == 0 {
        return Ok(());
    }
    eprintln!("Recording starts in {secs} s: get the screen ready (Ctrl+C cancels).");
    for left in (1..=secs).rev() {
        eprintln!("  {left}...");
        tick(left);
        let until = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < until {
            if stop.load(Ordering::SeqCst) {
                bail!("cancelled during the countdown; nothing was recorded");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    eprintln!("  recording.");
    Ok(())
}

/// `record --region ... --guide`: show where the region sits. Saves two PNGs in `out_dir` and opens
/// them: `region-guide.png` is exactly the captured area (what the video will contain, at
/// 1 px/pt scaled by the display's native factor), and `region-guide-position.png` is the whole
/// display with the area outlined in red. Uses the system `screencapture`, so it sees what the
/// recording will see only if Screen Recording permission is granted.
pub fn guide(source: Source, out_dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>> {
    use std::process::Command;
    if !matches!(source, Source::Region { .. }) {
        bail!("--guide is for --region");
    }
    if !scap::has_permission() {
        bail!("Screen Recording permission is not granted to this terminal app, so the guide would show only the desktop picture (System Settings > Privacy & Security > Screen Recording).");
    }
    let Resolved { label, area, .. } = resolve(source)?;
    std::fs::create_dir_all(out_dir)?;
    let region_png = out_dir.join("region-guide.png");
    let position_png = out_dir.join("region-guide-position.png");
    let rect_arg = |r: PtRect| format!("-R{},{},{},{}", r.x, r.y, r.w, r.h);
    let shot = |r: PtRect, to: &std::path::Path| -> Result<()> {
        let st = Command::new("screencapture").arg("-x").arg(rect_arg(r)).arg(to).status()?;
        if !st.success() || !to.exists() {
            bail!("screencapture failed ({st})");
        }
        Ok(())
    };
    shot(area, &region_png)?;
    // The display holding the region, for the "where is it" picture.
    let display = sources::displays()?
        .into_iter()
        .find(|d| {
            let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
            d.bounds.contains(cx, cy)
        })
        .ok_or_else(|| anyhow!("could not find the display holding the region"))?;
    let full_png = out_dir.join("region-guide-full.tmp.png");
    shot(display.bounds, &full_png)?;
    // The screenshot is in pixels, which can be a multiple of points on a Retina display.
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width", "-of", "csv=p=0"])
        .arg(&full_png)
        .output()
        .map_err(|e| anyhow!("ffprobe is needed for the guide: {e}"))?;
    let img_w: f64 = String::from_utf8_lossy(&probe.stdout).trim().parse().map_err(|_| anyhow!("could not read the screenshot size"))?;
    let k = img_w / display.bounds.w;
    let vf = format!(
        "drawbox=x={}:y={}:w={}:h={}:color=red:t={}",
        ((area.x - display.bounds.x) * k).round(),
        ((area.y - display.bounds.y) * k).round(),
        (area.w * k).round(),
        (area.h * k).round(),
        (3.0 * k).round().max(3.0)
    );
    let st = Command::new("ffmpeg").args(["-v", "error", "-y", "-i"]).arg(&full_png).args(["-vf", &vf]).arg(&position_png).status()?;
    let _ = std::fs::remove_file(&full_png);
    if !st.success() {
        bail!("ffmpeg could not draw the outline ({st})");
    }
    eprintln!("guide for {label}");
    let _ = Command::new("open").arg(&region_png).arg(&position_png).status();
    Ok(vec![region_png, position_png])
}

/// What the window tracker saw.
struct WindowTrackOutput {
    /// (clock ms, global x, global y) of the window's top-left corner.
    samples: Vec<(u64, f64, f64)>,
    /// First time the size differed from the starting size: (clock ms, width, height).
    resized: Option<(u64, f64, f64)>,
    /// Polls where the window was not found.
    missing: u64,
}

const WINDOW_POLL: Duration = Duration::from_millis(25);

/// Polls the window's position (and size) on the shared clock until `stop`. Every poll is kept:
/// positions are interpolated linearly between samples, so dropping "unchanged" polls would smear
/// a window that sits still and then moves.
fn spawn_window_tracker(
    id: u32,
    start: (f64, f64, f64, f64),
    clock: Clock,
    stop: Arc<AtomicBool>,
) -> Result<std::thread::JoinHandle<WindowTrackOutput>> {
    Ok(std::thread::Builder::new().name("capturra-window".into()).spawn(move || {
        let mut out = WindowTrackOutput { samples: vec![(0, start.0, start.1)], resized: None, missing: 0 };
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(WINDOW_POLL);
            let t = clock.now_ms();
            match sources::window_geom(id) {
                Some(g) => {
                    out.samples.push((t, g.bounds.x, g.bounds.y));
                    if out.resized.is_none() && ((g.bounds.w - start.2).abs() > 1.0 || (g.bounds.h - start.3).abs() > 1.0) {
                        out.resized = Some((t, g.bounds.w, g.bounds.h));
                    }
                }
                None => out.missing += 1,
            }
        }
        out
    })?)
}

/// `record --check`: resolve the source and build the capturer WITHOUT starting it, so a source
/// can be validated (and its frame size seen) without recording anything or asking for permissions.
pub fn check(source: Source, fps: u32) -> Result<String> {
    if !scap::is_supported() {
        bail!("screen capture needs macOS 12.3 or newer");
    }
    if !scap::has_permission() {
        bail!("Screen Recording permission is not granted to this terminal app (System Settings > Privacy & Security > Screen Recording).");
    }
    let Resolved { label, target, crop, area, .. } = resolve(source)?;
    let options = Options {
        fps,
        show_cursor: false,
        show_highlight: false,
        output_type: FrameType::BGRAFrame,
        output_resolution: Resolution::Captured,
        target: Some(target),
        crop_area: crop,
        ..Default::default()
    };
    let mut capturer = Capturer::build(options).map_err(|e| anyhow!("could not build the capturer: {e:?}"))?;
    let [w, h] = capturer.get_output_frame_size();
    if w == 0 || h == 0 {
        bail!("source: {label}\nthe capture library reports a {w} x {h} frame size for this source, so it cannot be recorded");
    }
    Ok(format!(
        "source: {label}\nframes would be {w} x {h} px for an area of {} x {} points ({:.3} px/pt); nothing was recorded",
        area.w,
        area.h,
        w as f64 / area.w
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_of_zero_returns_at_once() {
        let t = std::time::Instant::now();
        countdown(0, &AtomicBool::new(false), &|_| {}).unwrap();
        assert!(t.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn countdown_takes_one_second_per_step() {
        let t = std::time::Instant::now();
        countdown(1, &AtomicBool::new(false), &|_| {}).unwrap();
        let e = t.elapsed();
        assert!(e >= Duration::from_millis(950) && e < Duration::from_millis(1500), "{e:?}");
    }

    #[test]
    fn countdown_reports_each_second_left_in_order() {
        let seen = Mutex::new(Vec::new());
        countdown(2, &AtomicBool::new(false), &|n| seen.lock().unwrap().push(n)).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![2, 1]);
    }

    #[test]
    fn source_list_has_the_main_display_first_and_the_text_listing_matches() {
        let list = source_list().unwrap();
        assert_eq!(list.displays[0].index, 0);
        assert!(list.displays[0].main);
        assert!(list.windows.iter().all(|w| w.width >= 50.0 && w.height >= 50.0));
        let text = list_sources().unwrap();
        assert!(text.starts_with("Displays"));
        for w in &list.windows {
            assert!(text.contains(&w.id.to_string()), "window {} missing from the text listing", w.id);
        }
    }

    #[test]
    fn permissions_report_never_panics_and_names_an_app() {
        let p = permissions();
        assert!(!p.responsible_app.is_empty());
        assert_eq!(p.all_granted(), p.screen_recording && p.accessibility && p.input_monitoring);
    }

    #[test]
    fn ctrl_c_cancels_the_countdown_quickly_with_a_clear_error() {
        let t = std::time::Instant::now();
        let err = countdown(3, &AtomicBool::new(true), &|_| {}).unwrap_err().to_string();
        assert!(err.contains("cancelled during the countdown"), "{err}");
        assert!(t.elapsed() < Duration::from_millis(200));
    }
}

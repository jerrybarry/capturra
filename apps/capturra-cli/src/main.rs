mod synth;

use anyhow::{Context, Result};
use capturra_core::{EventLog, ZoomConfig, ZoomTimeline};
use capturra_recorder::geometry::{self, Source};
use capturra_recorder::RecordOptions;
use capturra_render::PreviewOptions;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "capturra", about = "Screen recorder spike: capture, cursor log, auto-zoom preview")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record the main display + mouse/keyboard events (macOS only).
    Record {
        #[arg(long, default_value = "recording")]
        out: PathBuf,
        #[arg(long, default_value_t = 10)]
        seconds: u64,
        /// Frame rate of the video. 30 keeps the encoder pipe comfortably under load; 60 is
        /// available but drops frames on some Macs.
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Show the displays and windows that can be recorded, then exit.
        #[arg(long)]
        list: bool,
        /// Record display <n> from --list (default 0, the main display).
        #[arg(long, conflicts_with = "window")]
        display: Option<usize>,
        /// Record one window by its id from --list; other apps and windows stay out of the video.
        #[arg(long)]
        window: Option<u32>,
        /// Record a rectangle `x,y,w,h` of the display, in points from its top-left corner.
        #[arg(long, conflicts_with = "window", value_name = "X,Y,W,H")]
        region: Option<String>,
        /// Resolve the source and report the frame size, without recording.
        #[arg(long)]
        check: bool,
        /// With --region: save and open pictures of where the region sits, then exit.
        #[arg(long, requires = "region")]
        guide: bool,
        /// Seconds to count down before recording starts (0 = start at once).
        #[arg(long, default_value_t = 3)]
        countdown: u32,
        /// Verification only: record video with no input events (skips the input-permission check).
        #[arg(long, hide = true)]
        allow_no_input: bool,
        /// Keep Capturra's own windows in a display or region recording (they are left out by default).
        #[arg(long)]
        show_capturra: bool,
        /// Also record system audio (macOS 13+; everything the Mac plays except Capturra's own sound).
        #[arg(long)]
        audio: bool,
        /// Also record the default microphone (needs the Microphone permission), mixed into the same sound track.
        #[arg(long)]
        mic: bool,
    },
    /// Sample the system cursor image for a while and dump distinct PNGs (macOS; diagnostic).
    CursorProbe {
        #[arg(long, default_value = "cursor-probe")]
        out: PathBuf,
        #[arg(long, default_value_t = 20)]
        seconds: u64,
        /// Create an NSApplication first (may be needed for a WindowServer connection).
        #[arg(long)]
        init_app: bool,
    },
    /// Generate zoom segments from an events.json and print them.
    Zoom {
        events: PathBuf,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long, default_value_t = 2.0)]
        level: f64,
    },
    /// Measure the auto-zoom on real recordings: how much is zoomed, how often, and how long the cursor is out of view.
    /// Each argument is a recording folder (the one holding recording.mp4 and events.json).
    ZoomStats { folders: Vec<PathBuf> },
    /// Print the element under the pointer a few times (move the pointer over buttons and links meanwhile).
    HoverProbe {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Print what the Accessibility API says is focused, a few times (click into a text field meanwhile).
    FocusProbe {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
    /// Render a zoomed preview video with a redrawn cursor.
    Preview {
        video: PathBuf,
        events: PathBuf,
        #[arg(long, default_value = "preview.mp4")]
        out: PathBuf,
        #[arg(long, default_value_t = 2.0)]
        level: f64,
        /// Output width in pixels. 0 = same as the source video (sharpest).
        #[arg(long, default_value_t = 0)]
        width: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        /// Put the recording on a padded, rounded gradient canvas like the Studio's export does (for timing).
        #[arg(long)]
        framed: bool,
        /// Encode with the Mac's hardware encoder (faster, bigger file) instead of x264.
        #[arg(long)]
        hardware: bool,
    },
    /// Create a synthetic test video + scripted cursor log (works on any OS).
    Synth {
        #[arg(long, default_value = "synthetic")]
        out: PathBuf,
    },
}

fn load_log(path: &PathBuf) -> Result<EventLog> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(EventLog::from_json(&s)?)
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Record {
            out,
            seconds,
            fps,
            list,
            display,
            window,
            region,
            check,
            guide,
            countdown,
            allow_no_input,
            show_capturra,
            audio,
            mic,
        } => {
            if list {
                print!("{}", capturra_recorder::list_sources()?);
                return Ok(());
            }
            let source = match (window, region) {
                (Some(id), _) => Source::Window(id),
                (None, Some(r)) => {
                    let rect = geometry::parse_region(&r).map_err(|e| anyhow::anyhow!("--region: {e}"))?;
                    Source::Region { display: display.unwrap_or(0), rect }
                }
                (None, None) => Source::Display(display.unwrap_or(0)),
            };
            if guide {
                for p in capturra_recorder::region_guide(source, &out)? {
                    println!("{}", p.display());
                }
                return Ok(());
            }
            if check {
                println!("{}", capturra_recorder::check_source(source, fps)?);
                return Ok(());
            }
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            ctrlc::set_handler(move || flag.store(true, Ordering::SeqCst))?;
            println!("Recording for {seconds}s (Ctrl+C to stop early)...");
            let paths = capturra_recorder::record(RecordOptions {
                on_event: None,
                source,
                countdown_secs: countdown,
                allow_no_input,
                // Stopped with Ctrl+C or --seconds, not a click.
                drop_stop_click: Default::default(),
                hide_own_windows: !show_capturra,
                system_audio: audio,
                microphone: mic,
                // read here, on the main thread, where macOS hands out its standard cursors
                hand_cursor: capturra_recorder::pointing_hand_sprite(),
                out_dir: out,
                fps,
                duration: Some(Duration::from_secs(seconds)),
                stop,
            })?;
            println!("video:  {}\nevents: {}", paths.video.display(), paths.events.display());
        }
        Cmd::CursorProbe { out, seconds, init_app } => {
            println!("sampling the system cursor for {seconds}s: move over a text field, a link and a window edge...");
            capturra_recorder::cursor_probe(&out, seconds, init_app)?;
        }
        Cmd::Zoom { events, out, level } => {
            let log = load_log(&events)?;
            let cfg = ZoomConfig { zoom_level: level, ..Default::default() };
            let tl = ZoomTimeline::new(&log, cfg);
            for (i, s) in tl.segments.iter().enumerate() {
                println!("segment {i}: {} ms -> {} ms, {} waypoint(s)", s.start_ms, s.end_ms, s.waypoints.len());
            }
            if let Some(out) = out {
                std::fs::write(&out, serde_json::to_string_pretty(&tl)?)?;
                println!("wrote {}", out.display());
            }
        }
        Cmd::HoverProbe { seconds } => {
            match capturra_recorder::pointing_hand_sprite() {
                Some(h) => println!(
                    "hand artwork: read ({}x{} px, scale {:.1}, hotspot {:.0},{:.0})",
                    h.width_px, h.height_px, h.scale, h.hotspot.0, h.hotspot.1
                ),
                None => println!("hand artwork: NOT available"),
            }
            for i in 0..seconds * 2 {
                println!("{:>4.1}s  {}", i as f64 / 2.0, capturra_recorder::hover_probe());
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        Cmd::FocusProbe { seconds } => {
            for i in 0..seconds * 2 {
                println!("{:>4.1}s  {}", i as f64 / 2.0, capturra_recorder::focus_probe());
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        Cmd::ZoomStats { folders } => {
            println!(
                "{:<22} {:>6} {:>6} {:>8} {:>9} {:>9} {:>8} {:>8}",
                "recording", "secs", "zooms", "per min", "zoomed", "no cursor", "pumping", "longest"
            );
            let mut totals = (0.0, 0usize, 0.0, 0.0, 0usize);
            for dir in &folders {
                if !dir.join("events.json").exists() || !dir.join("recording.mp4").exists() {
                    println!(
                        "{:<22} skipped: no events.json or recording.mp4",
                        dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                    );
                    continue;
                }
                let log = load_log(&dir.join("events.json"))?;
                let video_ms = capturra_render::probe_duration_ms(&dir.join("recording.mp4"))?;
                let log = log.to_video_time().clipped_to(video_ms);
                let s = ZoomTimeline::new(&log, ZoomConfig::default()).stats(&log, video_ms);
                println!(
                    "{:<22} {:>6.1} {:>6} {:>8.1} {:>8.0}% {:>8.0}% {:>8} {:>7.1}s",
                    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    video_ms as f64 / 1000.0,
                    s.zoom_ins,
                    s.zoom_ins_per_min,
                    s.coverage * 100.0,
                    s.cursor_hidden * 100.0,
                    s.pumping,
                    s.longest_ms as f64 / 1000.0
                );
                totals = (
                    totals.0 + video_ms as f64,
                    totals.1 + s.zoom_ins,
                    totals.2 + s.coverage * video_ms as f64,
                    totals.3 + s.cursor_hidden * s.coverage * video_ms as f64,
                    totals.4 + s.pumping,
                );
            }
            if totals.0 > 0.0 {
                println!(
                    "{:<22} {:>6.1} {:>6} {:>8.1} {:>8.0}% {:>8.0}% {:>8}",
                    "ALL",
                    totals.0 / 1000.0,
                    totals.1,
                    totals.1 as f64 / (totals.0 / 60_000.0),
                    totals.2 / totals.0 * 100.0,
                    if totals.2 > 0.0 { totals.3 / totals.2 * 100.0 } else { 0.0 },
                    totals.4
                );
            }
        }
        Cmd::Preview { video, events, out, level, width, fps, framed, hardware } => {
            let log = load_log(&events)?;
            // Events recorded after the last video frame can never be seen; drop them.
            let video_ms = capturra_render::probe_duration_ms(&video)?;
            let before = log.events.len();
            let log = log.clipped_to(video_ms);
            if log.events.len() < before {
                println!("ignoring {} event(s) after the end of the video ({:.2} s)", before - log.events.len(), video_ms as f64 / 1000.0);
            }
            let tl = ZoomTimeline::new(&log, ZoomConfig { zoom_level: level, ..Default::default() });
            println!("{} zoom segment(s); rendering (use --release for speed)...", tl.segments.len());
            if log.cursors.is_empty() {
                println!("no cursor images in this log (version {}): drawing the fallback circle", log.version);
            } else {
                println!("{} cursor image(s)", log.cursors.len());
            }
            let assets_dir = events.parent().map(|p| p.to_path_buf());
            let frame = framed.then(|| capturra_render::FrameStyle {
                aspect: Some(16.0 / 9.0),
                backdrop: capturra_render::Backdrop::Gradient([108, 99, 255], [58, 47, 184]),
                padding_pct: 6.0,
                radius: 14.0,
                shadow: 0.5,
            });
            capturra_render::render_preview(
                &video,
                &log,
                &tl,
                &out,
                &PreviewOptions {
                    width,
                    fps,
                    draw_cursor: true,
                    assets_dir,
                    frame,
                    encoder: if hardware { capturra_render::VideoEncoder::VideoToolbox } else { capturra_render::VideoEncoder::X264 },
                    ..Default::default()
                },
            )?;
            println!("wrote {}", out.display());
        }
        Cmd::Synth { out } => {
            synth::generate(&out)?;
            println!("wrote {}/recording.mp4 and {}/events.json", out.display(), out.display());
        }
    }
    Ok(())
}

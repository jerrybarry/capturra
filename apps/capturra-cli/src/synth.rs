//! Synthetic test data so the zoom pipeline can be tried on any OS, with no
//! screen-recording permissions: a test-pattern video plus a scripted cursor.

use anyhow::{bail, Context, Result};
use capturra_core::{DisplayInfo, EventLog, InputEvent, MouseButton};
use std::path::Path;
use std::process::Command;

const W: f64 = 1280.0;
const H: f64 = 720.0;

pub fn generate(out_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    let video = out_dir.join("recording.mp4");
    let status = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30", "-t", "12"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&video)
        .status()
        .context("failed to run ffmpeg")?;
    if !status.success() {
        bail!("ffmpeg failed to create the synthetic video");
    }

    // (time_ms, x, y, click_on_arrival)
    let path: &[(u64, f64, f64, bool)] = &[
        (0, 200.0, 200.0, false),
        (2000, 300.0, 250.0, true),
        (5000, 900.0, 400.0, true),
        (6200, 920.0, 430.0, true),
        (10000, 200.0, 600.0, true),
        (12000, 220.0, 610.0, false),
    ];

    let mut log = EventLog::new(DisplayInfo { width: W, height: H, scale: 1.0 });
    for pair in path.windows(2) {
        let (t0, x0, y0, _) = pair[0];
        let (t1, x1, y1, click) = pair[1];
        let mut t = t0;
        while t < t1 {
            let p = (t - t0) as f64 / (t1 - t0) as f64;
            log.events.push(InputEvent::Move { t_ms: t, x: x0 + (x1 - x0) * p, y: y0 + (y1 - y0) * p });
            t += 16;
        }
        if click {
            log.events.push(InputEvent::Down { t_ms: t1, x: x1, y: y1, button: MouseButton::Left });
            log.events.push(InputEvent::Up { t_ms: t1 + 80, x: x1, y: y1, button: MouseButton::Left });
        }
    }
    // A typing burst around t=8s.
    for i in 0..6 {
        log.events.push(InputEvent::Key { t_ms: 8000 + i * 100 });
    }

    add_cursor_changes(&mut log, out_dir)?;
    std::fs::write(out_dir.join("events.json"), log.to_json()?)?;
    Ok(())
}

/// On macOS, give the scripted run real cursor sprites that change type over time, written
/// through the same sampler the recorder uses. Elsewhere the log has no cursors (circle).
fn add_cursor_changes(log: &mut EventLog, out_dir: &Path) -> Result<()> {
    use capturra_recorder::cursor::{CursorImageData, CursorSampler, CursorSource};

    let all = capturra_recorder::standard_cursors();
    if all.is_empty() {
        return Ok(());
    }
    let (names, images): (Vec<_>, Vec<_>) = all.into_iter().unzip();

    /// Hands out one prepared image per sample.
    struct Next(Option<CursorImageData>);
    impl CursorSource for Next {
        fn sample(&mut self) -> Option<CursorImageData> {
            self.0.take()
        }
    }

    // (time_ms, cursor name): arrow, text field, link, window edge, hand, back to arrow.
    let script = [(0, "arrow"), (3000, "text"), (5000, "link"), (7000, "resize"), (9000, "hand"), (10500, "arrow")];
    // One scripted sample per cursor, so the live-recording debounce would never confirm them.
    let mut sampler = CursorSampler::new(Next(None), out_dir)?.with_debounce_ms(0);
    for (t_ms, name) in script {
        let i = names.iter().position(|n| *n == name).expect("standard cursor present");
        let img = &images[i];
        sampler = sampler.with_source(Next(Some(CursorImageData {
            png: img.png.clone(),
            hotspot: img.hotspot,
            scale: img.scale,
            width_px: img.width_px,
            height_px: img.height_px,
        })));
        sampler.poll(t_ms)?;
    }
    log.cursors = sampler.table;
    log.events.extend(sampler.events);
    Ok(())
}

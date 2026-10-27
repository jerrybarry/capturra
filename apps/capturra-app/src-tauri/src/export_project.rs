//! `export_project`: render the edited recording to `<recording folder>/exports/<name>.mp4|gif`.
//!
//! This uses the CPU renderer (`capturra-render`), which the Phase 3 `wgpu` compositor will
//! replace. What it applies today: enabled zoom segments (manual focus, each segment's own level
//! and ramps), the trim, the output size and frame rate, and the redrawn cursor with the
//! project's cursor settings (show, size, smoothing, ripple, squish), and the frame (gradient
//! background, padding, rounded corners, shadow, canvas shape).

use crate::audio_edit::AudioSettings;
use crate::projects::{read_log, read_project_file, recordings_root, resolve_dir, ProjectFile, VIDEO};
use crate::studio_zoom::{studio_segments, timeline_from_studio, StudioSegment};
use capturra_core::ZoomConfig;
use capturra_render::{Backdrop, CursorStyle, FrameStyle, PreviewOptions};

use crate::backgrounds;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Deserialize)]
pub struct ExportOptions {
    #[serde(default)]
    pub format: String,
    /// Output height in pixels (the Studio sends it as a string such as "1080"). Anything that is
    /// not a number means "same size as the recording".
    #[serde(default)]
    pub res: Value,
    #[serde(default)]
    pub fps: u32,
    #[serde(default)]
    pub name: String,
    /// `"fast"` = the hardware encoder (quicker, bigger file); anything else = x264 (smaller file).
    #[serde(default)]
    pub encoder: String,
}

#[derive(Debug, Serialize)]
pub struct ExportResult {
    pub path: String,
}

pub type Progress = Arc<dyn Fn(f64) + Send + Sync>;

/// A file name that is safe on macOS and nice in Finder: no separators, no leading dots.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if matches!(c, '/' | ':' | '\\' | '\0') || c.is_control() { '-' } else { c }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "export".to_string()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// `<dir>/exports/<name>.<ext>`, or `<name> 2.<ext>`... if that exists. Never overwrites.
pub fn output_path(dir: &Path, name: &str, ext: &str) -> Result<PathBuf, String> {
    let folder = dir.join("exports");
    std::fs::create_dir_all(&folder).map_err(|e| format!("could not create {}: {e}", folder.display()))?;
    let name = sanitize_name(name);
    let first = folder.join(format!("{name}.{ext}"));
    if !first.exists() {
        return Ok(first);
    }
    Ok((2..).map(|n| folder.join(format!("{name} {n}.{ext}"))).find(|p| !p.exists()).unwrap())
}

/// The Studio's six gradient presets (`BACKGROUNDS` in `src/studio/stage.ts`).
const BACKGROUNDS: [([u8; 3], [u8; 3]); 6] = [
    ([0x6c, 0x63, 0xff], [0x3a, 0x2f, 0xb8]),
    ([0xff, 0x7a, 0x59], [0xc2, 0x30, 0x6b]),
    ([0x19, 0xb4, 0xa1], [0x0a, 0x5d, 0x8f]),
    ([0x2c, 0x3e, 0x66], [0x0e, 0x16, 0x30]),
    ([0xf6, 0xd3, 0x65], [0xfd, 0xa0, 0x85]),
    ([0xe9, 0xe9, 0xef], [0xcf, 0xd2, 0xe2]),
];

fn hex_color(text: &str) -> Option<[u8; 3]> {
    let t = text.trim().trim_start_matches('#');
    if t.len() != 6 || !t.is_ascii() {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// Recordings narrower than this (width / height) default to their own shape instead of 16:9.
pub const WIDE_ENOUGH: f64 = 1.25;

/// The project's frame settings as the renderer's frame. Missing values use the Studio's own
/// defaults, so a project that was never edited exports as the Studio previews it. `None` when
/// the frame would be invisible (no padding, corners or shadow, and the recording's own shape).
pub fn frame_style(settings: &Option<Value>, display: (f64, f64), custom_dir: &Path) -> Result<Option<FrameStyle>, String> {
    let f = settings.as_ref().and_then(|s| s.get("frame"));
    let num = |key: &str, default: f64| f.and_then(|f| f.get(key)).and_then(Value::as_f64).unwrap_or(default);
    let text = |key: &str| f.and_then(|f| f.get(key)).and_then(Value::as_str);
    let bg = num("bg", 0.0);
    let gradient = match BACKGROUNDS.get(bg as usize).filter(|_| bg >= 0.0) {
        Some(preset) => *preset,
        None => (text("c1").and_then(hex_color).unwrap_or(BACKGROUNDS[0].0), text("c2").and_then(hex_color).unwrap_or(BACKGROUNDS[0].1)),
    };
    // `type` picks which background settings apply; a project without one is a gradient.
    let backdrop = match text("type").unwrap_or("gradient") {
        "color" => {
            let c = text("color").and_then(hex_color).unwrap_or(BACKGROUNDS[3].1);
            Backdrop::Gradient(c, c)
        }
        "wallpaper" => Backdrop::Image(Arc::new(backgrounds::wallpaper(text("wallpaper").unwrap_or_default())?)),
        "custom" => Backdrop::Image(Arc::new(backgrounds::custom(custom_dir, text("image").unwrap_or_default())?)),
        _ => Backdrop::Gradient(gradient.0, gradient.1),
    };
    // A recording that is not wide (a window, a phone-shaped region) would be a small picture on a
    // 16:9 canvas, so those default to the recording's own shape. The Studio makes the same choice.
    let default_aspect = if display.0 / display.1 < WIDE_ENOUGH { "source" } else { "16:9" };
    let aspect = match text("aspect").unwrap_or(default_aspect) {
        "16:9" => Some(16.0 / 9.0),
        "4:3" => Some(4.0 / 3.0),
        "1:1" => Some(1.0),
        "9:16" => Some(9.0 / 16.0),
        _ => None,
    };
    let style = FrameStyle {
        aspect,
        backdrop,
        padding_pct: num("padding", 6.0).clamp(0.0, 40.0),
        radius: num("radius", 14.0).clamp(0.0, 200.0),
        shadow: num("shadow", 0.5).clamp(0.0, 1.0),
    };
    let invisible = style.aspect.is_none() && style.padding_pct == 0.0 && style.radius == 0.0 && style.shadow == 0.0;
    Ok((!invisible).then_some(style))
}

/// Output width for a requested height. `aspect` is the output's width / height; 0 = the
/// recording's own size.
pub fn target_width(res: &Value, aspect: f64) -> u32 {
    let height = match res {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_end_matches('p').parse::<f64>().ok(),
        _ => None,
    };
    match height {
        Some(h) if h >= 16.0 && aspect > 0.0 => (((h * aspect).round() as u32) & !1).max(16),
        _ => 0,
    }
}

/// Palette-based GIF conversion at a capped width and frame rate.
pub fn gif_filter(fps: u32, width: u32) -> String {
    format!(
        "fps={fps},scale={width}:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4"
    )
}

/// The project's cursor settings as the renderer's style, and whether to draw the cursor at all.
/// Missing values fall back to the Studio's own defaults, so a project that was never edited
/// exports the way the Studio previews it. Smoothing (0..1, a moving average in the Studio)
/// maps to an exponential half-life of `smoothing * 120 ms`: 0.5 gives the renderer's old 60 ms.
pub fn cursor_style(settings: &Option<Value>) -> (bool, CursorStyle) {
    let c = settings.as_ref().and_then(|s| s.get("cursor"));
    let num = |key: &str, default: f64| c.and_then(|c| c.get(key)).and_then(Value::as_f64).unwrap_or(default);
    let flag = |key: &str, default: bool| c.and_then(|c| c.get(key)).and_then(Value::as_bool).unwrap_or(default);
    let smoothing = num("smoothing", 0.5).clamp(0.0, 1.0);
    (
        flag("show", true),
        CursorStyle {
            size: num("size", 1.2).clamp(0.3, 4.0),
            half_life_ms: (smoothing * 120.0).max(1.0),
            ripple: flag("ripple", true),
            squish: flag("squish", true),
            hand: flag("hand", true),
        },
    )
}

/// The saved trim as a render range, or `None` when it covers the whole video.
pub fn trim_range(trim: &Option<Value>, duration_ms: u64) -> Option<(u64, u64)> {
    let t = trim.as_ref()?;
    let a = t.get("in").and_then(Value::as_f64)?.max(0.0) as u64;
    let b = (t.get("out").and_then(Value::as_f64)?.max(0.0) as u64).min(duration_ms);
    (b > a && (a > 0 || b < duration_ms)).then_some((a, b))
}

fn segments_for(file: &ProjectFile, log: &capturra_core::EventLog, duration_ms: u64) -> Result<Vec<StudioSegment>, String> {
    match &file.segments {
        Some(saved) => serde_json::from_value(saved.clone()).map_err(|e| format!("saved segments are not valid: {e}")),
        None => Ok(studio_segments(log, ZoomConfig::default(), duration_ms)),
    }
}

/// Render project `id`. `duration_ms` is the video's length (the caller probes it). Progress runs
/// 0..1; the final 1.0 is sent by the caller once this returns.
pub fn export_in(root: &Path, id: &str, opts: &ExportOptions, duration_ms: u64, progress: Progress) -> Result<PathBuf, String> {
    let dir = resolve_dir(root, id)?;
    let ext = match opts.format.as_str() {
        "" | "mp4" => "mp4",
        "gif" => "gif",
        other => return Err(format!("unknown export format {other:?} (use mp4 or gif)")),
    };
    let raw = read_log(&dir)?;
    let file = read_project_file(&dir)?;
    let log = raw.to_video_time().clipped_to(duration_ms);
    let display = (log.display.width, log.display.height);
    let timeline = timeline_from_studio(&segments_for(&file, &log, duration_ms)?, display);
    let name = if opts.name.trim().is_empty() { file.title.clone().unwrap_or_else(|| "export".to_string()) } else { opts.name.clone() };
    let out = output_path(&dir, &name, ext)?;
    let fps = if opts.fps == 0 { 30 } else { opts.fps.clamp(1, 120) };
    let frame = frame_style(&file.settings, display, &backgrounds::current_custom_dir()?)?;
    let aspect = frame.as_ref().and_then(|f| f.aspect).unwrap_or(display.0 / display.1);
    let width = target_width(&opts.res, aspect);
    // A GIF is cut from a temporary MP4; the render is most of the work, so it gets 90% of the bar.
    let (render_out, share) = if ext == "gif" { (out.with_extension("render.mp4"), 0.9) } else { (out.clone(), 1.0) };

    let report = progress.clone();
    let (show_cursor, cursor) = cursor_style(&file.settings);
    // The Audio tab's changes, applied after the trim: fades count from the start of the exported part.
    let sound = AudioSettings::from_settings(&file.settings);
    let (cut_from, cut_to) = trim_range(&file.trim, duration_ms).unwrap_or((0, duration_ms));
    let floor = if sound.enhance { crate::audio_edit::noise_floor_db(&dir.join(VIDEO)) } else { None };
    let audio_filter = sound.filter(0.0, (cut_to - cut_from) as f64 / 1000.0, floor);
    let render = capturra_render::render_preview(
        &dir.join(VIDEO),
        &log,
        &timeline,
        &render_out,
        &PreviewOptions {
            width,
            fps,
            draw_cursor: show_cursor,
            cursor,
            frame,
            assets_dir: Some(dir.clone()),
            trim: trim_range(&file.trim, duration_ms),
            audio_filter,
            mute_audio: sound.mute,
            encoder: if opts.encoder == "fast" { capturra_render::VideoEncoder::VideoToolbox } else { capturra_render::VideoEncoder::X264 },
            on_progress: Some(Box::new(move |f| report(f * share))),
        },
    );
    if let Err(e) = render {
        let _ = std::fs::remove_file(&render_out);
        return Err(format!("render failed: {e:#}"));
    }
    if ext == "gif" {
        let gif_width = if width == 0 { 960 } else { width.min(960) };
        let status = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-i"])
            .arg(&render_out)
            .args(["-vf", &gif_filter(fps.min(15), gif_width)])
            .arg(&out)
            .status()
            .map_err(|e| format!("failed to start ffmpeg for the GIF: {e}"));
        let _ = std::fs::remove_file(&render_out);
        if !status?.success() {
            let _ = std::fs::remove_file(&out);
            return Err("ffmpeg could not make the GIF".to_string());
        }
    }
    Ok(out)
}

#[tauri::command]
pub async fn export_project(app: AppHandle, id: String, options: ExportOptions) -> Result<ExportResult, String> {
    let root = recordings_root()?;
    let (emit_app, emit_id) = (app.clone(), id.clone());
    let emit: Progress = Arc::new(move |fraction| {
        let _ = emit_app.emit("export", serde_json::json!({ "id": emit_id, "fraction": fraction }));
    });
    let done = emit.clone();
    let path = tauri::async_runtime::spawn_blocking(move || {
        let dir = resolve_dir(&root, &id)?;
        let duration = capturra_render::probe_duration_ms(&dir.join(VIDEO)).map_err(|e| format!("{e:#}"))?;
        export_in(&root, &id, &options, duration, emit)
    })
    .await
    .map_err(|e| e.to_string())??;
    done(1.0);
    Ok(ExportResult { path: path.to_string_lossy().into_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;

    const WIDE: (f64, f64) = (2048.0, 1280.0);
    use crate::projects::{write_project_file, EVENTS};
    use serde_json::json;
    use std::sync::Mutex;

    #[test]
    fn names_lose_separators_and_leading_dots_and_are_never_empty() {
        assert_eq!(sanitize_name("Q4 / launch: demo"), "Q4 - launch- demo");
        assert_eq!(sanitize_name("..hidden"), "hidden");
        assert_eq!(sanitize_name("   "), "export");
        assert_eq!(sanitize_name("../../etc/passwd"), "-..-etc-passwd");
        assert_eq!(sanitize_name(&"x".repeat(500)).chars().count(), 120);
    }

    #[test]
    fn an_existing_export_is_never_overwritten() {
        let dir = temp_dir("out");
        let first = output_path(&dir, "Demo", "mp4").unwrap();
        assert_eq!(first, dir.join("exports").join("Demo.mp4"));
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(output_path(&dir, "Demo", "mp4").unwrap(), dir.join("exports").join("Demo 2.mp4"));
        assert_eq!(output_path(&dir, "Demo", "gif").unwrap(), dir.join("exports").join("Demo.gif"));
    }

    #[test]
    fn resolution_is_a_height_that_keeps_the_display_shape() {
        let d = 2048.0 / 1280.0;
        assert_eq!(target_width(&json!("1080"), d), 1728);
        assert_eq!(target_width(&json!(720), d), 1152);
        assert_eq!(target_width(&json!("720p"), 16.0 / 9.0), 1280);
        assert_eq!(target_width(&json!("1080"), 9.0 / 16.0), 608);
        assert_eq!(target_width(&json!("source"), d), 0);
        assert_eq!(target_width(&Value::Null, d), 0);
        assert_eq!(target_width(&json!("0"), d), 0);
    }

    #[test]
    fn a_trim_covering_everything_is_no_trim() {
        assert_eq!(trim_range(&None, 5000), None);
        assert_eq!(trim_range(&Some(json!({ "in": 0, "out": 5000 })), 5000), None);
        assert_eq!(trim_range(&Some(json!({ "in": 0, "out": 9000 })), 5000), None);
        assert_eq!(trim_range(&Some(json!({ "in": 500, "out": 4000 })), 5000), Some((500, 4000)));
        assert_eq!(trim_range(&Some(json!({ "in": 4000, "out": 500 })), 5000), None);
        assert_eq!(trim_range(&Some(json!({ "in": 100 })), 5000), None);
    }

    #[test]
    fn the_encoding_choice_defaults_to_the_small_file_and_a_project_without_it_still_parses() {
        let o: ExportOptions = serde_json::from_str(r#"{ "format": "mp4", "res": "1080", "fps": 30, "name": "x" }"#).unwrap();
        assert_eq!(o.encoder, "", "the Studio of an older version sends none: x264");
        let fast: ExportOptions = serde_json::from_str(r#"{ "format": "mp4", "encoder": "fast" }"#).unwrap();
        assert_eq!(fast.encoder, "fast");
    }

    #[test]
    fn cursor_settings_map_to_the_renderer_with_the_studio_defaults() {
        let (show, style) = cursor_style(&None);
        assert!(show);
        assert_eq!(style, CursorStyle { size: 1.2, half_life_ms: 60.0, ripple: true, squish: true, hand: true });
        let s =
            Some(json!({ "cursor": { "show": false, "size": 2.0, "smoothing": 0.0, "ripple": false, "squish": false, "hand": false } }));
        let (show, style) = cursor_style(&s);
        assert!(!show);
        assert_eq!(style, CursorStyle { size: 2.0, half_life_ms: 1.0, ripple: false, squish: false, hand: false });
        // out-of-range numbers are held inside what the renderer handles
        let (_, style) = cursor_style(&Some(json!({ "cursor": { "size": 99, "smoothing": 5 } })));
        assert_eq!((style.size, style.half_life_ms), (4.0, 120.0));
        // settings without a cursor block, or of the wrong type, fall back
        assert_eq!(cursor_style(&Some(json!({ "zoom": {} }))).1.size, 1.2);
        assert_eq!(cursor_style(&Some(json!("nope"))).1.size, 1.2);
    }

    fn fs(settings: &Option<Value>, display: (f64, f64)) -> Option<FrameStyle> {
        frame_style(settings, display, Path::new("/nonexistent")).unwrap()
    }

    #[test]
    fn background_types_pick_flat_colour_wallpaper_or_custom_picture() {
        let nodir = Path::new("/nonexistent");
        let flat = Some(json!({ "frame": { "type": "color", "color": "#102030" } }));
        assert_eq!(fs(&flat, WIDE).unwrap().backdrop, Backdrop::Gradient([0x10, 0x20, 0x30], [0x10, 0x20, 0x30]));
        // `type` decides: other background settings next to it are ignored
        let g = Some(json!({ "frame": { "type": "gradient", "bg": 1, "color": "#102030" } }));
        assert_eq!(fs(&g, WIDE).unwrap().backdrop, Backdrop::Gradient(BACKGROUNDS[1].0, BACKGROUNDS[1].1));
        let w = Some(json!({ "frame": { "type": "wallpaper", "wallpaper": "aurora" } }));
        match fs(&w, WIDE).unwrap().backdrop {
            Backdrop::Image(p) => assert_eq!((p.w, p.h), (1600, 1000)),
            other => panic!("{other:?}"),
        }
        // an unknown wallpaper or a missing/unsafe custom picture is an error, not a silent gradient
        let bad = Some(json!({ "frame": { "type": "wallpaper", "wallpaper": "nope" } }));
        assert!(frame_style(&bad, WIDE, nodir).unwrap_err().contains("nope"));
        let gone = Some(json!({ "frame": { "type": "custom", "image": "gone.png" } }));
        assert!(frame_style(&gone, WIDE, nodir).unwrap_err().contains("missing"));
        let evil = Some(json!({ "frame": { "type": "custom", "image": "../../x.png" } }));
        assert!(frame_style(&evil, WIDE, nodir).unwrap_err().contains("not a valid"));
        // a custom picture from the backgrounds folder
        let dir = std::env::temp_dir().join(format!("capturra-frame-custom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mine.png"), include_bytes!("../../public/backgrounds/dune.png")).unwrap();
        let c = Some(json!({ "frame": { "type": "custom", "image": "mine.png" } }));
        assert!(matches!(frame_style(&c, WIDE, &dir).unwrap().unwrap().backdrop, Backdrop::Image(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frame_settings_map_to_the_renderer_with_the_studio_defaults() {
        let f = fs(&None, WIDE).unwrap();
        assert_eq!((f.padding_pct, f.radius, f.shadow, f.aspect), (6.0, 14.0, 0.5, Some(16.0 / 9.0)));
        assert_eq!(f.backdrop, Backdrop::Gradient([0x6c, 0x63, 0xff], [0x3a, 0x2f, 0xb8]));
        let s = Some(json!({ "frame": { "bg": 3, "padding": 10, "radius": 0, "shadow": 0, "aspect": "9:16" } }));
        let f = fs(&s, WIDE).unwrap();
        assert_eq!(
            (f.backdrop, f.aspect, f.padding_pct),
            (Backdrop::Gradient([0x2c, 0x3e, 0x66], [0x0e, 0x16, 0x30]), Some(9.0 / 16.0), 10.0)
        );
        // a custom gradient (bg -1) uses the two chosen colours; a bad hex falls back to the first preset
        let s = Some(json!({ "frame": { "bg": -1, "c1": "#ff0000", "c2": "00ff00" } }));
        assert_eq!(fs(&s, WIDE).unwrap().backdrop, Backdrop::Gradient([255, 0, 0], [0, 255, 0]));
        let s = Some(json!({ "frame": { "bg": -1, "c1": "red", "c2": "#12" } }));
        assert_eq!(fs(&s, WIDE).unwrap().backdrop, Backdrop::Gradient(BACKGROUNDS[0].0, BACKGROUNDS[0].1));
        // numbers are held inside what the renderer handles
        let f = fs(&Some(json!({ "frame": { "padding": 99, "radius": 9999, "shadow": 7 } })), WIDE).unwrap();
        assert_eq!((f.padding_pct, f.radius, f.shadow), (40.0, 200.0, 1.0));
    }

    #[test]
    fn a_recording_that_is_not_wide_defaults_to_its_own_shape() {
        // 971 x 1178: a window. Nothing saved, so no forced 16:9 canvas.
        assert_eq!(fs(&None, (971.0, 1178.0)).unwrap().aspect, None);
        assert_eq!(fs(&None, (1000.0, 1000.0)).unwrap().aspect, None);
        assert_eq!(fs(&None, (2048.0, 1280.0)).unwrap().aspect, Some(16.0 / 9.0));
        // a choice made in the Studio always wins
        let chosen = Some(json!({ "frame": { "aspect": "16:9" } }));
        assert_eq!(fs(&chosen, (971.0, 1178.0)).unwrap().aspect, Some(16.0 / 9.0));
    }

    #[test]
    fn an_invisible_frame_is_no_frame() {
        let plain = Some(json!({ "frame": { "padding": 0, "radius": 0, "shadow": 0, "aspect": "source" } }));
        assert_eq!(fs(&plain, WIDE), None);
        let rounded = Some(json!({ "frame": { "padding": 0, "radius": 8, "shadow": 0, "aspect": "source" } }));
        assert!(fs(&rounded, WIDE).is_some());
    }

    #[test]
    fn the_gif_filter_names_its_size_and_rate() {
        let f = gif_filter(15, 960);
        assert!(f.starts_with("fps=15,scale=960:-1") && f.contains("palettegen") && f.contains("paletteuse"));
    }

    #[test]
    fn unknown_formats_and_missing_projects_are_errors() {
        let root = temp_dir("fmt");
        make_recording(&root, "r");
        let opts =
            |format: &str| ExportOptions { format: format.into(), res: Value::Null, fps: 30, name: String::new(), encoder: String::new() };
        let none: Progress = Arc::new(|_| {});
        assert!(export_in(&root, "r", &opts("webm"), 1000, none.clone()).unwrap_err().contains("webm"));
        assert!(export_in(&root, "nope", &opts("mp4"), 1000, none).is_err());
    }

    /// Makes a 2 s test clip with ffmpeg, then exports it for real: a trimmed MP4 and a GIF.
    #[test]
    fn renders_a_trimmed_mp4_and_a_gif_from_a_real_clip() {
        let root = temp_dir("e2e");
        let dir = make_recording(&root, "clip");
        let made = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x160:rate=30", "-t", "2", "-pix_fmt", "yuv420p"])
            .arg(dir.join(VIDEO))
            .status()
            .expect("ffmpeg is needed for this test (brew install ffmpeg)");
        assert!(made.success());
        let duration = capturra_render::probe_duration_ms(&dir.join(VIDEO)).unwrap();
        write_project_file(
            &dir,
            &ProjectFile { version: 1, title: Some("Clip".into()), trim: Some(json!({ "in": 500, "out": 1500 })), ..Default::default() },
        )
        .unwrap();

        let seen = Arc::new(Mutex::new(Vec::<f64>::new()));
        let sink = seen.clone();
        let progress: Progress = Arc::new(move |f| sink.lock().unwrap().push(f));
        let mp4 = export_in(
            &root,
            "clip",
            &ExportOptions { format: "mp4".into(), res: json!("80"), fps: 30, name: "My Cut".into(), encoder: String::new() },
            duration,
            progress,
        )
        .unwrap();
        assert_eq!(mp4, dir.join("exports").join("My Cut.mp4"));
        let out_ms = capturra_render::probe_duration_ms(&mp4).unwrap();
        assert!((900..=1100).contains(&out_ms), "trimmed to about 1 s, got {out_ms} ms");
        // res "80" with the default 16:9 frame = a 142 x 80 canvas (the clip itself is 2:1)
        let size = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(&mp4)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&size.stdout).trim(), "142,80");
        let seen = seen.lock().unwrap();
        assert!(seen.len() > 3 && seen.windows(2).all(|w| w[0] <= w[1]) && *seen.last().unwrap() <= 0.99, "{seen:?}");

        let gif = export_in(
            &root,
            "clip",
            &ExportOptions { format: "gif".into(), res: json!("80"), fps: 30, name: "My Cut".into(), encoder: String::new() },
            duration,
            Arc::new(|_| {}),
        )
        .unwrap();
        assert_eq!(gif, dir.join("exports").join("My Cut.gif"));
        assert!(std::fs::metadata(&gif).unwrap().len() > 100);
        assert!(!dir.join("exports").join("My Cut.render.mp4").exists(), "the temporary MP4 is removed");
        assert!(dir.join(EVENTS).is_file());
    }
}

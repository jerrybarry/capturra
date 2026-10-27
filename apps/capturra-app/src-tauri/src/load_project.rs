//! `load_project`: everything the editor needs to open one recording. The zoom segments come from
//! Rust (`capturra-core`) unless the project already has saved edits, in which case those win.

use crate::projects::{meta_for, read_log, read_project_file, recordings_root, resolve_dir, Meta, VIDEO};
use crate::studio_zoom::studio_segments;
use capturra_core::{EventLog, InputEvent, ZoomConfig};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use tauri::{AppHandle, Manager};

#[derive(Debug, Serialize)]
pub struct DisplayOut {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

/// A cursor image in the shape `src/studio/stage.ts` reads: `hotspot` as `[x, y]`, `width` and
/// `height` in points. (`events.json` stores `{x, y}` and `{w, h}`.)
#[derive(Debug, Serialize, PartialEq)]
pub struct CursorOut {
    pub id: u32,
    pub file: String,
    pub hotspot: [f64; 2],
    pub width: f64,
    pub height: f64,
    pub scale: f64,
    /// `"pointing_hand"` for the hand shown over clickable things (see `CursorImage::role`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Loaded {
    #[serde(flatten)]
    pub meta: Meta,
    pub display: DisplayOut,
    /// In video time (t = 0 is the first frame) and cut at the end of the video.
    pub events: Vec<InputEvent>,
    pub cursors: Vec<CursorOut>,
    pub segments: Value,
    pub settings: Option<Value>,
    pub trim: Option<Value>,
}

pub fn cursors_out(log: &EventLog) -> Vec<CursorOut> {
    log.cursors
        .iter()
        .map(|c| CursorOut {
            id: c.id,
            file: c.file.clone(),
            hotspot: [c.hotspot.x, c.hotspot.y],
            width: c.size.w,
            height: c.size.h,
            scale: c.scale,
            role: c.role.clone(),
        })
        .collect()
}

/// Load the project in `dir`. `duration_ms` is the video's length, probed by the caller.
pub fn load_dir(root: &Path, dir: &Path, duration_ms: u64) -> Result<Loaded, String> {
    let raw = read_log(dir)?;
    let file = read_project_file(dir)?;
    let log = raw.to_video_time().clipped_to(duration_ms);
    let segments = match &file.segments {
        Some(saved) => saved.clone(),
        None => serde_json::to_value(studio_segments(&log, ZoomConfig::default(), duration_ms)).map_err(|e| e.to_string())?,
    };
    Ok(Loaded {
        meta: meta_for(root, dir, &raw, &file, duration_ms),
        display: DisplayOut { width: log.display.width, height: log.display.height, scale: log.display.scale },
        events: log.sorted(),
        cursors: cursors_out(&log),
        segments,
        settings: file.settings,
        trim: file.trim,
    })
}

/// The automatic zooms of the project in `dir` in the named style (see `ZoomConfig::preset`), for the
/// Studio's style picker. Same engine as the first load and the exporter.
pub fn build_zoom_dir(dir: &Path, duration_ms: u64, preset: &str) -> Result<Value, String> {
    let config = ZoomConfig::preset(preset).ok_or_else(|| format!("unknown zoom style {preset:?}"))?;
    let log = read_log(dir)?.to_video_time().clipped_to(duration_ms);
    serde_json::to_value(studio_segments(&log, config, duration_ms)).map_err(|e| e.to_string())
}

/// Rebuild the automatic zooms in a style. The editor replaces its zooms with the result.
#[tauri::command]
pub async fn build_zoom(id: String, preset: String) -> Result<Value, String> {
    let root = recordings_root()?;
    let folder = resolve_dir(&root, &id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let duration = capturra_render::probe_duration_ms(&folder.join(VIDEO)).map_err(|e| format!("{e:#}"))?;
        build_zoom_dir(&folder, duration, &preset)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `id` is a library id; a recording picked from elsewhere arrives as `dir` with no id.
#[tauri::command]
pub async fn load_project(app: AppHandle, id: Option<String>, dir: Option<String>) -> Result<Loaded, String> {
    let root = recordings_root()?;
    let key = id.filter(|s| !s.trim().is_empty()).or(dir).ok_or("load_project needs an id or a dir")?;
    let folder = resolve_dir(&root, &key)?;
    // The recording may live outside ~/Movies/Capturra; let the webview play it.
    app.asset_protocol_scope().allow_directory(&folder, true).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let duration = capturra_render::probe_duration_ms(&folder.join(VIDEO)).map_err(|e| format!("{e:#}"))?;
        load_dir(&root, &folder, duration)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;
    use crate::projects::{write_project_file, ProjectFile};
    use capturra_core::{CursorImage, Hotspot, SizePt};

    #[test]
    fn computes_segments_in_rust_when_nothing_is_saved() {
        let root = temp_dir("load");
        let dir = make_recording(&root, "r");
        let l = load_dir(&root, &dir, 6000).unwrap();
        let segs = l.segments.as_array().unwrap();
        assert_eq!(segs.len(), 1); // the one click at 1.2 s
        assert_eq!(segs[0]["focus"][0]["x"], 500.0);
        assert_eq!(l.display.width, 1000.0);
        assert_eq!(l.meta.id, "r");
        assert!(l.settings.is_none() && l.trim.is_none());
    }

    #[test]
    fn a_zoom_style_rebuilds_the_zooms_with_that_styles_level_and_unknown_styles_are_refused() {
        let root = temp_dir("style");
        let dir = make_recording(&root, "r");
        let level = |style: &str| build_zoom_dir(&dir, 6000, style).unwrap()[0]["level"].as_f64().unwrap();
        let (gentle, standard, punchy) = (level("gentle"), level("standard"), level("punchy"));
        assert!(gentle < standard && standard < punchy, "{gentle} {standard} {punchy}");
        assert_eq!(standard, ZoomConfig::default().zoom_level);
        // the first load uses the same engine, so "standard" is what it showed
        assert_eq!(build_zoom_dir(&dir, 6000, "standard").unwrap(), load_dir(&root, &dir, 6000).unwrap().segments);
        assert!(build_zoom_dir(&dir, 6000, "wild").unwrap_err().contains("unknown zoom style"));
    }

    #[test]
    fn saved_edits_win_over_computed_ones() {
        let root = temp_dir("saved");
        let dir = make_recording(&root, "r");
        let saved = serde_json::json!([{ "id": "mine", "enabled": false }]);
        let file = ProjectFile {
            version: 1,
            segments: Some(saved.clone()),
            settings: Some(serde_json::json!({ "zoom": { "preset": "punchy" } })),
            trim: Some(serde_json::json!({ "in": 500, "out": 4000 })),
            ..Default::default()
        };
        write_project_file(&dir, &file).unwrap();
        let l = load_dir(&root, &dir, 6000).unwrap();
        assert_eq!(l.segments, saved);
        assert_eq!(l.settings, file.settings);
        assert_eq!(l.trim, file.trim);
    }

    #[test]
    fn events_are_rebased_to_video_time_and_cut_at_the_end() {
        let root = temp_dir("rebase");
        let dir = make_recording(&root, "r");
        let mut log = read_log(&dir).unwrap();
        log.video_offset_ms = 100;
        std::fs::write(dir.join(crate::projects::EVENTS), log.to_json().unwrap()).unwrap();
        let l = load_dir(&root, &dir, 1150).unwrap();
        // the click at 1200 on the recording clock is at 1100 in the video; the release at 1300 -> 1200 is past the end
        let times: Vec<u64> = l.events.iter().map(|e| e.t_ms()).collect();
        assert_eq!(times, vec![0, 1100]);
    }

    #[test]
    fn cursor_images_use_the_shape_the_stage_reads() {
        let mut log = EventLog::new(capturra_core::DisplayInfo { width: 100.0, height: 100.0, scale: 1.0 });
        log.cursors = vec![
            CursorImage {
                id: 3,
                file: "cursors/3.png".into(),
                hotspot: Hotspot { x: 4.0, y: 5.0 },
                scale: 2.0,
                size: SizePt { w: 17.0, h: 23.0 },
                role: None,
            },
            CursorImage {
                id: 4,
                file: "cursors/4.png".into(),
                hotspot: Hotspot { x: 13.0, y: 8.0 },
                scale: 2.0,
                size: SizePt { w: 32.0, h: 32.0 },
                role: Some(CursorImage::POINTING_HAND.into()),
            },
        ];
        let c = cursors_out(&log);
        assert_eq!(
            c[0],
            CursorOut { id: 3, file: "cursors/3.png".into(), hotspot: [4.0, 5.0], width: 17.0, height: 23.0, scale: 2.0, role: None }
        );
        let v = serde_json::to_value(&c[0]).unwrap();
        assert_eq!(v["hotspot"], serde_json::json!([4.0, 5.0]));
        assert!(v.get("role").is_none(), "no role, no key: older front ends are unaffected");
        assert_eq!(serde_json::to_value(&c[1]).unwrap()["role"], "pointing_hand", "the Studio finds the hand by its role");
    }

    #[test]
    fn the_card_fields_sit_beside_the_editor_fields_in_one_object() {
        let root = temp_dir("flat");
        let dir = make_recording(&root, "r");
        let v = serde_json::to_value(load_dir(&root, &dir, 6000).unwrap()).unwrap();
        for key in [
            "id",
            "title",
            "created_at",
            "duration_ms",
            "dir",
            "video",
            "kind",
            "display",
            "events",
            "cursors",
            "segments",
            "settings",
            "trim",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
    }
}

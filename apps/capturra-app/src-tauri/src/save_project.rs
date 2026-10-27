//! `save_project`: write the Studio's edits next to the recording as `project.json`. The
//! recording itself (`recording.mp4`, `events.json`) is never touched.

use crate::projects::{read_project_file, recordings_root, resolve_dir, write_project_file, ProjectFile};
use crate::studio_zoom::StudioSegment;
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct SaveRequest {
    pub title: Option<String>,
    pub segments: Option<Value>,
    pub settings: Option<Value>,
    pub trim: Option<Value>,
}

/// Times are stored as whole milliseconds. The timeline works in fractional ones while an edge is being
/// dragged (14524.48...), so round them here instead of rejecting the save: a refused save loses the edit.
fn round_ms(value: Option<&mut Value>) {
    if let Some(v) = value {
        if let Some(x) = v.as_f64().filter(|x| x.is_finite()) {
            *v = Value::from(x.round().max(0.0) as u64);
        }
    }
}

/// Round every time field of the Studio's segments (`t0`, `p0`, `p1`, `t1` and each focus point's `t`).
/// Anything else in a segment, including fields a newer Studio adds, is left exactly as given.
pub fn whole_millisecond_segments(segments: &mut Option<Value>) {
    let Some(list) = segments.as_mut().and_then(Value::as_array_mut) else { return };
    for seg in list {
        for key in ["t0", "p0", "p1", "t1"] {
            round_ms(seg.get_mut(key));
        }
        if let Some(focus) = seg.get_mut("focus").and_then(Value::as_array_mut) {
            for point in focus {
                round_ms(point.get_mut("t"));
            }
        }
    }
}

/// Refuse edits the exporter could not read, rather than saving something that fails later.
fn validate(p: &SaveRequest) -> Result<(), String> {
    if let Some(segs) = &p.segments {
        let list = segs.as_array().ok_or("segments must be a list")?;
        for (i, s) in list.iter().enumerate() {
            serde_json::from_value::<StudioSegment>(s.clone()).map_err(|e| format!("segment {} is not valid: {e}", i + 1))?;
        }
    }
    if let Some(trim) = &p.trim {
        let (a, b) = (trim.get("in").and_then(Value::as_f64), trim.get("out").and_then(Value::as_f64));
        match (a, b) {
            (Some(a), Some(b)) if a >= 0.0 && b > a => {}
            _ => return Err("trim needs numbers `in` and `out` with out after in".to_string()),
        }
    }
    Ok(())
}

pub fn save_in(root: &Path, id: &str, mut project: SaveRequest) -> Result<(), String> {
    let dir = resolve_dir(root, id)?;
    whole_millisecond_segments(&mut project.segments);
    if let Some(trim) = project.trim.as_mut() {
        round_ms(trim.get_mut("in"));
        round_ms(trim.get_mut("out"));
    }
    validate(&project)?;
    // A share link belongs to the recording, not to the editor, so keep what is already there.
    let existing = read_project_file(&dir)?;
    let file = ProjectFile {
        version: ProjectFile::VERSION,
        title: project.title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()),
        segments: project.segments,
        settings: project.settings,
        trim: project.trim,
        share_url: existing.share_url,
    };
    write_project_file(&dir, &file)
}

#[tauri::command]
pub async fn save_project(id: String, project: SaveRequest) -> Result<bool, String> {
    let root = recordings_root()?;
    tauri::async_runtime::spawn_blocking(move || save_in(&root, &id, project).map(|()| true)).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;
    use crate::projects::{EVENTS, VIDEO};
    use serde_json::json;

    fn seg() -> Value {
        json!({ "id": "z1", "level": 2.0, "enabled": true, "manual": null, "t0": 0, "p0": 500, "p1": 2000, "t1": 2600,
                "focus": [{ "t": 500, "x": 10.0, "y": 20.0 }] })
    }

    fn req(segments: Option<Value>, trim: Option<Value>) -> SaveRequest {
        SaveRequest { title: Some(" Demo ".into()), segments, settings: Some(json!({ "export": { "fps": 30 } })), trim }
    }

    #[test]
    fn saves_and_reads_back_without_touching_the_recording() {
        let root = temp_dir("save");
        let dir = make_recording(&root, "r");
        let (v, e) = (std::fs::read(dir.join(VIDEO)).unwrap(), std::fs::read(dir.join(EVENTS)).unwrap());
        save_in(&root, "r", req(Some(json!([seg()])), Some(json!({ "in": 0, "out": 5000 })))).unwrap();
        let file = read_project_file(&dir).unwrap();
        assert_eq!(file.title.as_deref(), Some("Demo"));
        assert_eq!(file.segments, Some(json!([seg()])));
        assert_eq!(file.settings, Some(json!({ "export": { "fps": 30 } })));
        assert_eq!((std::fs::read(dir.join(VIDEO)).unwrap(), std::fs::read(dir.join(EVENTS)).unwrap()), (v, e));
    }

    /// The bug this guards: dragging a zoom edge sent 14524.480438184662 and the save was refused.
    #[test]
    fn fractional_milliseconds_from_a_drag_are_rounded_not_rejected() {
        let root = temp_dir("frac");
        let dir = make_recording(&root, "r");
        let mut s = seg();
        s["t0"] = json!(14000.4);
        s["p0"] = json!(14524.480438184662);
        s["p1"] = json!(16999.5);
        s["t1"] = json!(17600.0);
        s["focus"] = json!([{ "t": 14524.480438184662, "x": 10.5, "y": 20.25 }]);
        save_in(&root, "r", req(Some(json!([s])), Some(json!({ "in": 10.6, "out": 4999.7 })))).unwrap();
        let file = read_project_file(&dir).unwrap();
        let saved = &file.segments.unwrap()[0];
        assert_eq!(
            (saved["t0"].clone(), saved["p0"].clone(), saved["p1"].clone(), saved["t1"].clone()),
            (json!(14000), json!(14524), json!(17000), json!(17600))
        );
        assert_eq!(saved["focus"][0]["t"], json!(14524));
        assert_eq!(
            (saved["focus"][0]["x"].clone(), saved["focus"][0]["y"].clone()),
            (json!(10.5), json!(20.25)),
            "positions stay fractional"
        );
        assert_eq!(file.trim.unwrap(), json!({ "in": 11, "out": 5000 }));
        // what was saved is what export and load read back
        assert!(serde_json::from_value::<Vec<StudioSegment>>(read_project_file(&dir).unwrap().segments.unwrap()).is_ok());
    }

    #[test]
    fn a_time_dragged_before_zero_becomes_zero_and_bad_types_are_still_refused() {
        let root = temp_dir("neg");
        let dir = make_recording(&root, "r");
        let mut s = seg();
        s["t0"] = json!(-0.4);
        s["p0"] = json!(-30.2);
        save_in(&root, "r", req(Some(json!([s])), None)).unwrap();
        let saved = read_project_file(&dir).unwrap().segments.unwrap();
        assert_eq!((saved[0]["t0"].clone(), saved[0]["p0"].clone()), (json!(0), json!(0)));
        let mut bad = seg();
        bad["p0"] = json!("soon");
        assert!(save_in(&root, "r", req(Some(json!([bad])), None)).unwrap_err().contains("segment 1"));
    }

    #[test]
    fn extra_fields_the_studio_adds_are_kept_as_given() {
        let root = temp_dir("extra");
        let dir = make_recording(&root, "r");
        let mut s = seg();
        s["future_field"] = json!("kept");
        save_in(&root, "r", req(Some(json!([s.clone()])), None)).unwrap();
        assert_eq!(read_project_file(&dir).unwrap().segments, Some(json!([s])));
    }

    #[test]
    fn a_share_link_survives_a_save() {
        let root = temp_dir("share");
        let dir = make_recording(&root, "r");
        write_project_file(&dir, &ProjectFile { version: 1, share_url: Some("https://x/y".into()), ..Default::default() }).unwrap();
        save_in(&root, "r", req(None, None)).unwrap();
        assert_eq!(read_project_file(&dir).unwrap().share_url.as_deref(), Some("https://x/y"));
    }

    #[test]
    fn bad_edits_are_refused_and_leave_the_old_file_alone() {
        let root = temp_dir("bad");
        let dir = make_recording(&root, "r");
        save_in(&root, "r", req(Some(json!([seg()])), None)).unwrap();
        let before = std::fs::read(dir.join("project.json")).unwrap();
        assert!(save_in(&root, "r", req(Some(json!({ "not": "a list" })), None)).unwrap_err().contains("list"));
        assert!(save_in(&root, "r", req(Some(json!([{ "id": "z" }])), None)).unwrap_err().contains("segment 1"));
        assert!(save_in(&root, "r", req(None, Some(json!({ "in": 5000, "out": 100 })))).unwrap_err().contains("trim"));
        assert_eq!(std::fs::read(dir.join("project.json")).unwrap(), before);
    }

    #[test]
    fn unknown_or_escaping_ids_are_refused() {
        let root = temp_dir("id");
        assert!(save_in(&root, "missing", req(None, None)).is_err());
        assert!(save_in(&root, "../x", req(None, None)).is_err());
    }
}

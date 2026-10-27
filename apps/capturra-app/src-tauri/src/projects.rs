//! Where recordings live and how the Studio names them. A project is a recording folder
//! (`recording.mp4` + `events.json` + `cursors/`) plus an optional `project.json` holding the
//! edits. The id is the folder name under `~/Movies/Capturra`, or the absolute path of a
//! recording picked from somewhere else. Nothing here touches Tauri, so it tests anywhere.

use capturra_core::{EventLog, InputEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

pub const VIDEO: &str = "recording.mp4";
pub const EVENTS: &str = "events.json";
pub const PROJECT: &str = "project.json";

/// Where recordings live: the folder chosen in the settings, else `~/Movies/Capturra`.
pub fn recordings_root() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let settings = crate::settings::load(&crate::settings::config_path()?).settings;
    Ok(crate::settings::recordings_dir(&home, &settings))
}

pub fn is_recording_dir(dir: &Path) -> bool {
    dir.join(VIDEO).is_file() && dir.join(EVENTS).is_file()
}

/// Turn a project id into its folder. A bare folder name lives under `root`; an absolute path
/// (a recording picked from elsewhere) must itself be a recording folder. Anything that could
/// climb out of `root` (`..`, separators in a relative id) is refused.
pub fn resolve_dir(root: &Path, id: &str) -> Result<PathBuf, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("no project id".to_string());
    }
    let path = Path::new(id);
    let dir = if path.is_absolute() {
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(format!("{id:?} is not a valid recording folder"));
        }
        path.to_path_buf()
    } else {
        let mut parts = path.components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(name)), None) => root.join(name),
            _ => return Err(format!("{id:?} is not a valid project id")),
        }
    };
    if !is_recording_dir(&dir) {
        return Err(format!("{} does not hold {VIDEO} and {EVENTS}", dir.display()));
    }
    Ok(dir)
}

/// The id the Studio uses for `dir`: its folder name when it sits directly under `root`.
pub fn id_for(root: &Path, dir: &Path) -> String {
    match (dir.parent(), dir.file_name()) {
        (Some(parent), Some(name)) if parent == root => name.to_string_lossy().into_owned(),
        _ => dir.to_string_lossy().into_owned(),
    }
}

/// The edits saved next to a recording. `segments`, `settings` and `trim` are the Studio's own
/// JSON, kept as given so a newer Studio can add fields without a Rust change.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    pub title: Option<String>,
    pub segments: Option<Value>,
    pub settings: Option<Value>,
    pub trim: Option<Value>,
    pub share_url: Option<String>,
}

impl ProjectFile {
    pub const VERSION: u32 = 1;
}

/// Missing file = no edits yet. A file that exists but cannot be read is an error, not "no edits".
pub fn read_project_file(dir: &Path) -> Result<ProjectFile, String> {
    let path = dir.join(PROJECT);
    if !path.exists() {
        return Ok(ProjectFile::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let file: ProjectFile = serde_json::from_str(&text).map_err(|e| format!("{} is not valid: {e}", path.display()))?;
    if file.version > ProjectFile::VERSION {
        return Err(format!("{} was saved by a newer Capturra (version {})", path.display(), file.version));
    }
    Ok(file)
}

/// Write through a temporary file and rename, so a crash cannot leave half a project.
pub fn write_project_file(dir: &Path, file: &ProjectFile) -> Result<(), String> {
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{PROJECT}.tmp"));
    std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dir.join(PROJECT)).map_err(|e| format!("saving {}: {e}", dir.join(PROJECT).display()))
}

pub fn read_log(dir: &Path) -> Result<EventLog, String> {
    let path = dir.join(EVENTS);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    EventLog::from_json(&text).map_err(|e| format!("{} is not a valid events file: {e}", path.display()))
}

/// The library card for a recording.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Meta {
    pub id: String,
    pub title: String,
    pub created_at: u64,
    pub duration_ms: u64,
    pub width: f64,
    pub height: f64,
    pub dir: String,
    pub video: String,
    pub events: String,
    pub thumb_at: u64,
    pub share_url: Option<String>,
    pub kind: &'static str,
}

/// Build the card from what is on disk. `duration_ms` is the video's length (the caller probes
/// it; tests pass a number).
pub fn meta_for(root: &Path, dir: &Path, log: &EventLog, file: &ProjectFile, duration_ms: u64) -> Meta {
    let created_at = std::fs::metadata(dir)
        .and_then(|m| m.created().or_else(|_| m.modified()))
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let id = id_for(root, dir);
    let title = file
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.clone()));
    Meta {
        id,
        title,
        created_at,
        duration_ms,
        width: log.display.width,
        height: log.display.height,
        dir: dir.to_string_lossy().into_owned(),
        video: dir.join(VIDEO).to_string_lossy().into_owned(),
        events: dir.join(EVENTS).to_string_lossy().into_owned(),
        thumb_at: thumb_at(log, duration_ms),
        share_url: file.share_url.clone(),
        kind: "recording",
    }
}

/// A frame worth showing on the card: the first click (something is happening), else one second in.
fn thumb_at(log: &EventLog, duration_ms: u64) -> u64 {
    let video = log.to_video_time();
    let first_click = video.events.iter().find_map(|e| match e {
        InputEvent::Down { t_ms, .. } => Some(*t_ms),
        _ => None,
    });
    first_click.unwrap_or(1000).min(duration_ms.saturating_sub(1))
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use capturra_core::DisplayInfo;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    /// A fresh empty directory under the system temp dir.
    pub fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capturra-test-{}-{tag}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub fn sample_log() -> EventLog {
        let mut log = EventLog::new(DisplayInfo { width: 1000.0, height: 500.0, scale: 1.0 });
        log.events = vec![
            InputEvent::Move { t_ms: 100, x: 100.0, y: 100.0 },
            InputEvent::Down { t_ms: 1200, x: 500.0, y: 250.0, button: capturra_core::MouseButton::Left },
            InputEvent::Up { t_ms: 1300, x: 500.0, y: 250.0, button: capturra_core::MouseButton::Left },
        ];
        log
    }

    /// A recording folder with a placeholder video and a real events file.
    pub fn make_recording(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(VIDEO), b"not a real video").unwrap();
        std::fs::write(dir.join(EVENTS), sample_log().to_json().unwrap()).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;

    #[test]
    fn a_bare_folder_name_resolves_under_the_root() {
        let root = temp_dir("resolve");
        let dir = make_recording(&root, "2026-10-24 15.50.01");
        assert_eq!(resolve_dir(&root, "2026-10-24 15.50.01").unwrap(), dir);
        assert_eq!(id_for(&root, &dir), "2026-10-24 15.50.01");
    }

    #[test]
    fn ids_cannot_climb_out_of_the_root() {
        let root = temp_dir("climb");
        make_recording(&root, "ok");
        for bad in ["", "..", "../ok", "ok/..", "a/b", "/etc/../etc", "ok/"] {
            // "ok/" normalises to one component, so it is the only one allowed to succeed
            let r = resolve_dir(&root, bad);
            if bad == "ok/" {
                assert!(r.is_ok());
            } else {
                assert!(r.is_err(), "{bad:?} should be refused");
            }
        }
    }

    #[test]
    fn an_absolute_path_must_be_a_recording_folder() {
        let root = temp_dir("abs");
        let elsewhere = temp_dir("elsewhere");
        let dir = make_recording(&elsewhere, "picked");
        assert_eq!(resolve_dir(&root, dir.to_str().unwrap()).unwrap(), dir);
        assert_eq!(id_for(&root, &dir), dir.to_string_lossy());
        assert!(resolve_dir(&root, elsewhere.to_str().unwrap()).is_err());
    }

    #[test]
    fn project_file_round_trips_and_missing_means_no_edits() {
        let root = temp_dir("pf");
        let dir = make_recording(&root, "r");
        assert_eq!(read_project_file(&dir).unwrap(), ProjectFile::default());
        let file = ProjectFile {
            version: ProjectFile::VERSION,
            title: Some("Demo".into()),
            segments: Some(serde_json::json!([{ "id": "a" }])),
            ..Default::default()
        };
        write_project_file(&dir, &file).unwrap();
        assert_eq!(read_project_file(&dir).unwrap(), file);
        assert!(!dir.join("project.json.tmp").exists());
    }

    #[test]
    fn a_corrupt_or_newer_project_file_is_an_error_not_silently_empty() {
        let root = temp_dir("bad");
        let dir = make_recording(&root, "r");
        std::fs::write(dir.join(PROJECT), "{ nope").unwrap();
        assert!(read_project_file(&dir).unwrap_err().contains("not valid"));
        std::fs::write(dir.join(PROJECT), r#"{"version": 99}"#).unwrap();
        assert!(read_project_file(&dir).unwrap_err().contains("newer"));
    }

    #[test]
    fn meta_uses_the_saved_title_else_the_folder_name_and_the_first_click_for_the_thumbnail() {
        let root = temp_dir("meta");
        let dir = make_recording(&root, "my folder");
        let log = read_log(&dir).unwrap();
        let m = meta_for(&root, &dir, &log, &ProjectFile::default(), 5000);
        assert_eq!((m.title.as_str(), m.id.as_str(), m.thumb_at), ("my folder", "my folder", 1200));
        assert_eq!((m.width, m.height, m.kind), (1000.0, 500.0, "recording"));
        let named = ProjectFile { title: Some("  ".into()), ..Default::default() };
        assert_eq!(meta_for(&root, &dir, &log, &named, 5000).title, "my folder");
        let named = ProjectFile { title: Some("Onboarding".into()), ..Default::default() };
        assert_eq!(meta_for(&root, &dir, &log, &named, 5000).title, "Onboarding");
        // a thumbnail past the end of a short video is pulled back inside it
        assert_eq!(meta_for(&root, &dir, &log, &ProjectFile::default(), 800).thumb_at, 799);
    }
}

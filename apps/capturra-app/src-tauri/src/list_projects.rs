//! `list_projects`: the Library. Every folder under `~/Movies/Capturra` that holds a recording,
//! newest first.

use crate::projects::{is_recording_dir, meta_for, read_log, read_project_file, recordings_root, Meta, VIDEO};
use std::path::Path;

/// Scan `root`. `probe` reads a video's length. A folder that cannot be read is skipped with a
/// note on stderr, but if nothing at all could be read the first reason is returned, so a missing
/// ffprobe shows up as an error instead of an empty Library.
pub fn list_in(root: &Path, probe: &dyn Fn(&Path) -> Result<u64, String>) -> Result<Vec<Meta>, String> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(Vec::new()); // nothing recorded yet
    };
    let (mut out, mut first_error) = (Vec::new(), None::<String>);
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !is_recording_dir(&dir) {
            continue;
        }
        let built = (|| {
            let log = read_log(&dir)?;
            let file = read_project_file(&dir)?;
            let duration = probe(&dir.join(VIDEO))?;
            Ok::<_, String>(meta_for(root, &dir, &log, &file, duration))
        })();
        match built {
            Ok(meta) => out.push(meta),
            Err(e) => {
                eprintln!("library: skipping {}: {e}", dir.display());
                first_error.get_or_insert(e);
            }
        }
    }
    if out.is_empty() {
        if let Some(e) = first_error {
            return Err(e);
        }
    }
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| b.id.cmp(&a.id)));
    Ok(out)
}

#[tauri::command]
pub async fn list_projects() -> Result<Vec<Meta>, String> {
    let root = recordings_root()?;
    tauri::async_runtime::spawn_blocking(move || {
        list_in(&root, &|video| capturra_render::probe_duration_ms(video).map_err(|e| format!("{e:#}")))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;
    use crate::projects::{write_project_file, ProjectFile, EVENTS};

    #[test]
    fn missing_root_is_an_empty_library() {
        let root = temp_dir("nolib").join("nope");
        assert_eq!(list_in(&root, &|_| Ok(1000)).unwrap(), vec![]);
    }

    #[test]
    fn lists_recordings_and_ignores_other_folders_and_failed_runs() {
        let root = temp_dir("lib");
        make_recording(&root, "a");
        make_recording(&root, "b");
        std::fs::create_dir_all(root.join("empty")).unwrap();
        // a run that failed before events.json was written is not a project
        let video_only = root.join("video-only");
        std::fs::create_dir_all(&video_only).unwrap();
        std::fs::write(video_only.join(VIDEO), b"x").unwrap();
        std::fs::write(root.join("loose.txt"), b"x").unwrap();
        let list = list_in(&root, &|_| Ok(4000)).unwrap();
        let mut ids: Vec<_> = list.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["a", "b"]);
        assert!(list.iter().all(|m| m.duration_ms == 4000 && m.kind == "recording"));
    }

    #[test]
    fn saved_titles_show_up() {
        let root = temp_dir("title");
        let dir = make_recording(&root, "a");
        write_project_file(&dir, &ProjectFile { version: 1, title: Some("Onboarding".into()), ..Default::default() }).unwrap();
        assert_eq!(list_in(&root, &|_| Ok(1)).unwrap()[0].title, "Onboarding");
    }

    #[test]
    fn one_broken_recording_is_skipped_but_the_rest_still_list() {
        let root = temp_dir("broken");
        make_recording(&root, "good");
        let bad = make_recording(&root, "bad");
        std::fs::write(bad.join(EVENTS), "{ not json").unwrap();
        let list = list_in(&root, &|_| Ok(1)).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "good");
    }

    #[test]
    fn if_nothing_can_be_read_the_reason_is_returned_not_an_empty_list() {
        let root = temp_dir("noprobe");
        make_recording(&root, "a");
        let err = list_in(&root, &|_| Err("failed to run ffprobe".to_string())).unwrap_err();
        assert!(err.contains("ffprobe"));
    }
}

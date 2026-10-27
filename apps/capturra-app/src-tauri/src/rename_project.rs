//! `rename_project`: change a recording's title without touching its edits (zoom segments,
//! settings, trim, share link) or the recording itself.

use crate::projects::{read_project_file, recordings_root, resolve_dir, write_project_file, ProjectFile};
use std::path::Path;

const MAX_TITLE_CHARS: usize = 200;

pub fn rename_in(root: &Path, id: &str, title: &str) -> Result<String, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("a title cannot be empty".to_string());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("a title can be at most {MAX_TITLE_CHARS} characters"));
    }
    let dir = resolve_dir(root, id)?;
    let existing = read_project_file(&dir)?;
    write_project_file(&dir, &ProjectFile { version: ProjectFile::VERSION, title: Some(title.to_string()), ..existing })?;
    Ok(title.to_string())
}

/// Returns the title as saved (trimmed).
#[tauri::command]
pub async fn rename_project(id: String, title: String) -> Result<String, String> {
    let root = recordings_root()?;
    tauri::async_runtime::spawn_blocking(move || rename_in(&root, &id, &title)).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;
    use crate::projects::{EVENTS, VIDEO};
    use serde_json::json;

    #[test]
    fn renaming_keeps_every_edit_and_the_recording() {
        let root = temp_dir("rename");
        let dir = make_recording(&root, "r");
        let edits = ProjectFile {
            version: 1,
            title: Some("Old".into()),
            segments: Some(json!([{ "id": "z1" }])),
            settings: Some(json!({ "frame": { "padding": 9 } })),
            trim: Some(json!({ "in": 1, "out": 2 })),
            share_url: Some("https://x/y".into()),
        };
        write_project_file(&dir, &edits).unwrap();
        let before = (std::fs::read(dir.join(VIDEO)).unwrap(), std::fs::read(dir.join(EVENTS)).unwrap());
        assert_eq!(rename_in(&root, "r", "  Onboarding v2  ").unwrap(), "Onboarding v2");
        assert_eq!(read_project_file(&dir).unwrap(), ProjectFile { title: Some("Onboarding v2".into()), ..edits });
        assert_eq!((std::fs::read(dir.join(VIDEO)).unwrap(), std::fs::read(dir.join(EVENTS)).unwrap()), before);
    }

    #[test]
    fn a_recording_with_no_saved_edits_can_be_renamed() {
        let root = temp_dir("rename2");
        let dir = make_recording(&root, "r");
        rename_in(&root, "r", "Demo").unwrap();
        assert_eq!(read_project_file(&dir).unwrap().title.as_deref(), Some("Demo"));
    }

    #[test]
    fn empty_long_or_unknown_names_are_refused() {
        let root = temp_dir("rename3");
        make_recording(&root, "r");
        assert!(rename_in(&root, "r", "   ").unwrap_err().contains("empty"));
        assert!(rename_in(&root, "r", &"x".repeat(201)).unwrap_err().contains("200"));
        assert!(rename_in(&root, "missing", "x").is_err());
        assert!(rename_in(&root, "../x", "x").is_err());
    }
}

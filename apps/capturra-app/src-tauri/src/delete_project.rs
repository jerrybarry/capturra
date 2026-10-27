//! `delete_project`: remove a recording from the Library. The folder is moved to the Trash, not
//! erased, so a wrong click is recoverable. Only recordings directly under `~/Movies/Capturra`
//! can be deleted this way; a recording the user picked from elsewhere is never touched.

use crate::projects::{recordings_root, resolve_dir};
use std::path::{Path, PathBuf};

/// A name in `trash` that does not exist yet: `name`, then `name 2`, `name 3`, ...
fn free_name(trash: &Path, name: &str) -> PathBuf {
    let first = trash.join(name);
    if !first.exists() {
        return first;
    }
    (2..).map(|n| trash.join(format!("{name} {n}"))).find(|p| !p.exists()).unwrap()
}

pub fn delete_in(root: &Path, trash: &Path, id: &str) -> Result<(), String> {
    if Path::new(id).is_absolute() {
        return Err("only recordings in the Capturra folder can be deleted from the Library".to_string());
    }
    let dir = resolve_dir(root, id)?;
    std::fs::create_dir_all(trash).map_err(|e| format!("could not open the Trash: {e}"))?;
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("recording has no folder name")?;
    let target = free_name(trash, &name);
    std::fs::rename(&dir, &target).map_err(|e| format!("could not move {} to the Trash: {e}", dir.display()))
}

#[tauri::command]
pub async fn delete_project(id: String) -> Result<bool, String> {
    let root = recordings_root()?;
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    tauri::async_runtime::spawn_blocking(move || delete_in(&root, &home.join(".Trash"), &id).map(|()| true))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;

    #[test]
    fn moves_the_folder_to_the_trash_with_its_files() {
        let root = temp_dir("del");
        let trash = temp_dir("trash");
        let dir = make_recording(&root, "r");
        delete_in(&root, &trash, "r").unwrap();
        assert!(!dir.exists());
        assert!(trash.join("r").join("recording.mp4").is_file());
    }

    #[test]
    fn a_name_already_in_the_trash_is_not_overwritten() {
        let root = temp_dir("del2");
        let trash = temp_dir("trash2");
        std::fs::create_dir_all(trash.join("r")).unwrap();
        make_recording(&root, "r");
        delete_in(&root, &trash, "r").unwrap();
        assert!(trash.join("r 2").join("events.json").is_file());
        assert!(trash.join("r").is_dir());
    }

    #[test]
    fn only_known_recordings_directly_under_the_root_can_go() {
        let root = temp_dir("del3");
        let trash = temp_dir("trash3");
        let elsewhere = temp_dir("elsewhere3");
        let picked = make_recording(&elsewhere, "picked");
        std::fs::create_dir_all(root.join("not-a-recording")).unwrap();
        for bad in ["", "..", "../x", "missing", "not-a-recording", picked.to_str().unwrap()] {
            assert!(delete_in(&root, &trash, bad).is_err(), "{bad:?}");
        }
        assert!(picked.join("recording.mp4").is_file());
        assert!(root.join("not-a-recording").is_dir());
    }
}

//! `pick_recording`: let the user choose a recording folder from anywhere (Open recording...).
//! Uses the system folder chooser through `osascript`, so it needs no extra dependency.

use crate::projects::{is_recording_dir, EVENTS, VIDEO};
use std::path::{Path, PathBuf};

/// Interpret `osascript` finishing: the chosen folder, or `"cancelled"` when the user closed the dialog.
pub fn parse_choose_folder(success: bool, stdout: &str, stderr: &str) -> Result<PathBuf, String> {
    if !success {
        let reason = stderr.trim();
        return Err(if reason.contains("-128") || reason.to_lowercase().contains("cancel") {
            "cancelled".to_string()
        } else {
            format!("could not open the folder chooser: {reason}")
        });
    }
    let path = stdout.trim().trim_end_matches('/');
    if path.is_empty() {
        return Err("cancelled".to_string());
    }
    Ok(PathBuf::from(path))
}

/// The chosen folder must be a recording, with an error that says what to pick instead.
pub fn validate(dir: &Path) -> Result<String, String> {
    if !is_recording_dir(dir) {
        return Err(format!(
            "{} is not a Capturra recording: it needs {VIDEO} and {EVENTS}. Pick the folder that holds them.",
            dir.display()
        ));
    }
    Ok(dir.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn pick_recording() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = std::process::Command::new("osascript")
            .args(["-e", r#"POSIX path of (choose folder with prompt "Choose a Capturra recording folder")"#])
            .output()
            .map_err(|e| format!("could not run osascript: {e}"))?;
        let dir = parse_choose_folder(out.status.success(), &String::from_utf8_lossy(&out.stdout), &String::from_utf8_lossy(&out.stderr))?;
        validate(&dir)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::*;

    #[test]
    fn a_chosen_folder_loses_its_trailing_slash() {
        assert_eq!(parse_choose_folder(true, "/Users/me/Movies/Capturra/a/\n", "").unwrap(), PathBuf::from("/Users/me/Movies/Capturra/a"));
    }

    #[test]
    fn closing_the_dialog_is_reported_as_cancelled() {
        let err = "execution error: User canceled. (-128)";
        assert_eq!(parse_choose_folder(false, "", err).unwrap_err(), "cancelled");
        assert_eq!(parse_choose_folder(true, "  \n", "").unwrap_err(), "cancelled");
    }

    #[test]
    fn other_osascript_failures_are_not_mistaken_for_cancelling() {
        let err = parse_choose_folder(false, "", "osascript is not allowed assistive access").unwrap_err();
        assert!(err.contains("folder chooser") && err != "cancelled");
    }

    #[test]
    fn only_a_recording_folder_is_accepted() {
        let root = temp_dir("pick");
        let dir = make_recording(&root, "r");
        assert_eq!(validate(&dir).unwrap(), dir.to_string_lossy());
        let err = validate(&root).unwrap_err();
        assert!(err.contains("recording.mp4") && err.contains("events.json"));
    }
}

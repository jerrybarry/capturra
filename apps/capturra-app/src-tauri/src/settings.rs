//! App settings: where recordings go, what was recorded last, and the defaults the popover uses.
//! Stored as JSON in `~/Library/Application Support/<app identifier>/settings.json`. No Tauri types
//! here, so it tests anywhere.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const FILE: &str = "settings.json";
pub const IDENTIFIER: &str = "app.capturra.dev";
pub const DEFAULT_HOTKEY: &str = "CommandOrControl+Shift+2";

/// The source the popover had selected. A window is not remembered: its id changes every launch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LastSource {
    pub kind: String,
    pub display: Option<usize>,
    pub region: Option<[f64; 4]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// `None` = `~/Movies/Capturra`.
    pub recordings_dir: Option<String>,
    pub last_source: Option<LastSource>,
    pub fps: u32,
    pub countdown: u32,
    /// Start/stop shortcut in Tauri's format (`CommandOrControl+Shift+2`). Empty = no shortcut.
    pub hotkey: String,
    /// Keep Capturra's own windows (recording bar, countdown, menu-bar timer) in display and region
    /// recordings, for a demo of Capturra itself. Off by default.
    pub show_capturra: bool,
    /// Record system audio (everything the Mac plays except Capturra's own sound; macOS 13+) with every
    /// recording, however it is started. Off by default.
    pub system_audio: bool,
    /// Record the default microphone with every recording (needs the Microphone permission). Off by default.
    pub microphone: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: 1,
            recordings_dir: None,
            last_source: None,
            fps: 30,
            countdown: 3,
            hotkey: DEFAULT_HOTKEY.to_string(),
            show_capturra: false,
            system_audio: false,
            microphone: false,
        }
    }
}

/// What `get_settings` returns: the settings, plus a message if the file on disk had to be set aside.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loaded {
    pub settings: Settings,
    pub warning: Option<String>,
}

pub fn config_dir(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join(IDENTIFIER)
}

pub fn config_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    Ok(config_dir(&home).join(FILE))
}

pub fn default_recordings_dir(home: &Path) -> PathBuf {
    home.join("Movies").join("Capturra")
}

/// Missing file = defaults. A file that cannot be understood is renamed to `settings.json.bad`
/// (kept, not deleted) and defaults are used, with a warning for the UI.
pub fn load(path: &Path) -> Loaded {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Loaded { settings: Settings::default(), warning: None };
    };
    match serde_json::from_str::<Settings>(&text) {
        Ok(s) if s.version <= Settings::default().version => Loaded { settings: s, warning: None },
        other => {
            let why = match other {
                Ok(s) => format!("saved by a newer Capturra (version {})", s.version),
                Err(e) => e.to_string(),
            };
            let bad = path.with_extension("json.bad");
            let moved = std::fs::rename(path, &bad).is_ok();
            let warning = format!(
                "Settings could not be read ({why}); using defaults.{}",
                if moved { format!(" The old file was kept as {}.", bad.display()) } else { String::new() }
            );
            Loaded { settings: Settings::default(), warning: Some(warning) }
        }
    }
}

/// Check a settings change before it is saved. Creates the recordings folder to prove it is usable.
pub fn validate(s: &Settings) -> Result<(), String> {
    if !(15..=60).contains(&s.fps) {
        return Err(format!("frame rate must be between 15 and 60, not {}", s.fps));
    }
    if s.countdown > 10 {
        return Err(format!("countdown must be 10 seconds or less, not {}", s.countdown));
    }
    if let Some(dir) = &s.recordings_dir {
        let path = Path::new(dir);
        if !path.is_absolute() || path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("{dir:?} is not a full folder path"));
        }
        std::fs::create_dir_all(path).map_err(|e| format!("cannot use {dir}: {e}"))?;
        let probe = path.join(".capturra-write-test");
        std::fs::write(&probe, b"").map_err(|e| format!("{dir} is not writable: {e}"))?;
        let _ = std::fs::remove_file(probe);
    }
    if !s.hotkey.is_empty() && !s.hotkey.contains('+') {
        return Err("the shortcut needs a modifier, for example CommandOrControl+Shift+2".to_string());
    }
    Ok(())
}

/// Write through a temporary file and rename, so a crash cannot leave half a settings file.
pub fn save(path: &Path, s: &Settings) -> Result<(), String> {
    validate(s)?;
    let dir = path.parent().ok_or("settings path has no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("saving {}: {e}", path.display()))
}

/// The folder recordings are saved to under the current settings.
pub fn recordings_dir(home: &Path, settings: &Settings) -> PathBuf {
    match &settings.recordings_dir {
        Some(dir) => PathBuf::from(dir),
        None => default_recordings_dir(home),
    }
}

/// What the settings screens get: the settings and warning, plus the folder recordings really
/// go to (the default expanded for this user), so the page can show and open it.
#[derive(Debug, Serialize)]
pub struct SettingsView {
    #[serde(flatten)]
    pub loaded: Loaded,
    pub recordings_path: String,
}

#[tauri::command]
pub fn get_settings(hotkey: tauri::State<'_, crate::hotkey::HotkeyState>) -> Result<SettingsView, String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let mut loaded = load(&config_path()?);
    // A shortcut that could not be registered (another app owns it) is worth telling the user about.
    if let Some(problem) = hotkey.error() {
        loaded.warning = Some(match loaded.warning {
            Some(w) => format!("{w} {problem}"),
            None => problem,
        });
    }
    let recordings_path = recordings_dir(&home, &loaded.settings).to_string_lossy().into_owned();
    Ok(SettingsView { loaded, recordings_path })
}

/// Make sure the folder exists, so it can be opened on a fresh install.
pub fn ensure_folder(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))
}

/// Open the recordings folder in Finder, creating it first if nothing has been recorded yet.
#[tauri::command]
pub fn open_recordings_folder() -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let dir = recordings_dir(&home, &load(&config_path()?).settings);
    ensure_folder(&dir)?;
    let status = std::process::Command::new("open").arg(&dir).status().map_err(|e| format!("could not run open: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Finder could not open {}", dir.display()))
    }
}

/// Open a web URL in the user's default browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err("invalid url".to_string());
    }
    let status = std::process::Command::new("open")
        .arg(&url)
        .status()
        .map_err(|e| format!("could not open url: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("could not open url: {url}"))
    }
}

/// Validate, switch the shortcut, then save. If saving fails the old shortcut is put back.
#[tauri::command]
pub fn save_settings(app: tauri::AppHandle, settings: Settings) -> Result<Settings, String> {
    let path = config_path()?;
    validate(&settings)?;
    let old = load(&path).settings.hotkey;
    if settings.hotkey != old {
        crate::hotkey::apply(&app, &settings.hotkey)?;
    }
    if let Err(e) = save(&path, &settings) {
        if settings.hotkey != old {
            let _ = crate::hotkey::apply(&app, &old);
        }
        return Err(e);
    }
    Ok(settings)
}

/// Ask for a recordings folder with the system chooser. `"cancelled"` if the user closes it.
#[tauri::command]
pub async fn choose_recordings_dir() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let out = std::process::Command::new("osascript")
            .args(["-e", r#"POSIX path of (choose folder with prompt "Where should Capturra save recordings?")"#])
            .output()
            .map_err(|e| format!("could not run osascript: {e}"))?;
        crate::pick_recording::parse_choose_folder(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
        .map(|p| p.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::temp_dir;

    #[test]
    fn capturra_stays_out_of_recordings_by_default_and_an_old_settings_file_keeps_that() {
        assert!(!Settings::default().show_capturra);
        let path = temp_dir("showcap").join(FILE);
        // a file saved before this setting existed has no such field
        std::fs::write(&path, r#"{ "version": 1, "fps": 30 }"#).unwrap();
        assert!(!load(&path).settings.show_capturra);
        let on = Settings { show_capturra: true, ..Default::default() };
        save(&path, &on).unwrap();
        assert!(load(&path).settings.show_capturra);
    }

    #[test]
    fn system_audio_is_off_by_default_and_is_remembered_and_an_old_file_keeps_it_off() {
        assert!(!Settings::default().system_audio);
        let path = temp_dir("sysaudio").join(FILE);
        std::fs::write(&path, r#"{ "version": 1, "fps": 30 }"#).unwrap();
        assert!(!load(&path).settings.system_audio);
        save(&path, &Settings { system_audio: true, ..Default::default() }).unwrap();
        assert!(load(&path).settings.system_audio);
    }

    #[test]
    fn the_microphone_is_off_by_default_and_is_remembered_apart_from_system_audio() {
        assert!(!Settings::default().microphone);
        let path = temp_dir("mic").join(FILE);
        std::fs::write(&path, r#"{ "version": 1, "system_audio": true }"#).unwrap();
        let old = load(&path).settings;
        assert!(old.system_audio && !old.microphone, "an older file without the field keeps the microphone off");
        save(&path, &Settings { microphone: true, ..Default::default() }).unwrap();
        let back = load(&path).settings;
        assert!(back.microphone && !back.system_audio);
    }

    #[test]
    fn missing_file_gives_defaults_without_a_warning() {
        let l = load(&temp_dir("set").join(FILE));
        assert_eq!(l, Loaded { settings: Settings::default(), warning: None });
        assert_eq!((l.settings.fps, l.settings.countdown), (30, 3));
    }

    #[test]
    fn settings_round_trip_and_fields_added_later_default() {
        let path = temp_dir("rt").join("sub").join(FILE);
        let s = Settings {
            recordings_dir: Some(temp_dir("rec").to_string_lossy().into_owned()),
            last_source: Some(LastSource { kind: "region".into(), display: Some(1), region: Some([10.0, 20.0, 800.0, 600.0]) }),
            fps: 60,
            countdown: 0,
            hotkey: String::new(),
            ..Default::default()
        };
        save(&path, &s).unwrap();
        assert_eq!(load(&path).settings, s);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, r#"{ "version": 1, "fps": 45 }"#).unwrap();
        let l = load(&path).settings;
        assert_eq!((l.fps, l.countdown, l.hotkey.as_str()), (45, 3, DEFAULT_HOTKEY));
    }

    #[test]
    fn a_corrupt_or_newer_file_is_kept_aside_and_reported() {
        let dir = temp_dir("bad");
        let path = dir.join(FILE);
        std::fs::write(&path, "{ nope").unwrap();
        let l = load(&path);
        assert_eq!(l.settings, Settings::default());
        assert!(l.warning.unwrap().contains("settings.json.bad"));
        assert!(!path.exists() && dir.join("settings.json.bad").is_file());
        std::fs::write(&path, r#"{ "version": 9 }"#).unwrap();
        assert!(load(&path).warning.unwrap().contains("newer"));
    }

    #[test]
    fn bad_values_are_refused_and_the_old_file_stays() {
        let path = temp_dir("val").join(FILE);
        save(&path, &Settings::default()).unwrap();
        let before = std::fs::read(&path).unwrap();
        for bad in [
            Settings { fps: 5, ..Default::default() },
            Settings { fps: 240, ..Default::default() },
            Settings { countdown: 60, ..Default::default() },
            Settings { recordings_dir: Some("relative/dir".into()), ..Default::default() },
            Settings { recordings_dir: Some("/tmp/../etc".into()), ..Default::default() },
            Settings { hotkey: "R".into(), ..Default::default() },
        ] {
            assert!(save(&path, &bad).is_err(), "{bad:?}");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn a_new_recordings_folder_is_created_and_must_be_writable() {
        let folder = temp_dir("mk").join("nested").join("Recordings");
        validate(&Settings { recordings_dir: Some(folder.to_string_lossy().into_owned()), ..Default::default() }).unwrap();
        assert!(folder.is_dir() && !folder.join(".capturra-write-test").exists());
        assert!(validate(&Settings { recordings_dir: Some("/System/capturra-no".into()), ..Default::default() }).is_err());
    }

    #[test]
    fn the_recordings_folder_is_created_so_it_can_be_opened_on_a_fresh_install() {
        let dir = temp_dir("open").join("Movies").join("Capturra");
        assert!(!dir.exists());
        ensure_folder(&dir).unwrap();
        assert!(dir.is_dir());
        ensure_folder(&dir).unwrap(); // already there is fine
        assert!(ensure_folder(Path::new("/System/capturra-no")).is_err());
    }

    #[test]
    fn recordings_go_to_movies_capturra_unless_changed() {
        let home = Path::new("/Users/me");
        assert_eq!(recordings_dir(home, &Settings::default()), Path::new("/Users/me/Movies/Capturra"));
        let s = Settings { recordings_dir: Some("/Volumes/Work/Demos".into()), ..Default::default() };
        assert_eq!(recordings_dir(home, &s), Path::new("/Volumes/Work/Demos"));
        assert_eq!(config_dir(home), Path::new("/Users/me/Library/Application Support/app.capturra.dev"));
    }
}

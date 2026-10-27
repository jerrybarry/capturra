//! Backgrounds for the Studio frame: the bundled wallpapers and the user's own pictures.
//!
//! Wallpapers are compiled in, so the exporter and the preview always agree on them. Custom
//! pictures are copied (as PNG, at most 3840 px on the long side) into
//! `<config>/backgrounds/`; projects refer to them by file name, never by path.

use capturra_render::Picture;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Bundled wallpapers: id and PNG bytes. The same files are served to the Studio from
/// `public/backgrounds/<id>.png`.
const WALLPAPERS: [(&str, &[u8]); 8] = [
    ("aurora", include_bytes!("../../public/backgrounds/aurora.png")),
    ("sunset", include_bytes!("../../public/backgrounds/sunset.png")),
    ("lagoon", include_bytes!("../../public/backgrounds/lagoon.png")),
    ("orchid", include_bytes!("../../public/backgrounds/orchid.png")),
    ("ember", include_bytes!("../../public/backgrounds/ember.png")),
    ("meadow", include_bytes!("../../public/backgrounds/meadow.png")),
    ("midnight", include_bytes!("../../public/backgrounds/midnight.png")),
    ("dune", include_bytes!("../../public/backgrounds/dune.png")),
];

pub fn custom_dir(home: &Path) -> PathBuf {
    crate::settings::config_dir(home).join("backgrounds")
}

/// A custom background id is a plain PNG file name: no folders, no `..`.
pub fn valid_id(id: &str) -> bool {
    id.len() <= 80
        && id.to_ascii_lowercase().ends_with(".png")
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn wallpaper(id: &str) -> Result<Picture, String> {
    let (_, bytes) = WALLPAPERS.iter().find(|(name, _)| *name == id).ok_or_else(|| format!("unknown wallpaper {id:?}"))?;
    Picture::from_png(bytes)
}

pub fn custom(dir: &Path, id: &str) -> Result<Picture, String> {
    if !valid_id(id) {
        return Err(format!("{id:?} is not a valid background name"));
    }
    let bytes = std::fs::read(dir.join(id))
        .map_err(|e| format!("the custom background {id} is missing ({e}); pick another one in the Frame tab"))?;
    Picture::from_png(&bytes)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CustomBackground {
    pub id: String,
    /// Absolute path, for the asset protocol (thumbnails).
    pub path: String,
}

pub fn list(dir: &Path) -> Vec<CustomBackground> {
    let mut items: Vec<(std::time::SystemTime, CustomBackground)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let id = e.file_name().to_str()?.to_string();
            valid_id(&id).then(|| {
                (
                    e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH),
                    CustomBackground { path: e.path().to_string_lossy().into_owned(), id },
                )
            })
        })
        .collect();
    items.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    items.into_iter().map(|(_, b)| b).collect()
}

/// Longest side kept for a custom picture, in pixels.
const MAX_SIDE: usize = 3840;

/// Copy `source` into `dir` as a PNG no larger than `MAX_SIDE` px, using macOS's own `sips` (it reads
/// HEIC, JPEG, TIFF and more). Returns the new id.
pub fn import(dir: &Path, source: &Path) -> Result<CustomBackground, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let stem: String = source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("background")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    let stem = if stem.trim_matches('-').is_empty() { "background".to_string() } else { stem };
    let id = (1..)
        .map(|n| if n == 1 { format!("{stem}.png") } else { format!("{stem}-{n}.png") })
        .find(|name| !dir.join(name).exists())
        .unwrap();
    let out = dir.join(&id);
    let result = Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(source)
        .arg("--out")
        .arg(&out)
        .output()
        .map_err(|e| format!("could not run sips: {e}"))?;
    if !result.status.success() || !out.exists() {
        let _ = std::fs::remove_file(&out);
        return Err(format!("that file is not a picture macOS can read ({})", String::from_utf8_lossy(&result.stderr).trim()));
    }
    // Fail now, not at export time, if what sips wrote cannot be decoded.
    let pic = custom(dir, &id).inspect_err(|_| {
        let _ = std::fs::remove_file(&out);
    })?;
    // Only ever shrink (`sips -Z` would also enlarge a small picture).
    if pic.w.max(pic.h) > MAX_SIDE {
        let shrunk = Command::new("sips").args(["-Z", &MAX_SIDE.to_string()]).arg(&out).output();
        if !shrunk.map(|o| o.status.success()).unwrap_or(false) {
            let _ = std::fs::remove_file(&out);
            return Err("could not scale the picture down".to_string());
        }
    }
    Ok(CustomBackground { path: out.to_string_lossy().into_owned(), id })
}

pub fn remove(dir: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err(format!("{id:?} is not a valid background name"));
    }
    match std::fs::remove_file(dir.join(id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("could not delete {id}: {e}")),
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| "HOME is not set".to_string())
}

pub fn current_custom_dir() -> Result<PathBuf, String> {
    Ok(custom_dir(&home()?))
}

/// Ask for a picture with the standard macOS file dialog (AppleScript), then import it.
/// `None` when the dialog is cancelled.
#[tauri::command]
pub async fn add_custom_background() -> Result<Option<CustomBackground>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let dir = current_custom_dir()?;
        let out = Command::new("osascript")
            .args(["-e", "POSIX path of (choose file of type {\"public.image\"} with prompt \"Choose a background picture\")"])
            .output()
            .map_err(|e| format!("could not open the file dialog: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            if err.contains("-128") {
                return Ok(None); // cancelled
            }
            return Err(format!("the file dialog failed: {}", err.trim()));
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        import(&dir, Path::new(&path)).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn list_custom_backgrounds() -> Result<Vec<CustomBackground>, String> {
    Ok(list(&current_custom_dir()?))
}

#[tauri::command]
pub fn delete_custom_background(id: String) -> Result<(), String> {
    remove(&current_custom_dir()?, &id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("capturra-bg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_bundled_wallpaper_decodes_at_the_expected_size() {
        for (id, _) in WALLPAPERS {
            let p = wallpaper(id).unwrap();
            assert_eq!((p.w, p.h, p.rgb.len()), (1600, 1000, 1600 * 1000 * 3), "{id}");
        }
        assert!(wallpaper("nope").is_err());
    }

    #[test]
    fn ids_cannot_leave_the_backgrounds_folder() {
        for bad in ["../x.png", "a/b.png", ".hidden.png", "x.jpg", "x.png/", "", "a b.png"] {
            assert!(!valid_id(bad), "{bad:?}");
        }
        assert!(valid_id("my-photo_2.PNG"));
        assert!(custom(Path::new("/nonexistent"), "../../etc/passwd").is_err());
        assert!(remove(Path::new("/nonexistent"), "../x.png").is_err());
    }

    #[test]
    fn importing_converts_to_png_names_uniquely_lists_newest_first_and_deletes() {
        let dir = temp("import");
        // a real JPEG made by sips from a bundled wallpaper
        let png = dir.join("src.png");
        std::fs::write(&png, WALLPAPERS[0].1).unwrap();
        let jpg = dir.join("My Photo (1).jpg");
        assert!(Command::new("sips").args(["-s", "format", "jpeg"]).arg(&png).arg("--out").arg(&jpg).output().unwrap().status.success());
        let store = dir.join("store");
        let a = import(&store, &jpg).unwrap();
        let b = import(&store, &jpg).unwrap();
        assert_eq!(a.id, "My-Photo--1-.png");
        assert_eq!(b.id, "My-Photo--1--2.png");
        assert_eq!(custom(&store, &a.id).unwrap().w, 1600, "a small picture is not enlarged");
        assert_eq!(list(&store).len(), 2);
        remove(&store, &a.id).unwrap();
        remove(&store, &a.id).unwrap(); // already gone is fine
        assert_eq!(list(&store).iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![b.id.clone()]);
        // a huge picture is scaled down to MAX_SIDE
        let big = dir.join("big.png");
        assert!(Command::new("sips").args(["-z", "2000", "5000"]).arg(&png).arg("--out").arg(&big).output().unwrap().status.success());
        let c = import(&store, &big).unwrap();
        let pic = custom(&store, &c.id).unwrap();
        assert_eq!((pic.w, pic.h), (MAX_SIDE, 1536));
        remove(&store, &c.id).unwrap();
        // not a picture
        let txt = dir.join("note.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(import(&store, &txt).is_err());
        assert_eq!(list(&store).len(), 1, "a failed import leaves nothing behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

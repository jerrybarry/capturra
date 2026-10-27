//! `source_preview`: a small picture of a display or a window for the recording sheet's cards.
//! Uses the system `screencapture` and `sips`, so it needs Screen Recording permission and no
//! extra dependency. It shows what is on screen right now.

use crate::region_picker::base64;
use std::path::Path;
use std::process::Command;

/// Widest side of a preview, in pixels. Cards are small; this keeps the picture light.
const MAX_SIDE: u32 = 480;

/// What to photograph.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    /// A rectangle of the desktop in global points.
    Rect { x: f64, y: f64, w: f64, h: f64 },
    /// One window, by its window number, without its shadow.
    Window(u32),
}

/// The `screencapture` arguments for `target`, writing to `out`.
pub fn capture_args(target: Target, out: &Path) -> Vec<String> {
    let mut args = vec!["-x".to_string()];
    match target {
        Target::Rect { x, y, w, h } => args.push(format!("-R{x},{y},{w},{h}")),
        Target::Window(id) => args.extend(["-o".to_string(), format!("-l{id}")]),
    }
    args.push(out.to_string_lossy().into_owned());
    args
}

/// Shrink a PNG to a JPEG no wider or taller than `MAX_SIDE` and return it as a data URL.
pub fn jpeg_data_url(png: &Path, work: &Path) -> Result<String, String> {
    let jpg = work.join("preview.jpg");
    let status = Command::new("sips")
        .args(["-s", "format", "jpeg", "-s", "formatOptions", "70", "-Z", &MAX_SIDE.to_string()])
        .arg(png)
        .arg("--out")
        .arg(&jpg)
        .output()
        .map_err(|e| format!("could not run sips: {e}"))?;
    if !status.status.success() || !jpg.exists() {
        return Err(format!("sips could not shrink the picture: {}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    let bytes = std::fs::read(&jpg).map_err(|e| format!("reading the preview: {e}"))?;
    Ok(format!("data:image/jpeg;base64,{}", base64(&bytes)))
}

fn preview_of(target: Target) -> Result<String, String> {
    let work = std::env::temp_dir().join(format!(
        "capturra-preview-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("t").len() + rand_suffix()
    ));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        let png = work.join("shot.png");
        let out = Command::new("screencapture")
            .args(capture_args(target, &png))
            .output()
            .map_err(|e| format!("could not run screencapture: {e}"))?;
        if !out.status.success() || !png.exists() {
            return Err("screencapture failed: Screen Recording permission may be missing, or the window is gone".to_string());
        }
        jpeg_data_url(&png, &work)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn rand_suffix() -> usize {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos() as usize).unwrap_or(0)
}

/// `kind` is "display" (`id` = display index) or "window" (`id` = window number).
#[tauri::command]
pub async fn source_preview(kind: String, id: u32) -> Result<String, String> {
    let target = match kind.as_str() {
        "display" => {
            let sources = capturra_recorder::source_list().map_err(|e| format!("{e:#}"))?;
            let d = sources.displays.iter().find(|d| d.index == id as usize).ok_or_else(|| format!("display {id} not found"))?;
            Target::Rect { x: d.x, y: d.y, w: d.width, h: d.height }
        }
        "window" => Target::Window(id),
        other => return Err(format!("unknown source kind {other:?}")),
    };
    tauri::async_runtime::spawn_blocking(move || preview_of(target)).await.map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::temp_dir;

    #[test]
    fn a_display_is_captured_by_rectangle_and_a_window_by_number_without_its_shadow() {
        let out = Path::new("/tmp/x.png");
        assert_eq!(capture_args(Target::Rect { x: 2048.0, y: 0.0, w: 2560.0, h: 1440.0 }, out), ["-x", "-R2048,0,2560,1440", "/tmp/x.png"]);
        assert_eq!(capture_args(Target::Rect { x: -1440.0, y: 100.5, w: 1440.0, h: 900.0 }, out)[1], "-R-1440,100.5,1440,900");
        assert_eq!(capture_args(Target::Window(4121), out), ["-x", "-o", "-l4121", "/tmp/x.png"]);
    }

    #[test]
    fn a_big_picture_becomes_a_small_jpeg_data_url() {
        let dir = temp_dir("preview");
        let png = dir.join("big.png");
        let made = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=2048x1280:rate=1", "-frames:v", "1"])
            .arg(&png)
            .status()
            .expect("ffmpeg is needed for this test (brew install ffmpeg)");
        assert!(made.success());
        let url = jpeg_data_url(&png, &dir).unwrap();
        let body = url.strip_prefix("data:image/jpeg;base64,").expect("a jpeg data url");
        assert!(body.len() < 150_000, "a preview stays light, got {} characters", body.len());
        // a real JPEG starts with FFD8, which is "/9j/" in base64
        assert!(body.starts_with("/9j/"), "{}", &body[..16]);
        // the longest side is limited to 480 px
        let size = Command::new("sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(dir.join("preview.jpg")).output().unwrap();
        let text = String::from_utf8_lossy(&size.stdout).to_string();
        assert!(text.contains("pixelWidth: 480") && text.contains("pixelHeight: 300"), "{text}");
    }

    #[test]
    fn a_missing_picture_is_an_error_not_an_empty_preview() {
        let dir = temp_dir("preview2");
        assert!(jpeg_data_url(&dir.join("nope.png"), &dir).is_err());
    }
}

//! `video_frames`: a few small pictures from a recording, for the editor's Clip track. Each one is a
//! separate `ffmpeg` call that seeks to the time and decodes a single frame, so a long recording
//! costs a handful of seeks, not a full decode. Done here, not in the webview, because seeking a
//! `<video>` element is only as reliable as the webview's decoder and the server behind it.

use crate::projects::{recordings_root, resolve_dir, VIDEO};
use crate::region_picker::base64;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Most pictures one call may ask for, and the allowed picture height in pixels.
pub const MAX_FRAMES: usize = 64;
pub const MIN_HEIGHT: u32 = 16;
pub const MAX_HEIGHT: u32 = 360;
/// How many `ffmpeg` runs go at once.
const PARALLEL: usize = 4;

/// The `ffmpeg` arguments that save one frame at `t_ms` of `video`, `height` pixels tall.
pub fn frame_args(video: &Path, t_ms: u64, height: u32, out: &Path) -> Vec<String> {
    let height = height.clamp(MIN_HEIGHT, MAX_HEIGHT);
    vec![
        "-y".into(),
        "-v".into(),
        "error".into(),
        "-ss".into(),
        format!("{:.3}", t_ms as f64 / 1000.0),
        "-i".into(),
        video.to_string_lossy().into_owned(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        format!("scale=-2:{height}"),
        "-q:v".into(),
        "5".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Save one JPEG per time into `work`, returning each path, or `None` for a time that could not be
/// reached (past the end of the video, for instance). Runs a few at a time.
pub fn frames_to_files(video: &Path, times_ms: &[u64], height: u32, work: &Path) -> Vec<Option<PathBuf>> {
    let mut out: Vec<Option<PathBuf>> = vec![None; times_ms.len()];
    for (batch_no, batch) in times_ms.chunks(PARALLEL).enumerate() {
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .iter()
                .enumerate()
                .map(|(k, &t)| {
                    let index = batch_no * PARALLEL + k;
                    let file = work.join(format!("frame-{index}.jpg"));
                    scope.spawn(move || {
                        let ran = Command::new("ffmpeg").args(frame_args(video, t, height, &file)).output();
                        match ran {
                            Ok(o) if o.status.success() && file.metadata().map(|m| m.len() > 0).unwrap_or(false) => Some(file),
                            _ => None,
                        }
                    })
                })
                .collect();
            for (k, h) in handles.into_iter().enumerate() {
                out[batch_no * PARALLEL + k] = h.join().ok().flatten();
            }
        });
    }
    out
}

fn data_url(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| format!("data:image/jpeg;base64,{}", base64(&bytes)))
}

/// Pictures at `times_ms` of project `id`, as JPEG data URLs (`None` where a frame was not available).
#[tauri::command]
pub async fn video_frames(id: String, times_ms: Vec<u64>, height: u32) -> Result<Vec<Option<String>>, String> {
    if times_ms.len() > MAX_FRAMES {
        return Err(format!("at most {MAX_FRAMES} frames per request, not {}", times_ms.len()));
    }
    let root = recordings_root()?;
    let video = resolve_dir(&root, &id)?.join(VIDEO);
    tauri::async_runtime::spawn_blocking(move || {
        let work = std::env::temp_dir().join(format!(
            "capturra-frames-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let files = frames_to_files(&video, &times_ms, height, &work);
        let urls = files.iter().map(|f| f.as_deref().and_then(data_url)).collect();
        let _ = std::fs::remove_dir_all(&work);
        Ok(urls)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::temp_dir;

    /// Average colour of an image, via ffmpeg (scaled to one pixel).
    fn average_rgb(image: &Path) -> [u8; 3] {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(image)
            .args(["-vf", "scale=1:1", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
            .output()
            .unwrap();
        [out.stdout[0], out.stdout[1], out.stdout[2]]
    }

    #[test]
    fn the_arguments_seek_before_opening_and_limit_the_height() {
        let a = frame_args(Path::new("/v/recording.mp4"), 12_345, 90, Path::new("/t/f.jpg"));
        let seek = a.iter().position(|x| x == "-ss").unwrap();
        let input = a.iter().position(|x| x == "-i").unwrap();
        assert!(seek < input, "-ss before -i is a fast seek");
        assert_eq!(a[seek + 1], "12.345");
        assert!(a.contains(&"scale=-2:90".to_string()));
        assert!(frame_args(Path::new("v"), 0, 5, Path::new("o")).contains(&"scale=-2:16".to_string()));
        assert!(frame_args(Path::new("v"), 0, 5000, Path::new("o")).contains(&"scale=-2:360".to_string()));
    }

    #[test]
    fn each_time_gets_its_own_frame_of_the_clip() {
        let dir = temp_dir("frames");
        let clip = dir.join("clip.mp4");
        // two seconds each of red, green and blue
        let made = Command::new("ffmpeg")
            .args([
                "-y",
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=320x200:r=30:d=2",
                "-f",
                "lavfi",
                "-i",
                "color=c=green:s=320x200:r=30:d=2",
            ])
            .args([
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=320x200:r=30:d=2",
                "-filter_complex",
                "[0][1][2]concat=n=3:v=1:a=0",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status()
            .expect("ffmpeg is needed for this test (brew install ffmpeg)");
        assert!(made.success());
        let files = frames_to_files(&clip, &[1000, 3000, 5000, 60_000], 100, &dir);
        let got: Vec<[u8; 3]> = files[..3].iter().map(|f| average_rgb(f.as_ref().expect("a frame"))).collect();
        assert!(got[0][0] > 200 && got[0][1] < 60 && got[0][2] < 60, "red: {:?}", got[0]);
        assert!(got[1][1] > 100 && got[1][0] < 60 && got[1][2] < 60, "green: {:?}", got[1]);
        assert!(got[2][2] > 200 && got[2][0] < 60 && got[2][1] < 60, "blue: {:?}", got[2]);
        assert!(files[3].is_none(), "a time past the end of the video has no frame");
        // 100 px tall, width follows the shape (320x200 -> 160x100)
        let size = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "csv=p=0"])
            .arg(files[0].as_ref().unwrap())
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&size.stdout).trim(), "160,100");
        assert!(data_url(files[0].as_ref().unwrap()).unwrap().starts_with("data:image/jpeg;base64,/9j/"));
    }

    #[test]
    fn a_missing_video_gives_no_frames_not_an_error() {
        let dir = temp_dir("frames2");
        assert_eq!(frames_to_files(&dir.join("nope.mp4"), &[0, 500], 90, &dir), vec![None, None]);
        assert!(frames_to_files(&dir.join("nope.mp4"), &[], 90, &dir).is_empty());
    }
}

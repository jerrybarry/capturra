//! `audio_waveform`: the outline of a recording's sound for the editor's Audio track. ffmpeg decodes the
//! audio to 8 kHz mono (enough for an outline, and 32 KB per second of recording), and each slice of the
//! timeline gets its loudest sample.

use crate::projects::{recordings_root, resolve_dir, VIDEO};
use std::path::Path;
use std::process::Command;

/// Most bars one call may ask for.
pub const MAX_BARS: usize = 8000;
const RATE: u32 = 8000;

/// Whether the file has a sound track at all.
pub fn has_audio(video: &Path) -> bool {
    Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a:0", "-show_entries", "stream=codec_type", "-of", "csv=p=0"])
        .arg(video)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "audio")
        .unwrap_or(false)
}

/// `bars` loudness values from 0 to 1, evenly spread over `samples`: each is the loudest sample of its
/// slice, scaled so the loudest slice is 1 (a quiet recording still shows its shape) and eased with a
/// square root (speech would otherwise look like a few spikes).
pub fn peaks_from_samples(samples: &[f32], bars: usize) -> Vec<f32> {
    let bars = bars.clamp(1, MAX_BARS);
    let mut peaks = vec![0f32; bars];
    if samples.is_empty() {
        return peaks;
    }
    for (b, peak) in peaks.iter_mut().enumerate() {
        let from = b * samples.len() / bars;
        let to = ((b + 1) * samples.len() / bars).max(from + 1).min(samples.len());
        *peak = samples[from..to].iter().fold(0f32, |m, s| m.max(s.abs()));
    }
    let loudest = peaks.iter().fold(0f32, |m, p| m.max(*p));
    if loudest > 1e-4 {
        for p in &mut peaks {
            *p = (*p / loudest).sqrt();
        }
    } else {
        peaks.fill(0.0);
    }
    peaks
}

/// The waveform of `video`, or `None` when it has no sound or ffmpeg could not read it.
pub fn audio_peaks(video: &Path, bars: usize) -> Option<Vec<f32>> {
    if !has_audio(video) {
        return None;
    }
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-vn", "-ac", "1", "-ar", &RATE.to_string(), "-f", "f32le", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let samples: Vec<f32> = out.stdout.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    Some(peaks_from_samples(&samples, bars))
}

/// The outline of project `id`'s sound, or `None` if the recording has none.
#[tauri::command]
pub async fn audio_waveform(id: String, bars: usize) -> Result<Option<Vec<f32>>, String> {
    let root = recordings_root()?;
    let video = resolve_dir(&root, &id)?.join(VIDEO);
    tauri::async_runtime::spawn_blocking(move || audio_peaks(&video, bars)).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::temp_dir;

    #[test]
    fn each_slice_gets_its_loudest_sample_and_the_loudest_slice_is_one() {
        let mut s = vec![0.0f32; 400];
        s[50] = -0.1; // slice 0 of 4
        s[250] = 0.4; // slice 2
        let p = peaks_from_samples(&s, 4);
        assert_eq!(p.len(), 4);
        assert!((p[2] - 1.0).abs() < 1e-6, "{p:?}");
        assert!((p[0] - (0.25f32).sqrt()).abs() < 1e-6, "0.1 / 0.4 = 0.25, eased: {p:?}");
        assert_eq!((p[1], p[3]), (0.0, 0.0));
    }

    #[test]
    fn silence_and_empty_input_give_a_flat_line_and_bar_counts_are_limited() {
        assert_eq!(peaks_from_samples(&[0.0; 100], 5), vec![0.0; 5]);
        assert_eq!(peaks_from_samples(&[], 3), vec![0.0; 3]);
        assert_eq!(peaks_from_samples(&[0.5; 10], 0).len(), 1);
        assert_eq!(peaks_from_samples(&[0.5; 10], 1_000_000).len(), MAX_BARS);
    }

    #[test]
    fn a_real_clip_shows_sound_then_silence_and_a_clip_without_sound_has_no_waveform() {
        let dir = temp_dir("wave");
        let (with, without) = (dir.join("with.mp4"), dir.join("without.mp4"));
        // 2 s of a tone, then 2 s of silence
        let made = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=160x90:r=30:d=4"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono:d=2"])
            .args(["-filter_complex", "[1][2]concat=n=2:v=0:a=1[a]", "-map", "0:v", "-map", "[a]", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&with)
            .status()
            .expect("ffmpeg is needed for this test (brew install ffmpeg)");
        assert!(made.success());
        let p = audio_peaks(&with, 40).expect("this clip has sound");
        assert_eq!(p.len(), 40);
        assert!(p[..15].iter().all(|v| *v > 0.5), "the tone: {p:?}");
        assert!(p[25..].iter().all(|v| *v < 0.2), "the silence: {p:?}");
        let silent = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:s=160x90:r=30:d=1", "-pix_fmt", "yuv420p"])
            .arg(&without)
            .status()
            .unwrap();
        assert!(silent.success());
        assert_eq!(audio_peaks(&without, 40), None);
        assert_eq!(audio_peaks(&dir.join("missing.mp4"), 40), None);
    }
}

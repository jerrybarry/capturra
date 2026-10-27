//! Editing a recording's sound: the Audio tab's settings, the ffmpeg filter chain they become, and the
//! processed preview the editor plays. One chain feeds both the preview and the export, so what you
//! hear while editing is what you export. The recording itself is never changed.

use crate::projects::{recordings_root, resolve_dir, VIDEO};
use crate::waveform::has_audio;
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

/// What the Audio tab stores in `project.json` under `settings.audio`. Missing values use the defaults.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Clean up a voice: cut rumble, reduce background noise, even out the level, add a little clarity.
    pub enhance: bool,
    /// How much background noise to remove when `enhance` is on, 0 to 1.
    pub noise: f64,
    /// Gain in dB.
    pub volume: f64,
    /// Bring the whole sound to a standard loudness (-16 LUFS).
    pub normalize: bool,
    /// Fade lengths in seconds.
    pub fade_in: f64,
    pub fade_out: f64,
    /// Leave the sound out of exports.
    pub mute: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self { enhance: false, noise: 0.5, volume: 0.0, normalize: false, fade_in: 0.0, fade_out: 0.0, mute: false }
    }
}

impl AudioSettings {
    /// Read `settings.audio` out of a project's settings; anything missing or of the wrong type is the default.
    pub fn from_settings(settings: &Option<Value>) -> Self {
        settings.as_ref().and_then(|s| s.get("audio")).map(Self::from_value).unwrap_or_default()
    }

    pub fn from_value(v: &Value) -> Self {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    /// The ffmpeg `-af` chain, or `None` when the sound is left as recorded. Fades are placed between
    /// `from_s` and `to_s` on the stream the chain runs on: `0` and the length for an export (cut to the
    /// trim first), the trim's own start and end for a preview of the whole recording.
    ///
    /// `floor_db` is the recording's own background noise level (see `noise_floor_db`); the noise filter
    /// is told where the noise sits, and the Noise reduction setting says how much of it to remove.
    pub fn filter(&self, from_s: f64, to_s: f64, floor_db: Option<f64>) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        let volume = self.volume.clamp(-30.0, 24.0);
        if self.enhance {
            // 6 to 30 dB of noise reduction, aimed at the noise level measured in this recording
            let nr = 6.0 + self.noise.clamp(0.0, 1.0) * 24.0;
            let nf = floor_db.map_or(DEFAULT_FLOOR_DB, |f| f + 3.0).clamp(-80.0, -20.0);
            parts.push("highpass=f=80".into());
            parts.push(format!("afftdn=nr={nr:.1}:nf={nf:.1}"));
            parts.push("equalizer=f=250:t=q:w=1:g=-2".into());
            parts.push("equalizer=f=3500:t=q:w=1:g=2.5".into());
            parts.push("acompressor=threshold=0.1:ratio=2.5:attack=20:release=250:makeup=1.4".into());
        }
        if volume.abs() > 0.05 {
            parts.push(format!("volume={volume:.1}dB"));
        }
        if self.normalize {
            parts.push("loudnorm=I=-16:TP=-1.5:LRA=11".into());
        }
        let fade_in = self.fade_in.clamp(0.0, 10.0);
        let fade_out = self.fade_out.clamp(0.0, 10.0);
        let span = (to_s - from_s).max(0.0);
        if fade_in > 0.05 {
            parts.push(format!("afade=t=in:st={from_s:.3}:d={:.3}", fade_in.min(span)));
        }
        if fade_out > 0.05 {
            let d = fade_out.min(span);
            parts.push(format!("afade=t=out:st={:.3}:d={d:.3}", (to_s - d).max(from_s)));
        }
        if parts.is_empty() {
            return None;
        }
        // gain and compression can push peaks over full scale: a limiter keeps them from clipping
        if self.enhance || volume > 0.05 || self.normalize {
            parts.push("alimiter=limit=0.95".into());
        }
        Some(parts.join(","))
    }
}

/// Used when the noise level of a recording could not be measured.
const DEFAULT_FLOOR_DB: f64 = -45.0;
/// The measured floor never goes above this: a recording with no quiet moment would otherwise be
/// "denoised" into mush.
const MAX_FLOOR_DB: f64 = -35.0;

/// The background noise level (dB, RMS) of `samples`: the 10th percentile of the loudness of 50 ms
/// windows, i.e. what the quietest moments sound like.
pub fn floor_from_samples(samples: &[f32], rate: u32) -> Option<f64> {
    let window = (rate as usize / 20).max(1);
    let mut levels: Vec<f64> = samples
        .chunks_exact(window)
        .map(|w| {
            let rms = (w.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / w.len() as f64).sqrt();
            20.0 * rms.max(1e-6).log10()
        })
        .collect();
    if levels.len() < 10 {
        return None;
    }
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Some(levels[levels.len() / 10].min(MAX_FLOOR_DB))
}

/// `floor_from_samples` for a recording: its sound decoded to 16 kHz mono.
pub fn noise_floor_db(video: &std::path::Path) -> Option<f64> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let samples: Vec<f32> = out.stdout.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    floor_from_samples(&samples, 16_000)
}

/// Where processed previews go (inside the asset scope of `tauri.conf.json`).
fn preview_dir() -> Result<PathBuf, String> {
    let dir = crate::settings::config_path()?.parent().ok_or("no settings folder")?.join("audio-preview");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    Ok(dir)
}

fn file_stem(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// The recording's sound with the Audio tab's changes applied, as a file the editor plays alongside the
/// muted picture. `None` when nothing needs processing (the player then uses the recording's own sound),
/// when the sound is switched off, or when the recording has none.
#[tauri::command]
pub async fn audio_preview(id: String, audio: Value, trim_in_ms: u64, trim_out_ms: u64) -> Result<Option<String>, String> {
    let root = recordings_root()?;
    let video = resolve_dir(&root, &id)?.join(VIDEO);
    let settings = AudioSettings::from_value(&audio);
    let dir = preview_dir()?;
    tauri::async_runtime::spawn_blocking(move || {
        let stem = file_stem(&id);
        // earlier previews of this recording are out of date now
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(&format!("{stem}-")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        if settings.mute || !has_audio(&video) {
            return Ok(None);
        }
        let floor = if settings.enhance { noise_floor_db(&video) } else { None };
        let Some(chain) = settings.filter(trim_in_ms as f64 / 1000.0, trim_out_ms as f64 / 1000.0, floor) else { return Ok(None) };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let out = dir.join(format!("{stem}-{stamp}.m4a"));
        let ran = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-i"])
            .arg(&video)
            .args(["-vn", "-af", &chain, "-c:a", "aac", "-b:a", "160k"])
            .arg(&out)
            .output()
            .map_err(|e| format!("could not run ffmpeg: {e}"))?;
        if !ran.status.success() {
            let _ = std::fs::remove_file(&out);
            return Err(format!("ffmpeg could not process the audio: {}", String::from_utf8_lossy(&ran.stderr).trim()));
        }
        Ok(Some(out.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::testutil::temp_dir;
    use serde_json::json;

    #[test]
    fn missing_or_wrong_settings_are_the_defaults_and_the_defaults_change_nothing() {
        assert_eq!(AudioSettings::from_settings(&None), AudioSettings::default());
        assert_eq!(AudioSettings::from_settings(&Some(json!({ "audio": "loud" }))), AudioSettings::default());
        assert_eq!(AudioSettings::from_settings(&Some(json!({ "zoom": {} }))).filter(0.0, 10.0, None), None);
        let s = AudioSettings::from_settings(&Some(json!({ "audio": { "enhance": true, "volume": 3.0, "unknown": 1 } })));
        assert!(s.enhance && s.volume == 3.0 && s.noise == 0.5, "{s:?}");
    }

    #[test]
    fn each_setting_adds_its_filter_and_a_limiter_guards_against_clipping() {
        let off = AudioSettings::default();
        assert_eq!(off.filter(0.0, 10.0, None), None);
        let enhance = AudioSettings { enhance: true, noise: 1.0, ..off.clone() }.filter(0.0, 10.0, None).unwrap();
        assert!(enhance.starts_with("highpass=f=80,afftdn=nr=30.0:nf=-45.0,"), "{enhance}");
        let aimed = AudioSettings { enhance: true, ..off.clone() }.filter(0.0, 10.0, Some(-52.0)).unwrap();
        assert!(aimed.contains("afftdn=nr=18.0:nf=-49.0"), "the filter is aimed 3 dB above the measured floor: {aimed}");
        assert!(enhance.ends_with("alimiter=limit=0.95"), "{enhance}");
        assert!(AudioSettings { enhance: true, noise: 0.0, ..off.clone() }.filter(0.0, 10.0, None).unwrap().contains("afftdn=nr=6.0:"));
        assert_eq!(
            AudioSettings { volume: -6.0, ..off.clone() }.filter(0.0, 10.0, None).unwrap(),
            "volume=-6.0dB",
            "turning down needs no limiter"
        );
        assert_eq!(AudioSettings { volume: 6.0, ..off.clone() }.filter(0.0, 10.0, None).unwrap(), "volume=6.0dB,alimiter=limit=0.95");
        assert!(AudioSettings { normalize: true, ..off.clone() }.filter(0.0, 10.0, None).unwrap().starts_with("loudnorm=I=-16"));
        // gain is applied before the loudness step, the limiter is last
        let all = AudioSettings { enhance: true, volume: 3.0, normalize: true, fade_in: 1.0, fade_out: 2.0, ..off }
            .filter(0.0, 20.0, None)
            .unwrap();
        let at = |k: &str| all.find(k).unwrap_or_else(|| panic!("{k} in {all}"));
        assert!(
            at("afftdn") < at("volume=")
                && at("volume=") < at("loudnorm")
                && at("loudnorm") < at("afade=t=in")
                && at("afade=t=out") < at("alimiter"),
            "{all}"
        );
    }

    #[test]
    fn fades_sit_at_the_edges_of_the_range_and_never_outlast_it() {
        let s = AudioSettings { fade_in: 1.5, fade_out: 2.0, ..Default::default() };
        let export = s.filter(0.0, 30.0, None).unwrap();
        assert_eq!(export, "afade=t=in:st=0.000:d=1.500,afade=t=out:st=28.000:d=2.000");
        // a preview of the whole recording fades inside the trim (4 s to 14 s)
        assert_eq!(s.filter(4.0, 14.0, None).unwrap(), "afade=t=in:st=4.000:d=1.500,afade=t=out:st=12.000:d=2.000");
        // a 1 s clip cannot hold a 2 s fade
        assert_eq!(AudioSettings { fade_out: 2.0, ..Default::default() }.filter(0.0, 1.0, None).unwrap(), "afade=t=out:st=0.000:d=1.000");
        // out-of-range numbers are clamped
        assert!(AudioSettings { volume: 500.0, ..Default::default() }.filter(0.0, 5.0, None).unwrap().starts_with("volume=24.0dB"));
    }

    fn run(input_args: &[&str], chain: Option<&str>, out: &std::path::Path) {
        let mut c = Command::new("ffmpeg");
        c.args(["-y", "-v", "error"]).args(input_args);
        if let Some(chain) = chain {
            c.args(["-af", chain]);
        }
        let o = c.arg(out).output().expect("ffmpeg is needed for this test (brew install ffmpeg)");
        assert!(o.status.success(), "ffmpeg refused the chain {chain:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn mean_db(file: &std::path::Path, from: f64, secs: f64) -> f64 {
        let o = Command::new("ffmpeg")
            .args(["-nostats", "-ss", &from.to_string(), "-t", &secs.to_string(), "-i"])
            .arg(file)
            .args(["-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&o.stderr).to_string();
        log.lines()
            .find_map(|l| l.split("mean_volume: ").nth(1).map(|r| r.split_whitespace().next().unwrap().parse().unwrap()))
            .unwrap_or_else(|| panic!("no mean_volume:\n{log}"))
    }

    /// The real ffmpeg accepts every chain the settings can make, and the settings do what they say.
    #[test]
    fn the_chains_run_in_ffmpeg_and_change_the_sound_as_asked() {
        let dir = temp_dir("audioedit");
        let src = dir.join("voice.wav");
        // a quiet 440 Hz tone under white noise, 4 s
        run(
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=4:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=d=4:c=white:a=0.02:r=48000",
                "-filter_complex",
                "[0]volume=0.1[t];[t][1]amix=inputs=2:normalize=0",
            ],
            None,
            &src,
        );
        let input = |p: &std::path::Path| vec!["-i".to_string(), p.to_string_lossy().into_owned()];
        let process = |s: &AudioSettings, name: &str| -> PathBuf {
            let out = dir.join(name);
            let a = input(&src);
            let args: Vec<&str> = a.iter().map(String::as_str).collect();
            run(&args, s.filter(0.0, 4.0, Some(-39.0)).as_deref(), &out);
            out
        };
        let before = mean_db(&src, 1.0, 1.0);
        // every setting at once is a valid chain
        let everything =
            AudioSettings { enhance: true, noise: 0.8, volume: 4.0, normalize: true, fade_in: 0.5, fade_out: 0.5, mute: false };
        assert!(mean_db(&process(&everything, "all.m4a"), 1.0, 1.0).is_finite());
        // +6 dB is +6 dB
        let louder = mean_db(&process(&AudioSettings { volume: 6.0, ..Default::default() }, "vol.m4a"), 1.0, 1.0);
        assert!((louder - before - 6.0).abs() < 1.0, "{before} dB then {louder} dB");
        // loudness normalising raises a quiet recording to a standard level
        let normal = mean_db(&process(&AudioSettings { normalize: true, ..Default::default() }, "norm.m4a"), 1.0, 2.0);
        assert!(normal > before + 6.0, "{before} dB then {normal} dB");
        // a fade-in starts silent and is full a second later
        let faded = process(&AudioSettings { fade_in: 1.0, ..Default::default() }, "fade.m4a");
        assert!(mean_db(&faded, 0.0, 0.2) < mean_db(&faded, 2.0, 0.5) - 10.0);
        // Noise reduction: a voice-like tone in bursts over steady hiss. The hiss between the bursts must
        // get much quieter, the tone must stay, and the floor measured from the recording is what aims it.
        let bursts = dir.join("bursts.wav");
        run(
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=300:duration=8:sample_rate=48000,volume='if(lt(mod(t,2),1),0.3,0)':eval=frame",
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=d=8:c=white:a=0.02:r=48000",
                "-filter_complex",
                "[0][1]amix=inputs=2:normalize=0",
            ],
            None,
            &bursts,
        );
        let raw: Vec<f32> = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&bursts)
            .args(["-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
            .output()
            .unwrap()
            .stdout
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let floor = floor_from_samples(&raw, 16_000).expect("a measurable floor");
        assert!((-45.0..=-35.0).contains(&floor), "white noise at amplitude 0.02 sits near -39 dB, measured {floor}");
        let a = input(&bursts);
        let args: Vec<&str> = a.iter().map(String::as_str).collect();
        let clean = dir.join("bursts-clean.m4a");
        run(&args, AudioSettings { enhance: true, noise: 1.0, ..Default::default() }.filter(0.0, 8.0, Some(floor)).as_deref(), &clean);
        let (hiss_before, hiss_after) = (mean_db(&bursts, 5.2, 0.6), mean_db(&clean, 5.2, 0.6));
        let (tone_before, tone_after) = (mean_db(&bursts, 4.2, 0.6), mean_db(&clean, 4.2, 0.6));
        // how far the voice stands above the hiss: the clean-up must widen it a lot, and not by turning the voice down
        let (gap_before, gap_after) = (tone_before - hiss_before, tone_after - hiss_after);
        assert!(
            gap_after > gap_before + 8.0,
            "voice over hiss: {gap_before} dB then {gap_after} dB (hiss {hiss_before} -> {hiss_after}, tone {tone_before} -> {tone_after})"
        );
        assert!(tone_after > tone_before - 3.0, "the tone must survive: {tone_before} dB then {tone_after} dB");
    }

    #[test]
    fn the_noise_floor_is_the_quiet_moments_and_never_louder_than_the_cap() {
        let rate = 16_000;
        // 5 s: loud (0.3) for 40% of the windows, quiet (0.003 = -50 dB) for the rest
        let mut s = Vec::new();
        for w in 0..100 {
            s.extend(std::iter::repeat_n(if w % 5 < 2 { 0.3f32 } else { 0.003 }, rate / 20));
        }
        let f = floor_from_samples(&s, rate as u32).unwrap();
        assert!((f + 50.0).abs() < 1.0, "{f}");
        // nothing but loud sound: capped, not trusted
        assert_eq!(floor_from_samples(&vec![0.5f32; rate * 2], rate as u32), Some(MAX_FLOOR_DB));
        assert_eq!(floor_from_samples(&[0.1; 100], rate as u32), None, "too short to measure");
    }

    #[test]
    fn preview_file_names_cannot_leave_their_folder() {
        assert_eq!(file_stem("2026-10-27 19.46.11"), "2026-10-27_19_46_11");
        assert_eq!(file_stem("../../etc/passwd"), "______etc_passwd");
    }
}

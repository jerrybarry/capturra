//! System audio on the shared clock. Platform-independent, so the placement and muxing are tested
//! without a Mac.
//!
//! ScreenCaptureKit hands over audio in small chunks, each stamped with the presentation time of its
//! first sample. `AudioTrack` writes them to a raw stereo f32 file so that sample N sits at
//! N / rate seconds on the recording clock: a gap in the chunks becomes silence and an overlap is cut,
//! but jitter under `GAP_TOLERANCE_MS` is left alone so the samples stay contiguous and the track does
//! not drift against the video. At the end `mux` cuts the track at the first video frame and adds it to
//! the finished video without re-encoding the picture.

use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

/// What system audio is captured as (`scap`'s patched macOS engine asks for the same).
pub const SAMPLE_RATE: u32 = 48_000;

/// Chunk-start jitter smaller than this is ignored; anything larger is a real gap or overlap.
pub const GAP_TOLERANCE_MS: u64 = 20;

/// What happened to the audio while it was placed on the clock.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AudioStats {
    pub chunks: u64,
    /// Sample frames (one per channel pair) in the file.
    pub frames: u64,
    /// Gaps in the stream that were filled with silence, and the longest one.
    pub gaps: u64,
    pub silence_ms: u64,
    pub largest_gap_ms: u64,
    /// Audio that overlapped what was already written (or came from before the clock started) and was cut.
    pub overlap_cut_ms: u64,
}

/// Raw interleaved stereo f32 little-endian samples on the recording clock.
pub struct AudioTrack {
    out: BufWriter<File>,
    rate: u32,
    /// Sample frames written so far.
    written: u64,
    stats: AudioStats,
}

impl AudioTrack {
    pub fn create(path: &Path, rate: u32) -> Result<Self> {
        let file = File::create(path).with_context(|| format!("could not create {}", path.display()))?;
        Ok(Self { out: BufWriter::with_capacity(1 << 20, file), rate, written: 0, stats: AudioStats::default() })
    }

    fn ms(&self, frames: u64) -> u64 {
        frames * 1000 / self.rate as u64
    }

    fn silence(&mut self, frames: u64) -> Result<()> {
        const BLOCK: usize = 4096;
        let zeros = [0u8; BLOCK * 8];
        let mut left = frames;
        while left > 0 {
            let n = left.min(BLOCK as u64) as usize;
            self.out.write_all(&zeros[..n * 8])?;
            left -= n as u64;
        }
        self.written += frames;
        Ok(())
    }

    /// Place one chunk. `channels` is planar (one `Vec` per channel; a mono chunk is played on both
    /// sides, extra channels are ignored); `start_ns` is the time of its first sample on the
    /// recording clock and may be negative.
    pub fn push(&mut self, start_ns: i64, channels: &[Vec<f32>]) -> Result<()> {
        let Some(left) = channels.first() else { return Ok(()) };
        let right = channels.get(1).unwrap_or(left);
        let frames = left.len().min(right.len());
        if frames == 0 {
            return Ok(());
        }
        let start = (start_ns as i128 * self.rate as i128 / 1_000_000_000) as i64;
        let diff = start - self.written as i64;
        let tolerance = (self.rate as u64 * GAP_TOLERANCE_MS / 1000) as i64;
        let first = self.stats.chunks == 0;
        self.stats.chunks += 1;
        let mut skip = 0usize;
        if first {
            // File time zero is clock zero: lead in with silence, or drop what came before the clock.
            if diff > 0 {
                self.silence(diff as u64)?;
            } else {
                skip = (-diff) as usize;
            }
        } else if diff > tolerance {
            let gap_ms = self.ms(diff as u64);
            self.silence(diff as u64)?;
            self.stats.gaps += 1;
            self.stats.silence_ms += gap_ms;
            self.stats.largest_gap_ms = self.stats.largest_gap_ms.max(gap_ms);
        } else if diff < -tolerance {
            skip = (-diff) as usize;
        }
        if skip > 0 {
            self.stats.overlap_cut_ms += self.ms(skip.min(frames) as u64);
        }
        if skip >= frames {
            return Ok(());
        }
        let mut buf = Vec::with_capacity((frames - skip) * 8);
        for i in skip..frames {
            buf.extend_from_slice(&left[i].to_le_bytes());
            buf.extend_from_slice(&right[i].to_le_bytes());
        }
        self.out.write_all(&buf)?;
        self.written += (frames - skip) as u64;
        Ok(())
    }

    /// Pad with silence up to `end_ms` on the clock and flush.
    pub fn finish(mut self, end_ms: u64) -> Result<AudioStats> {
        let target = end_ms * self.rate as u64 / 1000;
        if self.written < target {
            self.silence(target - self.written)?;
        }
        self.out.flush()?;
        self.stats.frames = self.written;
        Ok(self.stats)
    }
}

enum Msg {
    Chunk { start_ns: i64, channels: Vec<Vec<f32>> },
    Finish,
}

/// Hands chunks from the capture thread to an `AudioTrack` on its own thread, so a slow disk can
/// never hold up the video frames.
#[derive(Clone)]
pub struct AudioSender(Sender<Msg>);

impl AudioSender {
    pub fn send(&self, start_ns: i64, channels: Vec<Vec<f32>>) {
        let _ = self.0.send(Msg::Chunk { start_ns, channels });
    }
}

pub struct AudioRecorder {
    tx: Sender<Msg>,
    handle: JoinHandle<Result<AudioTrack>>,
    path: PathBuf,
}

impl AudioRecorder {
    pub fn start(path: &Path, rate: u32) -> Result<(Self, AudioSender)> {
        let mut track = AudioTrack::create(path, rate)?;
        let (tx, rx) = mpsc::channel::<Msg>();
        let handle = std::thread::Builder::new().name("capturra-audio".into()).spawn(move || {
            while let Ok(Msg::Chunk { start_ns, channels }) = rx.recv() {
                track.push(start_ns, &channels)?;
            }
            Ok(track)
        })?;
        Ok((Self { tx: tx.clone(), handle, path: path.to_path_buf() }, AudioSender(tx)))
    }

    /// Stop the writer thread (chunks sent later are ignored) and pad the track to `end_ms`.
    pub fn finish(self, end_ms: u64) -> Result<(PathBuf, AudioStats)> {
        let _ = self.tx.send(Msg::Finish);
        let track = self.handle.join().map_err(|_| anyhow!("the audio thread panicked"))??;
        let stats = track.finish(end_ms)?;
        Ok((self.path, stats))
    }
}

/// One raw track for `mux`: stereo f32le samples recorded at `rate` (what the device was set to).
pub struct AudioInput<'a> {
    pub path: &'a Path,
    pub rate: u32,
    /// `real rate / rate` when the device clock was measured to run off nominal (see `mic::drift_tempo`),
    /// else 1.0. The track is sped up or slowed by this factor so it stays on the video's clock.
    pub tempo: f64,
}

/// Add the raw tracks to `video` (replacing the file). Each is cut at `video_origin_ms` so its time zero
/// meets the first video frame; several (system audio + microphone) are mixed into one AAC track, padded
/// or cut to the video's length. The picture is copied, not re-encoded. On failure the video is left
/// as it was.
pub fn mux(video: &Path, inputs: &[AudioInput], video_origin_ms: u64) -> Result<()> {
    if inputs.is_empty() {
        bail!("there is no audio to add");
    }
    let tmp = video.with_extension("with-audio.mp4");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-i"]).arg(video);
    for input in inputs {
        cmd.args(["-f", "f32le", "-ar", &input.rate.to_string(), "-ac", "2", "-ss"])
            .arg(format!("{:.3}", video_origin_ms as f64 / 1000.0))
            .arg("-i")
            .arg(input.path);
    }
    let mut chains: Vec<String> = Vec::new();
    for (i, input) in inputs.iter().enumerate() {
        let tempo = if (input.tempo - 1.0).abs() > 1e-9 { format!(",atempo={:.8}", input.tempo) } else { String::new() };
        chains.push(format!("[{}:a]aresample={SAMPLE_RATE}{tempo}[a{i}]", i + 1));
    }
    let mix = if inputs.len() == 1 {
        "[a0]apad[a]".to_string()
    } else {
        let labels: String = (0..inputs.len()).map(|i| format!("[a{i}]")).collect();
        // normalize=0 keeps each source at its own level; the limiter stops a loud sum from clipping
        format!("{labels}amix=inputs={}:duration=longest:normalize=0,alimiter=limit=0.95,apad[a]", inputs.len())
    };
    chains.push(mix);
    let output = cmd
        .args(["-filter_complex", &chains.join(";"), "-map", "0:v", "-map", "[a]", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k"])
        .args(["-shortest", "-movflags", "+faststart"])
        .arg(&tmp)
        .output()
        .context("could not run ffmpeg to add the audio")?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("ffmpeg could not add the audio: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    std::fs::rename(&tmp, video).context("could not replace the video with the version that has audio")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn tone(frames: usize, value: f32) -> Vec<Vec<f32>> {
        vec![vec![value; frames]; 2]
    }

    fn read(path: &Path) -> Vec<f32> {
        std::fs::read(path).unwrap().chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    fn track(name: &str) -> (AudioTrack, PathBuf) {
        let path = std::env::temp_dir().join(name);
        (AudioTrack::create(&path, RATE).unwrap(), path)
    }

    #[test]
    fn contiguous_chunks_are_written_back_to_back() {
        let (mut t, path) = track("capturra_audio_contiguous.f32");
        // 10 ms chunks, each starting where the last ended, with up to 1 ms of timestamp jitter
        for i in 0..10i64 {
            t.push(i * 10_000_000 + (i % 2) * 900_000, &tone(480, 0.5)).unwrap();
        }
        let stats = t.finish(100).unwrap();
        assert_eq!((stats.chunks, stats.frames, stats.gaps, stats.overlap_cut_ms), (10, 4800, 0, 0));
        assert_eq!(read(&path).len(), 4800 * 2);
        assert!(read(&path).iter().all(|&s| s == 0.5));
    }

    #[test]
    fn a_gap_becomes_silence_and_the_clock_is_kept() {
        let (mut t, path) = track("capturra_audio_gap.f32");
        t.push(0, &tone(480, 1.0)).unwrap();
        // the next chunk should start at 10 ms but starts at 110 ms: 100 ms of silence
        t.push(110_000_000, &tone(480, 1.0)).unwrap();
        let stats = t.finish(120).unwrap();
        assert_eq!((stats.gaps, stats.silence_ms, stats.largest_gap_ms), (1, 100, 100));
        let samples = read(&path);
        assert_eq!(samples.len(), 120 * 48 * 2, "exactly 120 ms of stereo audio");
        assert_eq!(samples[0], 1.0);
        assert_eq!(samples[480 * 2 + 10], 0.0, "in the gap");
        assert_eq!(samples[110 * 48 * 2 + 10], 1.0, "second chunk starts at 110 ms");
    }

    #[test]
    fn an_overlap_is_cut_and_the_start_is_aligned_to_clock_zero() {
        let (mut t, path) = track("capturra_audio_overlap.f32");
        // the first chunk starts 50 ms into the recording: 50 ms of silence first
        t.push(50_000_000, &tone(4800, 1.0)).unwrap(); // 100 ms
                                                       // overlaps the last 50 ms of it
        t.push(100_000_000, &tone(4800, 2.0)).unwrap();
        let stats = t.finish(0).unwrap();
        assert_eq!(stats.overlap_cut_ms, 50);
        assert_eq!(stats.frames, (50 + 100 + 50) * 48);
        let samples = read(&path);
        assert_eq!(samples[10], 0.0);
        assert_eq!(samples[60 * 48 * 2], 1.0);
        assert_eq!(samples[160 * 48 * 2], 2.0, "only the part past the overlap is kept");
    }

    #[test]
    fn audio_from_before_the_clock_started_is_dropped() {
        let (mut t, path) = track("capturra_audio_before.f32");
        // first chunk begins 20 ms before clock zero: its first 20 ms are cut
        t.push(-20_000_000, &tone(1920, 1.0)).unwrap(); // 40 ms
        let stats = t.finish(0).unwrap();
        assert_eq!((stats.frames, stats.overlap_cut_ms), (20 * 48, 20));
        assert_eq!(read(&path).len(), 20 * 48 * 2);
    }

    #[test]
    fn mono_is_played_on_both_sides_and_the_tail_is_padded() {
        let (mut t, path) = track("capturra_audio_mono.f32");
        t.push(0, &[vec![0.25; 480]]).unwrap();
        let stats = t.finish(50).unwrap();
        assert_eq!(stats.frames, 50 * 48);
        let s = read(&path);
        assert_eq!((s[0], s[1]), (0.25, 0.25));
        assert_eq!(s[s.len() - 1], 0.0);
    }

    #[test]
    fn the_recorder_thread_places_chunks_and_pads_to_the_end() {
        let path = std::env::temp_dir().join("capturra_audio_recorder.f32");
        let (rec, tx) = AudioRecorder::start(&path, SAMPLE_RATE).unwrap();
        tx.send(0, tone(4800, 1.0));
        tx.send(100_000_000, tone(4800, 1.0));
        let (p, stats) = rec.finish(300).unwrap();
        tx.send(300_000_000, tone(4800, 1.0)); // after finish: ignored, must not panic
        assert_eq!(p, path);
        assert_eq!((stats.chunks, stats.frames), (2, 300 * 48));
    }

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg").arg("-version").output().is_ok() && Command::new("ffprobe").arg("-version").output().is_ok()
    }

    fn probe(path: &Path, args: &[&str]) -> String {
        let out = Command::new("ffprobe").args(["-v", "error"]).args(args).args(["-of", "csv=p=0"]).arg(path).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The whole path with real ffmpeg: a video that starts 1000 ms after the clock, and a 1 kHz tone
    /// that starts 500 ms after the video does. In the muxed file the tone must start at 0.5 s and
    /// the file must keep the video's length and frames.
    #[test]
    fn the_muxed_audio_lines_up_with_the_first_video_frame() {
        if !ffmpeg_available() {
            return;
        }
        let video = std::env::temp_dir().join("capturra_audio_mux.mp4");
        let raw = std::env::temp_dir().join("capturra_audio_mux.f32");
        let (w, h) = (320u32, 180u32);
        let mut enc = crate::encoder::Encoder::new(&video, w, h, 30).unwrap();
        enc.push_frame(1000, &vec![90u8; (w * h * 4) as usize]).unwrap();
        let origin = enc.origin_ms().unwrap();
        enc.finish(4000).unwrap(); // 3.0 s of video
        let frames_before = probe(&video, &["-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames"]);
        let duration_before: f64 = probe(&video, &["-show_entries", "format=duration"]).parse().unwrap();

        // The tone starts at clock 1500 ms = video 0.5 s, and lasts 500 ms; silence before and after.
        let mut t = AudioTrack::create(&raw, SAMPLE_RATE).unwrap();
        let tone_frames = (SAMPLE_RATE / 2) as usize;
        let sine: Vec<f32> =
            (0..tone_frames).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / SAMPLE_RATE as f32).sin()).collect();
        t.push(1_500_000_000, &[sine.clone(), sine]).unwrap();
        t.finish(3900).unwrap(); // ends a little short of the video: must be padded

        mux(&video, &[AudioInput { path: &raw, rate: SAMPLE_RATE, tempo: 1.0 }], origin).unwrap();
        assert!(!video.with_extension("with-audio.mp4").exists(), "the temporary file is gone");
        assert_eq!(probe(&video, &["-select_streams", "a:0", "-show_entries", "stream=codec_name"]), "aac");
        let duration: f64 = probe(&video, &["-show_entries", "format=duration"]).parse().unwrap();
        assert!((duration - duration_before).abs() < 0.08, "length changed from {duration_before} to {duration}");
        let frames_after = probe(&video, &["-count_frames", "-select_streams", "v:0", "-show_entries", "stream=nb_read_frames"]);
        assert_eq!(frames_after, frames_before, "the picture must be copied untouched");

        let detect = Command::new("ffmpeg")
            .args(["-nostats", "-i"])
            .arg(&video)
            .args(["-vn", "-af", "silencedetect=noise=-50dB:d=0.1", "-f", "null", "-"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&detect.stderr).to_string();
        let silence_end = log
            .lines()
            .find_map(|l| l.split("silence_end: ").nth(1).map(|r| r.split_whitespace().next().unwrap().parse::<f64>().unwrap()))
            .unwrap_or_else(|| panic!("no silence_end in:\n{log}"));
        assert!((silence_end - 0.5).abs() < 0.06, "the tone starts at {silence_end} s, wanted 0.5\n{log}");
    }

    fn mean_volume_db(video: &Path, from: f64, secs: f64) -> f64 {
        let out = Command::new("ffmpeg")
            .args(["-nostats", "-ss", &from.to_string(), "-t", &secs.to_string(), "-i"])
            .arg(video)
            .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&out.stderr).to_string();
        log.lines()
            .find_map(|l| l.split("mean_volume: ").nth(1).map(|r| r.split_whitespace().next().unwrap().parse::<f64>().unwrap()))
            .unwrap_or_else(|| panic!("no mean_volume in:\n{log}"))
    }

    fn sine(freq: f32, secs: f32, rate: u32, amp: f32) -> Vec<f32> {
        (0..(secs * rate as f32) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin()).collect()
    }

    /// System audio plus a microphone at a different rate: both end up in the one sound track, the mic
    /// where it was recorded, and a tempo correction does not change the length.
    #[test]
    fn system_audio_and_a_microphone_are_mixed_into_one_track_on_the_same_clock() {
        if !ffmpeg_available() {
            return;
        }
        let video = std::env::temp_dir().join("capturra_audio_mix.mp4");
        let (sys_raw, mic_raw) =
            (std::env::temp_dir().join("capturra_audio_mix_sys.f32"), std::env::temp_dir().join("capturra_audio_mix_mic.f32"));
        let (w, h) = (320u32, 180u32);
        let mut enc = crate::encoder::Encoder::new(&video, w, h, 30).unwrap();
        enc.push_frame(1000, &vec![90u8; (w * h * 4) as usize]).unwrap();
        let origin = enc.origin_ms().unwrap();
        enc.finish(5000).unwrap(); // 4.0 s

        // system audio: a 440 Hz tone for the whole recording (clock 0 to 5 s) at 48 kHz
        let mut sys = AudioTrack::create(&sys_raw, 48_000).unwrap();
        let t = sine(440.0, 5.0, 48_000, 0.3);
        sys.push(0, &[t.clone(), t]).unwrap();
        sys.finish(5000).unwrap();
        // microphone at 44.1 kHz, mono: a 1 kHz tone from clock 2.5 s to 3.0 s (video 1.5 s to 2.0 s)
        let mut mic = AudioTrack::create(&mic_raw, 44_100).unwrap();
        mic.push(2_500_000_000, &[sine(1000.0, 0.5, 44_100, 0.3)]).unwrap();
        mic.finish(5000).unwrap();

        mux(
            &video,
            &[AudioInput { path: &sys_raw, rate: 48_000, tempo: 1.0 }, AudioInput { path: &mic_raw, rate: 44_100, tempo: 1.00002 }],
            origin,
        )
        .unwrap();
        assert_eq!(probe(&video, &["-select_streams", "a", "-show_entries", "stream=index"]).lines().count(), 1, "one sound track");
        let duration: f64 = probe(&video, &["-show_entries", "format=duration"]).parse().unwrap();
        assert!((duration - 4.0).abs() < 0.1, "length {duration}");
        let alone = mean_volume_db(&video, 0.3, 0.5);
        let mixed = mean_volume_db(&video, 1.55, 0.4);
        assert!(
            mixed - alone > 2.0 && mixed - alone < 4.5,
            "the microphone should add about 3 dB where it speaks: {alone} dB then {mixed} dB"
        );
        let after = mean_volume_db(&video, 2.3, 0.5);
        assert!((after - alone).abs() < 1.0, "the microphone is silent again afterwards: {alone} dB vs {after} dB");
    }

    #[test]
    fn a_failed_mux_leaves_the_video_alone() {
        if !ffmpeg_available() {
            return;
        }
        let video = std::env::temp_dir().join("capturra_audio_mux_fail.mp4");
        std::fs::write(&video, b"not a video").unwrap();
        let raw = std::env::temp_dir().join("capturra_audio_mux_fail.f32");
        std::fs::write(&raw, vec![0u8; 800]).unwrap();
        assert!(mux(&video, &[AudioInput { path: &raw, rate: SAMPLE_RATE, tempo: 1.0 }], 0).is_err());
        assert_eq!(std::fs::read(&video).unwrap(), b"not a video");
        assert!(!video.with_extension("with-audio.mp4").exists());
    }
}

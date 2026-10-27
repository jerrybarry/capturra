//! Microphone samples on the recording clock. Platform-independent; `mic_macos.rs` feeds it.
//!
//! The microphone's own clock is not the Mac's: over a long recording it runs a little fast or slow,
//! which would slide the voice away from the picture. Chunks are placed back to back from the first
//! one (a callback's arrival time is too jittery to place each chunk by), and the number of samples
//! the device really delivered over the recording is compared with the clock afterwards: if the device
//! was measurably off nominal, `drift_tempo` gives the factor that puts the track back on the clock.

/// A drift smaller than this (20 ppm, under 40 ms in 30 minutes) is not worth correcting.
const MIN_DRIFT: f64 = 0.00002;
/// A larger one means the measurement is wrong (a stalled device, a sleep), not that the clock drifts.
const MAX_DRIFT: f64 = 0.005;
/// Shorter recordings are too short to measure a rate from callback times.
const MIN_MEASURE_SECS: f64 = 60.0;

/// Assigns each incoming chunk its place on the recording clock.
#[derive(Debug, Default)]
pub struct MicPlacer {
    rate: u32,
    first_start_ns: Option<i64>,
    frames: u64,
    last_end_ns: i64,
}

impl MicPlacer {
    pub fn new(rate: u32) -> Self {
        Self { rate, ..Default::default() }
    }

    /// A chunk of `frames` sample frames has just arrived at `arrival_ns` (clock time). Returns the time of
    /// its first sample. The first chunk is placed by its arrival (it ended then); every later one
    /// follows the previous one directly.
    pub fn place(&mut self, arrival_ns: i64, frames: usize) -> i64 {
        let len_ns = frames as i64 * 1_000_000_000 / self.rate.max(1) as i64;
        let start = match self.first_start_ns {
            None => {
                let s = arrival_ns - len_ns;
                self.first_start_ns = Some(s);
                s
            }
            Some(first) => first + self.frames as i64 * 1_000_000_000 / self.rate.max(1) as i64,
        };
        self.frames += frames as u64;
        self.last_end_ns = arrival_ns;
        start
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The factor to give `mux` (`AudioInput::tempo`) so the track matches the clock; 1.0 when the
    /// recording was too short to measure, the drift is negligible, or the measurement looks wrong.
    pub fn tempo(&self) -> f64 {
        let Some(first) = self.first_start_ns else { return 1.0 };
        drift_tempo(self.frames, self.last_end_ns - first, self.rate)
    }
}

/// `frames` sample frames arrived over `elapsed_ns` of clock time from a device set to `rate`. If it
/// really delivered `frames / elapsed` per second instead, played at `rate` the track would be
/// `measured / rate` too long or short; speeding it up by that factor corrects it.
pub fn drift_tempo(frames: u64, elapsed_ns: i64, rate: u32) -> f64 {
    let secs = elapsed_ns as f64 / 1e9;
    if secs < MIN_MEASURE_SECS || rate == 0 {
        return 1.0;
    }
    let measured = frames as f64 / secs;
    let ratio = measured / rate as f64;
    let drift = (ratio - 1.0).abs();
    if !(MIN_DRIFT..=MAX_DRIFT).contains(&drift) {
        return 1.0;
    }
    ratio
}

/// Split interleaved samples into one `Vec` per channel.
pub fn to_planar(interleaved: &[f32], channels: usize) -> Vec<Vec<f32>> {
    let channels = channels.max(1);
    let mut out = vec![Vec::with_capacity(interleaved.len() / channels); channels];
    for frame in interleaved.chunks_exact(channels) {
        for (c, s) in frame.iter().enumerate() {
            out[c].push(*s);
        }
    }
    out
}

pub fn i16_to_f32(s: i16) -> f32 {
    s as f32 / 32768.0
}

pub fn u16_to_f32(s: u16) -> f32 {
    (s as f32 - 32768.0) / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_follow_each_other_whatever_the_callback_jitter() {
        let mut p = MicPlacer::new(48_000);
        // 10 ms chunks (480 frames) arriving with up to 7 ms of jitter
        let first = p.place(1_010_000_000, 480);
        assert_eq!(first, 1_000_000_000, "the first chunk ended when it arrived");
        let second = p.place(1_024_000_000, 480);
        assert_eq!(second, 1_010_000_000, "the second starts where the first ended, not where it arrived");
        assert_eq!(p.place(1_033_000_000, 480), 1_020_000_000);
        assert_eq!(p.frames(), 1440);
    }

    #[test]
    fn a_short_recording_is_never_corrected() {
        assert_eq!(drift_tempo(48_000 * 30 + 50, 30_000_000_000, 48_000), 1.0);
    }

    #[test]
    fn a_device_running_fast_or_slow_is_corrected_by_the_measured_ratio() {
        // 30 min at a nominal 48 kHz; the device really produced 48_002.4 frames per second
        let secs = 1800i64;
        let frames = (48_002.4 * secs as f64) as u64;
        let t = drift_tempo(frames, secs * 1_000_000_000, 48_000);
        assert!((t - 48_002.4 / 48_000.0).abs() < 1e-7, "{t}");
        let slow = drift_tempo((47_997.0 * secs as f64) as u64, secs * 1_000_000_000, 48_000);
        assert!(slow < 1.0 && (slow - 47_997.0 / 48_000.0).abs() < 1e-7);
    }

    #[test]
    fn a_negligible_or_absurd_drift_is_left_alone() {
        assert_eq!(drift_tempo(48_000 * 600, 600_000_000_000, 48_000), 1.0, "exactly nominal");
        assert_eq!(drift_tempo((48_000.5 * 600.0) as u64, 600_000_000_000, 48_000), 1.0, "10 ppm");
        assert_eq!(drift_tempo(48_000 * 300, 600_000_000_000, 48_000), 1.0, "half the samples: a stall, not drift");
    }

    #[test]
    fn the_placer_reports_the_tempo_after_a_long_run() {
        let mut p = MicPlacer::new(44_100);
        let mut t = 0i64;
        // 10 minutes of 10 ms chunks arriving a hair slower than the device's nominal rate
        for _ in 0..60_000 {
            t += 10_000_000 + 4_000; // each 441-frame chunk took 10.004 ms of clock time
            p.place(t, 441);
        }
        let tempo = p.tempo();
        assert!((tempo - (1.0 / 1.0004)).abs() < 2e-5, "tempo {tempo}");
    }

    #[test]
    fn interleaved_samples_are_split_per_channel_and_integers_convert() {
        assert_eq!(to_planar(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2), vec![vec![1.0, 3.0, 5.0], vec![2.0, 4.0, 6.0]]);
        assert_eq!(to_planar(&[1.0, 2.0, 3.0], 1), vec![vec![1.0, 2.0, 3.0]]);
        assert_eq!(i16_to_f32(i16::MIN), -1.0);
        assert_eq!(u16_to_f32(32768), 0.0);
        assert!((i16_to_f32(16384) - 0.5).abs() < 1e-6);
    }
}

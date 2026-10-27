//! The microphone on macOS: the default input device through `cpal` (CoreAudio), and the Microphone
//! permission through AVFoundation. This is the only file that touches either.

use crate::audio::AudioSender;
use crate::clock::Clock;
use crate::mic::{self, MicPlacer};
use crate::MicAccess;
use anyhow::{anyhow, bail, Result};
use block2::RcBlock;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use objc2::runtime::Bool;
use objc2::{class, msg_send};
use objc2_foundation::NSString;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// `AVMediaTypeAudio`.
fn media_audio() -> objc2::rc::Retained<NSString> {
    NSString::from_str("soun")
}

/// What macOS has granted the app we run from. Never shows a prompt.
pub fn access() -> MicAccess {
    let media = media_audio();
    // AVAuthorizationStatus: 0 not determined, 1 restricted, 2 denied, 3 authorized
    let status: isize = unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: &*media] };
    match status {
        3 => MicAccess::Authorized,
        2 => MicAccess::Denied,
        1 => MicAccess::Restricted,
        _ => MicAccess::NotDetermined,
    }
}

/// Ask for access if it has not been decided yet (macOS shows its prompt for the app we run from) and
/// wait for the answer, up to two minutes. Returns the status afterwards.
pub fn request_access() -> MicAccess {
    if access() != MicAccess::NotDetermined {
        return access();
    }
    let media = media_audio();
    let (tx, rx) = mpsc::channel::<bool>();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    unsafe {
        let _: () = msg_send![class!(AVCaptureDevice), requestAccessForMediaType: &*media, completionHandler: &*block];
    }
    let _ = rx.recv_timeout(Duration::from_secs(120));
    access()
}

/// The system's wording for what to do about a missing permission.
pub fn access_problem(status: MicAccess, app: &str) -> Option<String> {
    match status {
        MicAccess::Authorized => None,
        MicAccess::Denied => Some(format!(
            "Microphone access is off for {app}. Turn it on in System Settings > Privacy & Security > Microphone, \\
             then quit and reopen the app."
        )),
        MicAccess::Restricted => Some("Microphone access is restricted on this Mac (a profile or Screen Time blocks it).".to_string()),
        MicAccess::NotDetermined => Some(format!("Microphone access has not been answered yet for {app}.")),
    }
}

struct Sink {
    clock: Clock,
    sender: AudioSender,
    placer: MicPlacer,
}

/// An open microphone. Samples are thrown away until `arm` hands it the recording clock.
pub struct Mic {
    stream: Stream,
    sink: Arc<Mutex<Option<Sink>>>,
    pub device: String,
    pub rate: u32,
    pub channels: u16,
}

/// What the microphone delivered over the recording.
#[derive(Debug, Clone, Copy)]
pub struct MicSummary {
    pub frames: u64,
    /// Pass to `audio::AudioInput::tempo`.
    pub tempo: f64,
}

impl Mic {
    /// Open the default input device and start it. Fails loudly if there is none.
    pub fn open() -> Result<Mic> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no microphone was found. Plug one in or choose an input in System Settings > Sound."))?;
        let name = device.name().unwrap_or_else(|_| "the default input".to_string());
        let supported =
            device.default_input_config().map_err(|e| anyhow!("could not read the settings of the microphone {name:?}: {e}"))?;
        let (rate, channels, format) = (supported.sample_rate().0, supported.channels(), supported.sample_format());
        let config = supported.config();
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let on_error = |e: cpal::StreamError| eprintln!("microphone stream error: {e}");
        let ch = channels as usize;
        let stream = match format {
            SampleFormat::F32 => {
                let s = sink.clone();
                device.build_input_stream(&config, move |d: &[f32], _| feed(&s, d, ch), on_error, None)
            }
            SampleFormat::I16 => {
                let s = sink.clone();
                device.build_input_stream(
                    &config,
                    move |d: &[i16], _| feed(&s, &d.iter().map(|&x| mic::i16_to_f32(x)).collect::<Vec<f32>>(), ch),
                    on_error,
                    None,
                )
            }
            SampleFormat::U16 => {
                let s = sink.clone();
                device.build_input_stream(
                    &config,
                    move |d: &[u16], _| feed(&s, &d.iter().map(|&x| mic::u16_to_f32(x)).collect::<Vec<f32>>(), ch),
                    on_error,
                    None,
                )
            }
            other => bail!("the microphone {name:?} delivers {other:?} samples, which Capturra cannot read"),
        }
        .map_err(|e| anyhow!("could not open the microphone {name:?}: {e}"))?;
        stream.play().map_err(|e| anyhow!("could not start the microphone {name:?}: {e}"))?;
        Ok(Mic { stream, sink, device: name, rate, channels })
    }

    /// From now on, samples go to `sender`, placed on `clock`.
    pub fn arm(&self, clock: Clock, sender: AudioSender) {
        *self.sink.lock().unwrap() = Some(Sink { clock, sender, placer: MicPlacer::new(self.rate) });
    }

    /// Stop the device and say what it delivered.
    pub fn stop(self) -> MicSummary {
        let _ = self.stream.pause();
        drop(self.stream);
        match self.sink.lock().unwrap().take() {
            Some(s) => MicSummary { frames: s.placer.frames(), tempo: s.placer.tempo() },
            None => MicSummary { frames: 0, tempo: 1.0 },
        }
    }
}

fn feed(sink: &Mutex<Option<Sink>>, data: &[f32], channels: usize) {
    // Never wait on the audio thread: if `arm` holds the lock this chunk is simply not recorded.
    let Ok(mut guard) = sink.try_lock() else { return };
    let Some(s) = guard.as_mut() else { return };
    let frames = data.len() / channels.max(1);
    if frames == 0 {
        return;
    }
    let start_ns = s.placer.place(s.clock.now_ns(), frames);
    s.sender.send(start_ns, mic::to_planar(data, channels));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading the status goes through AVFoundation and must neither crash nor prompt. (It does not open
    /// the microphone: that would record.)
    #[test]
    fn the_permission_status_can_be_read_without_a_prompt() {
        let status = access();
        eprintln!("microphone access for this process: {status:?}");
        assert!(access_problem(MicAccess::Authorized, "x").is_none());
        assert!(access_problem(MicAccess::Denied, "Terminal").unwrap().contains("Terminal"));
        let _ = status;
    }
}

//! Recording sessions for the UI: turns a request from the popover into a `capturra-recorder`
//! run on its own thread, and lets the UI stop or cancel it. No Tauri types in here, so it is
//! unit-tested without a window.

use capturra_recorder::geometry::Source;
use capturra_recorder::{EventSink, RecordOptions, RecordingPaths};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// What the popover asks for. `kind` is "display", "window" or "region".
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct StartRequest {
    pub kind: String,
    pub display: Option<usize>,
    pub window: Option<u32>,
    /// x, y, width, height in points from the display's top-left corner.
    pub region: Option<[f64; 4]>,
    pub fps: Option<u32>,
    pub countdown: Option<u32>,
    /// Keep Capturra's own windows in a display or region recording. `None` = leave them out.
    pub show_capturra: Option<bool>,
    /// Record system audio too. `None` = off (the start paths fill it from the saved setting).
    pub system_audio: Option<bool>,
    /// Record the microphone too. `None` = off (the start paths fill it from the saved setting).
    pub microphone: Option<bool>,
}

impl StartRequest {
    /// Whether Capturra's own windows are left out of the recording (the default).
    pub fn hides_capturra(&self) -> bool {
        !self.show_capturra.unwrap_or(false)
    }

    pub fn source(&self) -> Result<Source, String> {
        match self.kind.as_str() {
            "display" => Ok(Source::Display(self.display.unwrap_or(0))),
            "window" => self.window.map(Source::Window).ok_or_else(|| "choose a window to record".to_string()),
            "region" => {
                let [x, y, w, h] = self.region.ok_or_else(|| "enter a region (x, y, width, height)".to_string())?;
                let rect = capturra_recorder::geometry::parse_region(&format!("{x},{y},{w},{h}"))?;
                Ok(Source::Region { display: self.display.unwrap_or(0), rect })
            }
            other => Err(format!("unknown source kind {other:?}")),
        }
    }
}

/// The flags of the running recording.
struct Active {
    stop: Arc<AtomicBool>,
    /// Whether a pointer click asked for the stop; see `RecordOptions::drop_stop_click`.
    by_click: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Recorder {
    active: Mutex<Option<Active>>,
    /// The system hand pointer, read once at startup on the main thread (macOS hands the standard cursors
    /// out nowhere else) and saved with every recording that sees the pointer over something clickable.
    hand: Mutex<Option<capturra_recorder::cursor::CursorImageData>>,
}

impl Recorder {
    pub fn set_hand(&self, hand: Option<capturra_recorder::cursor::CursorImageData>) {
        *self.hand.lock().unwrap() = hand;
    }

    pub fn is_active(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    /// Start recording on a new thread. `on_done` runs on that thread when the recording has
    /// finished (or failed or been cancelled), after the recorder is free again.
    pub fn start(
        self: &Arc<Self>,
        req: &StartRequest,
        out_dir: PathBuf,
        on_event: EventSink,
        on_done: impl FnOnce(Result<RecordingPaths, String>) + Send + 'static,
    ) -> Result<(), String> {
        self.start_with(req, out_dir, on_event, false, on_done)
    }

    pub(crate) fn start_with(
        self: &Arc<Self>,
        req: &StartRequest,
        out_dir: PathBuf,
        on_event: EventSink,
        allow_no_input: bool,
        on_done: impl FnOnce(Result<RecordingPaths, String>) + Send + 'static,
    ) -> Result<(), String> {
        let source = req.source()?;
        let stop = Arc::new(AtomicBool::new(false));
        let by_click = Arc::new(AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap();
            if active.is_some() {
                return Err("already recording".to_string());
            }
            *active = Some(Active { stop: stop.clone(), by_click: by_click.clone() });
        }
        let opts = RecordOptions {
            on_event: Some(on_event),
            source,
            out_dir,
            countdown_secs: req.countdown.unwrap_or(3),
            allow_no_input,
            drop_stop_click: by_click,
            hide_own_windows: req.hides_capturra(),
            system_audio: req.system_audio.unwrap_or(false),
            microphone: req.microphone.unwrap_or(false),
            hand_cursor: self.hand.lock().unwrap().clone(),
            fps: req.fps.unwrap_or(30),
            // The UI stops it; there is no time limit.
            duration: None,
            stop,
        };
        let me = self.clone();
        let spawned = std::thread::Builder::new().name("capturra-session".into()).spawn(move || {
            let result = capturra_recorder::record(opts).map_err(|e| format!("{e:#}"));
            *me.active.lock().unwrap() = None;
            on_done(result);
        });
        if let Err(e) = spawned {
            *self.active.lock().unwrap() = None;
            return Err(format!("could not start the recording thread: {e}"));
        }
        Ok(())
    }

    /// Stop (or, during the countdown, cancel) because the user clicked the menu-bar icon or a
    /// Stop button. That click is trimmed from the recording. `false` if nothing is running.
    pub fn stop(&self) -> bool {
        self.stop_with(true)
    }

    /// Stop from a keyboard shortcut: no click caused it, so every click stays in the recording.
    pub fn stop_without_click(&self) -> bool {
        self.stop_with(false)
    }

    fn stop_with(&self, by_click: bool) -> bool {
        match self.active.lock().unwrap().as_ref() {
            Some(run) => {
                run.by_click.store(by_click, Ordering::SeqCst);
                run.stop.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }
}

/// Local time as `YYYY-MM-DD HH.MM.SS` (dots, because ':' is awkward in Finder).
pub fn timestamp_now() -> String {
    std::process::Command::new("date")
        .arg("+%Y-%m-%d %H.%M.%S")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            format!("recording-{secs}")
        })
}

/// Wait until `cond` holds or `timeout` passes (tests).
#[cfg(test)]
fn wait_for(timeout: std::time::Duration, mut cond: impl FnMut() -> bool) -> bool {
    let end = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < end {
        if cond() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    cond()
}

#[cfg(test)]
mod tests {
    use super::*;
    use capturra_recorder::geometry::PtRect;
    use capturra_recorder::RecordEvent;
    use std::time::Duration;

    fn req(kind: &str) -> StartRequest {
        StartRequest {
            kind: kind.into(),
            display: None,
            window: None,
            region: None,
            fps: None,
            countdown: None,
            show_capturra: None,
            system_audio: None,
            microphone: None,
        }
    }

    #[test]
    fn capturra_is_hidden_from_a_recording_unless_asked_to_show() {
        assert!(req("display").hides_capturra());
        assert!(StartRequest { show_capturra: Some(false), ..req("display") }.hides_capturra());
        assert!(!StartRequest { show_capturra: Some(true), ..req("display") }.hides_capturra());
    }

    #[test]
    fn a_request_from_the_popover_without_the_field_still_parses_and_hides_capturra() {
        let r: StartRequest = serde_json::from_str(r#"{"kind":"display","display":0}"#).unwrap();
        assert!(r.show_capturra.is_none() && r.hides_capturra());
        assert!(r.system_audio.is_none() && r.microphone.is_none(), "sound stays off unless the saved setting or the caller asks for it");
    }

    #[test]
    fn system_audio_is_off_unless_asked_for() {
        assert!(!req("display").system_audio.unwrap_or(false));
        let r: StartRequest = serde_json::from_str(r#"{"kind":"display","system_audio":true}"#).unwrap();
        assert_eq!(r.system_audio, Some(true));
        let m: StartRequest = serde_json::from_str(r#"{"kind":"display","microphone":true}"#).unwrap();
        assert_eq!((m.microphone, m.system_audio), (Some(true), None), "the microphone is its own choice");
    }

    #[test]
    fn requests_map_to_sources() {
        assert_eq!(req("display").source().unwrap(), Source::Display(0));
        assert_eq!(StartRequest { display: Some(2), ..req("display") }.source().unwrap(), Source::Display(2));
        assert_eq!(StartRequest { window: Some(77), ..req("window") }.source().unwrap(), Source::Window(77));
        let r = StartRequest { display: Some(1), region: Some([10.0, 20.0, 300.0, 200.0]), ..req("region") }.source().unwrap();
        assert_eq!(r, Source::Region { display: 1, rect: PtRect { x: 10.0, y: 20.0, w: 300.0, h: 200.0 } });
    }

    #[test]
    fn incomplete_or_bad_requests_say_what_is_missing() {
        assert!(req("window").source().unwrap_err().contains("choose a window"));
        assert!(req("region").source().unwrap_err().contains("region"));
        let e = StartRequest { region: Some([0.0, 0.0, 0.0, 10.0]), ..req("region") }.source().unwrap_err();
        assert!(e.contains("at least 2"), "{e}");
        assert!(req("nonsense").source().unwrap_err().contains("unknown source kind"));
    }

    #[test]
    fn the_timestamp_folder_name_is_safe_for_finder() {
        let t = timestamp_now();
        assert!(!t.is_empty() && !t.contains(':') && !t.contains('/'), "{t}");
    }

    #[test]
    fn a_click_stop_trims_the_click_and_a_shortcut_stop_does_not() {
        let rec = Arc::new(Recorder::default());
        let (stop, by_click) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        *rec.active.lock().unwrap() = Some(Active { stop: stop.clone(), by_click: by_click.clone() });
        assert!(rec.stop_without_click());
        assert!(stop.load(Ordering::SeqCst) && !by_click.load(Ordering::SeqCst));
        assert!(rec.stop());
        assert!(by_click.load(Ordering::SeqCst));
    }

    #[test]
    fn stop_with_nothing_running_is_a_noop() {
        assert!(!Arc::new(Recorder::default()).stop());
    }

    // Needs Screen Recording permission for the app running the tests (it has it here, as the
    // recorder tests already do). Records the main display for ~2 s with no input events.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_session_counts_down_records_stops_and_frees_the_recorder() {
        let dir = std::env::temp_dir().join(format!("capturra-session-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = Arc::new(Recorder::default());
        let events = Arc::new(Mutex::new(Vec::<RecordEvent>::new()));
        let done: Arc<Mutex<Option<Result<RecordingPaths, String>>>> = Arc::new(Mutex::new(None));
        let (ev, dn) = (events.clone(), done.clone());
        let request = StartRequest { countdown: Some(1), ..req("display") };
        rec.start_with(&request, dir.clone(), Arc::new(move |e| ev.lock().unwrap().push(e)), true, move |r| *dn.lock().unwrap() = Some(r))
            .unwrap();
        assert!(rec.is_active());
        assert_eq!(rec.start_with(&request, dir.clone(), Arc::new(|_| {}), true, |_| {}).unwrap_err(), "already recording");
        assert!(wait_for(Duration::from_secs(15), || events.lock().unwrap().contains(&RecordEvent::Started)));
        std::thread::sleep(Duration::from_secs(2));
        assert!(rec.stop());
        assert!(wait_for(Duration::from_secs(20), || done.lock().unwrap().is_some()), "recording never finished");
        let result = done.lock().unwrap().take().unwrap();
        let paths = result.unwrap_or_else(|e| panic!("recording failed: {e}"));
        assert!(paths.video.exists() && paths.events.exists());
        assert_eq!(*events.lock().unwrap(), vec![RecordEvent::Countdown(1), RecordEvent::Started]);
        assert!(!rec.is_active(), "the recorder must be free again for the next recording");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn cancelling_during_the_countdown_records_nothing() {
        let dir = std::env::temp_dir().join(format!("capturra-session-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = Arc::new(Recorder::default());
        let done: Arc<Mutex<Option<Result<RecordingPaths, String>>>> = Arc::new(Mutex::new(None));
        let dn = done.clone();
        let request = StartRequest { countdown: Some(5), ..req("display") };
        rec.start_with(&request, dir.clone(), Arc::new(|_| {}), true, move |r| *dn.lock().unwrap() = Some(r)).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(rec.stop());
        assert!(wait_for(Duration::from_secs(10), || done.lock().unwrap().is_some()));
        let err = done.lock().unwrap().take().unwrap().unwrap_err();
        assert!(err.contains("cancelled during the countdown"), "{err}");
        assert!(!dir.join("events.json").exists());
        assert!(!rec.is_active());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! The global start/stop shortcut. Press it to start recording what the popover last used
//! (display or region; a window is not remembered), press it again to stop. A stop from the
//! shortcut is not a click, so no click is trimmed from the recording.

use crate::session::{Recorder, StartRequest};
use crate::settings::{LastSource, Settings};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

/// The registered shortcut and, if the last attempt to register one failed, why.
#[derive(Default)]
pub struct HotkeyState {
    current: Mutex<Option<Shortcut>>,
    error: Mutex<Option<String>>,
}

impl HotkeyState {
    pub fn error(&self) -> Option<String> {
        self.error.lock().unwrap().clone()
    }
}

pub fn parse(text: &str) -> Result<Shortcut, String> {
    text.parse::<Shortcut>().map_err(|e| format!("{text:?} is not a valid shortcut ({e}). Example: CommandOrControl+Shift+2"))
}

/// What pressing the shortcut starts: the last source from the settings, else the main display.
pub fn request_from_settings(s: &Settings) -> StartRequest {
    let (kind, display, region) = match &s.last_source {
        Some(LastSource { kind, display, region }) if kind == "display" || kind == "region" => (kind.clone(), *display, *region),
        _ => ("display".to_string(), None, None),
    };
    // A region request without its numbers would be refused; fall back to the whole display.
    let kind = if kind == "region" && region.is_none() { "display".to_string() } else { kind };
    StartRequest {
        kind,
        display: display.or(Some(0)),
        window: None,
        region,
        fps: Some(s.fps),
        countdown: Some(s.countdown),
        show_capturra: Some(s.show_capturra),
        system_audio: Some(s.system_audio),
        microphone: Some(s.microphone),
    }
}

/// Replace the registered shortcut with `text` (empty = none). The old one is released first, so
/// a failure leaves no shortcut; the reason is kept for the popover.
pub fn apply(app: &AppHandle, text: &str) -> Result<(), String> {
    let state = app.state::<HotkeyState>();
    let shortcuts = app.global_shortcut();
    if let Some(old) = state.current.lock().unwrap().take() {
        let _ = shortcuts.unregister(old);
    }
    *state.error.lock().unwrap() = None;
    if text.is_empty() {
        return Ok(());
    }
    let result = parse(text).and_then(|sc| {
        shortcuts
            .on_shortcut(sc, |app, _, event| {
                if event.state == ShortcutState::Pressed {
                    pressed(app);
                }
            })
            .map_err(|e| format!("could not register {text}: {e}. Another app may already use it."))?;
        *state.current.lock().unwrap() = Some(sc);
        Ok(())
    });
    if let Err(e) = &result {
        *state.error.lock().unwrap() = Some(e.clone());
    }
    result
}

fn pressed(app: &AppHandle) {
    let rec = app.state::<Arc<Recorder>>();
    if rec.is_active() {
        rec.stop_without_click();
        return;
    }
    let settings = crate::settings::config_path().map(|p| crate::settings::load(&p).settings).unwrap_or_default();
    if let Err(message) = crate::begin_recording(app, request_from_settings(&settings)) {
        crate::show_failure(app, message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(last: Option<LastSource>) -> Settings {
        Settings { last_source: last, fps: 60, countdown: 0, ..Default::default() }
    }

    #[test]
    fn with_nothing_remembered_the_shortcut_records_the_main_display() {
        let r = request_from_settings(&settings_with(None));
        assert_eq!((r.system_audio, r.microphone), (Some(false), Some(false)));
        assert_eq!(
            request_from_settings(&Settings { microphone: true, ..settings_with(None) }).microphone,
            Some(true),
            "the shortcut follows the saved microphone choice"
        );
        assert_eq!(
            request_from_settings(&Settings { system_audio: true, ..settings_with(None) }).system_audio,
            Some(true),
            "the shortcut follows the saved audio choice"
        );
        assert_eq!((r.kind.as_str(), r.display, r.region, r.window), ("display", Some(0), None, None));
        assert_eq!((r.fps, r.countdown), (Some(60), Some(0)));
    }

    #[test]
    fn it_repeats_the_last_display_or_region() {
        let d = request_from_settings(&settings_with(Some(LastSource { kind: "display".into(), display: Some(1), region: None })));
        assert_eq!((d.kind.as_str(), d.display), ("display", Some(1)));
        let region = Some([10.0, 20.0, 800.0, 600.0]);
        let r = request_from_settings(&settings_with(Some(LastSource { kind: "region".into(), display: Some(1), region })));
        assert_eq!((r.kind.as_str(), r.display, r.region), ("region", Some(1), region));
        assert!(r.source().is_ok());
    }

    #[test]
    fn a_remembered_region_without_numbers_or_an_unknown_kind_falls_back_to_the_display() {
        let broken = request_from_settings(&settings_with(Some(LastSource { kind: "region".into(), display: Some(2), region: None })));
        assert_eq!((broken.kind.as_str(), broken.display), ("display", Some(2)));
        let odd = request_from_settings(&settings_with(Some(LastSource { kind: "window".into(), display: None, region: None })));
        assert_eq!(odd.kind, "display");
    }

    #[test]
    fn shortcuts_are_parsed_with_a_helpful_error() {
        assert!(parse("CommandOrControl+Shift+2").is_ok());
        assert!(parse("Alt+Space").is_ok());
        let e = parse("Banana+Z").unwrap_err();
        assert!(e.contains("Banana+Z") && e.contains("Example"), "{e}");
        assert!(parse("").is_err());
    }
}

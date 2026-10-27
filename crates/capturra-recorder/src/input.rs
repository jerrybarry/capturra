//! Global mouse/keyboard logger (macOS). Records timing and positions only.
//! Key identity is intentionally discarded.

use crate::clock::Clock;
use anyhow::{bail, Result};
use capturra_core::{InputEvent, MouseButton};
use rdev::{listen, Button, EventType};
use std::sync::{Arc, Mutex};

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// Input Monitoring permission (macOS 10.15+). Does not show a prompt.
    fn CGPreflightListenEventAccess() -> bool;
}

/// The app macOS attributes our permissions to: the terminal / launcher we were started from.
pub fn responsible_app() -> String {
    if let Ok(id) = std::env::var("__CFBundleIdentifier") {
        return id;
    }
    match std::env::var("TERM_PROGRAM") {
        Ok(p) => p,
        Err(_) => "the app you launched capturra from (e.g. Terminal, iTerm, VS Code)".to_string(),
    }
}

/// (Accessibility, Input Monitoring) as granted to the launching app right now. Never prompts.
pub fn status() -> (bool, bool) {
    unsafe { (crate::ax::trusted(), CGPreflightListenEventAccess()) }
}

/// Fail up front, with an actionable message, if input events cannot be captured.
pub fn check_permissions() -> Result<()> {
    let accessibility = crate::ax::trusted();
    let input_monitoring = unsafe { CGPreflightListenEventAccess() };
    if accessibility && input_monitoring {
        return Ok(());
    }
    let mut missing = Vec::new();
    if !accessibility {
        missing.push("Accessibility");
    }
    if !input_monitoring {
        missing.push("Input Monitoring");
    }
    bail!(
        "missing permission: {}\n\
         macOS grants these to the app that launched capturra, which is: {}\n\
         Enable it in System Settings > Privacy & Security > Accessibility and > Input Monitoring, \
         then quit and reopen that app and re-run. Without this no mouse or keyboard events are recorded.",
        missing.join(" + "),
        responsible_app()
    );
}

pub struct InputLogger {
    events: Arc<Mutex<Vec<InputEvent>>>,
}

fn map_button(b: Button) -> MouseButton {
    match b {
        Button::Left => MouseButton::Left,
        Button::Right => MouseButton::Right,
        Button::Middle => MouseButton::Middle,
        Button::Unknown(_) => MouseButton::Other,
    }
}

impl InputLogger {
    pub fn start(clock: Clock) -> Self {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = events.clone();
        // `rdev::listen` blocks forever and cannot be stopped, so it lives on a
        // detached thread; we just snapshot the buffer when recording ends.
        std::thread::spawn(move || {
            let mut last = (0.0f64, 0.0f64);
            let result = listen(move |ev| {
                let t_ms = clock.now_ms();
                let e = match ev.event_type {
                    EventType::MouseMove { x, y } => {
                        last = (x, y);
                        Some(InputEvent::Move { t_ms, x, y })
                    }
                    EventType::ButtonPress(b) => Some(InputEvent::Down { t_ms, x: last.0, y: last.1, button: map_button(b) }),
                    EventType::ButtonRelease(b) => Some(InputEvent::Up { t_ms, x: last.0, y: last.1, button: map_button(b) }),
                    EventType::KeyPress(_) => Some(InputEvent::Key { t_ms }),
                    _ => None,
                };
                if let Some(e) = e {
                    sink.lock().unwrap().push(e);
                }
            });
            if let Err(e) = result {
                eprintln!(
                    "input listener failed: {e:?}\nGrant Accessibility and Input Monitoring to your terminal / app in System Settings > Privacy & Security."
                );
            }
        });
        Self { events }
    }

    pub fn snapshot(&self) -> Vec<InputEvent> {
        self.events.lock().unwrap().clone()
    }
}

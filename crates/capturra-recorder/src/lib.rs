//! Screen + input recording. Real capture is macOS-only (ScreenCaptureKit, the Accessibility API); the encoder,
//! the audio placement, the frame pacing and the coordinate mapping are portable and tested anywhere ffmpeg exists.
//!
//! Everything is stamped from one monotonic clock (`Clock`): video frames, input events, cursor changes,
//! the focused field, what is under the pointer, system audio and the microphone.
#![allow(clippy::too_many_arguments)] // the capture thread is handed everything it needs rather than sharing a struct

pub mod audio;
mod clock;
pub mod encoder;
pub mod focus;
pub mod geometry;
pub mod hover;
pub mod mic;
mod pump;
pub mod quality;

#[cfg(target_os = "macos")]
mod ax;
#[cfg(target_os = "macos")]
mod capture;
pub mod cursor;
#[cfg(target_os = "macos")]
mod cursor_macos;
#[cfg(target_os = "macos")]
mod focus_macos;
#[cfg(target_os = "macos")]
mod hover_macos;
#[cfg(target_os = "macos")]
mod input;
#[cfg(target_os = "macos")]
mod mic_macos;
#[cfg(target_os = "macos")]
mod sources;

pub use clock::Clock;

use anyhow::Result;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

/// Progress a UI can follow while `record` runs. The CLI ignores these (it reads stderr).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordEvent {
    /// Seconds left before the clock starts (3, 2, 1).
    Countdown(u32),
    /// The countdown is over and the recording clock has started.
    Started,
}

pub type EventSink = Arc<dyn Fn(RecordEvent) + Send + Sync>;

/// A display that can be recorded. `index` is what `Source::Display` takes (0 = main).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DisplayItem {
    pub index: usize,
    pub name: String,
    /// Global points.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub pixel_width: u64,
    pub pixel_height: u64,
    pub main: bool,
    /// Whether recordings of this display can be sharp (see `quality`).
    pub sharpness: quality::Sharpness,
}

/// A window that can be recorded (`Source::Window(id)`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowItem {
    pub id: u32,
    pub app: String,
    pub title: String,
    /// Global points.
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct SourceList {
    pub displays: Vec<DisplayItem>,
    pub windows: Vec<WindowItem>,
    /// Untitled windows left out because the capture library can only record titled ones.
    pub untitled_hidden: usize,
}

/// What macOS has granted to the app that launched us. Checking never shows a prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct Permissions {
    pub screen_recording: bool,
    pub accessibility: bool,
    pub input_monitoring: bool,
    /// The app the permissions belong to (the terminal or launcher), for the onboarding text.
    pub responsible_app: String,
}

impl Permissions {
    pub fn all_granted(&self) -> bool {
        self.screen_recording && self.accessibility && self.input_monitoring
    }
}

/// Whether macOS lets the app we run from use the microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MicAccess {
    Authorized,
    Denied,
    Restricted,
    NotDetermined,
}

/// The microphone permission right now. Never shows a prompt.
#[cfg(target_os = "macos")]
pub fn microphone_access() -> MicAccess {
    mic_macos::access()
}

/// Ask for the microphone permission if it has not been decided (macOS shows its prompt) and wait for the answer.
#[cfg(target_os = "macos")]
pub fn request_microphone_access() -> MicAccess {
    mic_macos::request_access()
}

#[cfg(not(target_os = "macos"))]
pub fn microphone_access() -> MicAccess {
    MicAccess::Denied
}

#[cfg(not(target_os = "macos"))]
pub fn request_microphone_access() -> MicAccess {
    MicAccess::Denied
}

/// What is under the pointer right now, by the Accessibility API (for `capturra hover-probe`).
#[cfg(target_os = "macos")]
pub fn hover_probe() -> String {
    hover_macos::probe_once()
}

#[cfg(not(target_os = "macos"))]
pub fn hover_probe() -> String {
    "the element under the pointer can only be read on macOS".to_string()
}

/// What the Accessibility API says about the keyboard focus right now (for `capturra focus-probe`).
#[cfg(target_os = "macos")]
pub fn focus_probe() -> String {
    focus_macos::probe_once()
}

#[cfg(not(target_os = "macos"))]
pub fn focus_probe() -> String {
    "the focus can only be read on macOS".to_string()
}

/// The system's pointing-hand cursor (real artwork, hotspot and scale), to save with a recording so the
/// renderer can show it over clickable things. macOS hands the standard cursors out only on the main thread
/// of a process with an app, so this is `None` anywhere else: call it from the main thread and pass the
/// result in `RecordOptions::hand_cursor`.
#[cfg(target_os = "macos")]
pub fn pointing_hand_sprite() -> Option<cursor::CursorImageData> {
    cursor_macos::standard_cursors().into_iter().find(|(name, _)| *name == "link").map(|(_, img)| img)
}

#[cfg(not(target_os = "macos"))]
pub fn pointing_hand_sprite() -> Option<cursor::CursorImageData> {
    None
}

pub struct RecordOptions {
    /// Called with countdown ticks and the start of the clock. `None` for the CLI.
    pub on_event: Option<EventSink>,
    pub source: geometry::Source,
    pub out_dir: PathBuf,
    /// Seconds counted down (printed to the terminal) before the recording clock starts. 0 = none.
    pub countdown_secs: u32,
    /// Record video without input events: skips the input-permission check and the zero-events
    /// guard. For verification runs; a recording made this way has no cursor or zoom data.
    pub allow_no_input: bool,
    /// Set before stopping when a pointer click did it (the menu-bar icon or a Stop button): that
    /// click is not part of the content, so the last click is dropped if it landed just before the
    /// stop. A stop from a keyboard shortcut or Ctrl+C leaves it false and keeps every click.
    pub drop_stop_click: Arc<AtomicBool>,
    /// Leave Capturra's own windows (the Studio with its recording bar and countdown, the popover and
    /// the menu-bar timer) out of a display or region recording. Turn it off to record Capturra itself,
    /// for example in a demo of the app. A window recording is unaffected: it shows only that window.
    pub hide_own_windows: bool,
    /// Also record system audio (everything the Mac plays except Capturra's own sound; macOS 13+,
    /// needs only the Screen Recording permission). It is placed on the shared clock and added to
    /// `recording.mp4` when the recording ends.
    pub system_audio: bool,
    /// Also record the default microphone (needs the Microphone permission). Mixed with the system audio
    /// into the video's one sound track, on the same clock.
    pub microphone: bool,
    /// The system hand pointer (see `pointing_hand_sprite`). With it, and with Accessibility, the recording
    /// notes when the pointer is over something clickable and carries the hand to show for it.
    pub hand_cursor: Option<cursor::CursorImageData>,
    pub fps: u32,
    /// Stop automatically after this long. `None` = until `stop` is set.
    pub duration: Option<Duration>,
    pub stop: Arc<AtomicBool>,
}

#[derive(Debug)]
pub struct RecordingPaths {
    pub video: PathBuf,
    pub events: PathBuf,
}

#[cfg(target_os = "macos")]
pub fn record(opts: RecordOptions) -> Result<RecordingPaths> {
    capture::record(opts)
}

#[cfg(not(target_os = "macos"))]
pub fn record(_opts: RecordOptions) -> Result<RecordingPaths> {
    anyhow::bail!("recording is only supported on macOS (use `capturra synth` to try the zoom pipeline elsewhere)")
}

/// `capturra record --check`: validate a source and report the frame size without recording.
#[cfg(target_os = "macos")]
pub fn check_source(source: geometry::Source, fps: u32) -> Result<String> {
    capture::check(source, fps)
}

#[cfg(not(target_os = "macos"))]
pub fn check_source(_source: geometry::Source, _fps: u32) -> Result<String> {
    anyhow::bail!("capture sources are only supported on macOS")
}

/// Displays and windows that can be recorded right now.
#[cfg(target_os = "macos")]
pub fn source_list() -> Result<SourceList> {
    capture::source_list()
}

#[cfg(not(target_os = "macos"))]
pub fn source_list() -> Result<SourceList> {
    anyhow::bail!("capture sources are only supported on macOS")
}

/// Current permission state (never prompts).
#[cfg(target_os = "macos")]
pub fn permissions() -> Permissions {
    capture::permissions()
}

#[cfg(not(target_os = "macos"))]
pub fn permissions() -> Permissions {
    Permissions::default()
}

/// `capturra record --region ... --guide`: save and open pictures showing where the region sits.
#[cfg(target_os = "macos")]
pub fn region_guide(source: geometry::Source, out_dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    capture::guide(source, out_dir)
}

#[cfg(not(target_os = "macos"))]
pub fn region_guide(_source: geometry::Source, _out_dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    anyhow::bail!("capture sources are only supported on macOS")
}

/// The text of `capturra record --list`: displays and windows that can be recorded.
#[cfg(target_os = "macos")]
pub fn list_sources() -> Result<String> {
    capture::list_sources()
}

#[cfg(not(target_os = "macos"))]
pub fn list_sources() -> Result<String> {
    anyhow::bail!("listing capture sources is only supported on macOS")
}

/// Verification probe for system cursor capture (macOS only).
#[cfg(target_os = "macos")]
pub fn cursor_probe(out: &std::path::Path, seconds: u64, init_app: bool) -> Result<()> {
    cursor_macos::probe(out, seconds, init_app)
}

#[cfg(not(target_os = "macos"))]
pub fn cursor_probe(_out: &std::path::Path, _seconds: u64, _init_app: bool) -> Result<()> {
    anyhow::bail!("cursor capture is only supported on macOS")
}

/// Real macOS standard cursors (name, image) for demos; empty off macOS.
#[cfg(target_os = "macos")]
pub fn standard_cursors() -> Vec<(&'static str, cursor::CursorImageData)> {
    cursor_macos::standard_cursors()
}

#[cfg(not(target_os = "macos"))]
pub fn standard_cursors() -> Vec<(&'static str, cursor::CursorImageData)> {
    Vec::new()
}

//! Capturra menu-bar recorder. A tray icon opens a small popover to pick what to record;
//! while recording, clicking the tray icon stops it. All recording logic lives in
//! `capturra-recorder`; this file is the shell around it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_edit;
mod backgrounds;
mod delete_project;
mod export_project;
mod hotkey;
mod list_projects;
mod load_project;
mod pick_recording;
mod projects;
mod region_picker;
mod rename_project;
mod save_project;
mod session;
mod settings;
mod source_preview;
mod studio_zoom;
mod video_frames;
mod waveform;

use capturra_recorder::{Permissions, RecordEvent, SourceList};
use serde::Serialize;
use session::{timestamp_now, Recorder, StartRequest};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, State, WebviewWindow, WindowEvent};

/// What the popover hears while a recording runs.
#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum Progress {
    Countdown { seconds: u32 },
    Recording,
    Saved { dir: String, video: String, events: String },
    Failed { message: String },
}

fn emit(app: &AppHandle, p: Progress) {
    let _ = app.emit("record", p);
}

fn set_tray_title(app: &AppHandle, title: Option<&str>) {
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_title(title);
    }
}

#[tauri::command]
fn get_sources() -> Result<SourceList, String> {
    capturra_recorder::source_list().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn get_permissions() -> Permissions {
    capturra_recorder::permissions()
}

/// Open the System Settings pane for one permission: "screen", "accessibility" or "input".
#[tauri::command]
fn open_privacy_settings(pane: String) -> Result<(), String> {
    let anchor = match pane.as_str() {
        "screen" => "Privacy_ScreenCapture",
        "accessibility" => "Privacy_Accessibility",
        "input" => "Privacy_ListenEvent",
        "microphone" => "Privacy_Microphone",
        other => return Err(format!("unknown pane {other:?}")),
    };
    std::process::Command::new("open")
        .arg(format!("x-apple.systempreferences:com.apple.preference.security?{anchor}"))
        .status()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// What macOS says about the microphone for the app we run from, and the app's name for the message.
#[derive(serde::Serialize)]
struct MicrophoneAccess {
    status: capturra_recorder::MicAccess,
    app: String,
}

/// The Microphone permission. With `request`, asks if it has not been decided yet (macOS shows its prompt
/// and this waits for the answer, off the main thread so the windows stay alive).
#[tauri::command]
async fn microphone_access(request: bool) -> Result<MicrophoneAccess, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let status = if request { capturra_recorder::request_microphone_access() } else { capturra_recorder::microphone_access() };
        MicrophoneAccess { status, app: capturra_recorder::permissions().responsible_app }
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    std::process::Command::new("open").arg("-R").arg(path).status().map(|_| ()).map_err(|e| e.to_string())
}

/// Open System Settings on the Displays page, where the resolution is chosen.
#[tauri::command]
fn open_display_settings() -> Result<(), String> {
    let status = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.Displays-Settings.extension")
        .status()
        .map_err(|e| format!("could not open System Settings: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("System Settings did not open".to_string())
    }
}

/// Save and open the region guide pictures (where the rectangle sits on the screen).
#[tauri::command]
fn show_region_guide(req: StartRequest) -> Result<(), String> {
    let source = req.source()?;
    let dir = std::env::temp_dir().join("capturra-guide");
    capturra_recorder::region_guide(source, &dir).map(|_| ()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn is_recording(rec: State<'_, Arc<Recorder>>) -> bool {
    rec.is_active()
}

#[tauri::command]
fn start_recording(app: AppHandle, req: StartRequest) -> Result<(), String> {
    begin_recording(&app, req)
}

/// Start a recording (from the popover, the Studio or the shortcut): check permissions, pick the
/// output folder, and run the session while reporting progress to the windows.
pub(crate) fn begin_recording(app: &AppHandle, mut req: StartRequest) -> Result<(), String> {
    let rec = app.state::<Arc<Recorder>>();
    // Whether Capturra's own windows appear in the video is a saved setting, for every way of starting.
    if req.show_capturra.is_none() {
        let saved = settings::config_path().map(|p| settings::load(&p).settings.show_capturra).unwrap_or(false);
        req.show_capturra = Some(saved);
    }
    // Likewise for system audio: an explicit choice from the caller wins, otherwise the saved setting.
    if req.system_audio.is_none() {
        let saved = settings::config_path().map(|p| settings::load(&p).settings.system_audio).unwrap_or(false);
        req.system_audio = Some(saved);
    }
    if req.microphone.is_none() {
        let saved = settings::config_path().map(|p| settings::load(&p).settings.microphone).unwrap_or(false);
        req.microphone = Some(saved);
    }
    let app = app.clone();
    let perms = capturra_recorder::permissions();
    if !perms.all_granted() {
        return Err("Capturra still needs permissions (see the list at the top)".to_string());
    }
    let out_dir = projects::recordings_root()?.join(timestamp_now());

    let sink_app = app.clone();
    let on_event: capturra_recorder::EventSink = Arc::new(move |e| match e {
        RecordEvent::Countdown(n) => {
            set_tray_title(&sink_app, Some(&n.to_string()));
            emit(&sink_app, Progress::Countdown { seconds: n });
        }
        RecordEvent::Started => {
            set_tray_title(&sink_app, Some("● 0:00"));
            emit(&sink_app, Progress::Recording);
            // A once-a-second elapsed timer in the menu bar until the recording ends.
            let tick_app = sink_app.clone();
            std::thread::spawn(move || {
                let started = std::time::Instant::now();
                let rec = tick_app.state::<Arc<Recorder>>();
                while rec.is_active() {
                    let s = started.elapsed().as_secs();
                    set_tray_title(&tick_app, Some(&format!("● {}:{:02}", s / 60, s % 60)));
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            });
        }
    });

    let done_app = app.clone();
    let done_dir = out_dir.clone();
    rec.inner().start(&req, out_dir, on_event, move |result| {
        set_tray_title(&done_app, None);
        match result {
            Ok(paths) => emit(
                &done_app,
                Progress::Saved {
                    dir: done_dir.display().to_string(),
                    video: paths.video.display().to_string(),
                    events: paths.events.display().to_string(),
                },
            ),
            Err(message) => show_failure(&done_app, message),
        }
    })
}

#[tauri::command]
fn stop_recording(rec: State<'_, Arc<Recorder>>) -> bool {
    rec.stop()
}

fn toggle_popover(window: &WebviewWindow, tray_rect: tauri::Rect) {
    if window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    // Centre the popover under the tray icon.
    if let (Ok(scale), Ok(size)) = (window.scale_factor(), window.outer_size()) {
        let pos = tray_rect.position.to_physical::<f64>(scale);
        let icon = tray_rect.size.to_physical::<f64>(scale);
        let x = (pos.x + icon.width / 2.0 - size.width as f64 / 2.0).max(0.0);
        let y = pos.y + icon.height;
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// Tell the popover a recording failed, and bring it back: it closes when it loses focus, so a
/// failure would otherwise go unseen.
pub(crate) fn show_failure(app: &AppHandle, message: String) {
    emit(app, Progress::Failed { message });
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Show the Studio window (created hidden at start-up) and bring it to the front.
fn open_studio(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("studio") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() {
    let recorder = Arc::new(Recorder::default());
    tauri::Builder::default()
        .manage(recorder)
        .manage(hotkey::HotkeyState::default())
        .manage(region_picker::PickerState::default())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            get_sources,
            get_permissions,
            open_privacy_settings,
            microphone_access,
            reveal_in_finder,
            open_display_settings,
            show_region_guide,
            is_recording,
            start_recording,
            stop_recording,
            list_projects::list_projects,
            load_project::load_project,
            load_project::build_zoom,
            save_project::save_project,
            delete_project::delete_project,
            pick_recording::pick_recording,
            rename_project::rename_project,
            region_picker::pick_region,
            source_preview::source_preview,
            video_frames::video_frames,
            waveform::audio_waveform,
            audio_edit::audio_preview,
            region_picker::picker_info,
            region_picker::picker_submit,
            region_picker::picker_cancel,
            settings::get_settings,
            settings::save_settings,
            settings::choose_recordings_dir,
            settings::open_recordings_folder,
            export_project::export_project,
            backgrounds::add_custom_background,
            backgrounds::list_custom_backgrounds,
            backgrounds::delete_custom_background,
        ])
        .setup(|app| {
            // A menu-bar app: no Dock icon, no app menu.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            // The system hand pointer, for showing the hand over clickable things in finished videos. This
            // runs on the main thread, the only place macOS hands out its standard cursors.
            let hand = capturra_recorder::pointing_hand_sprite();
            eprintln!("hand pointer artwork: {}", if hand.is_some() { "read" } else { "not available" });
            app.state::<Arc<Recorder>>().set_hand(hand);

            // The start/stop shortcut from the settings. A failure is shown in the popover, not fatal.
            let wanted = settings::config_path().map(|p| settings::load(&p).settings.hotkey).unwrap_or_default();
            if let Err(e) = hotkey::apply(app.handle(), &wanted) {
                eprintln!("shortcut: {e}");
            }

            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            // Left click = the recorder popover; right click = this menu.
            let menu = Menu::with_items(
                app,
                &[
                    &MenuItem::with_id(app, "studio", "Open Studio", true, None::<&str>)?,
                    &MenuItem::with_id(app, "quit", "Quit Capturra", true, None::<&str>)?,
                ],
            )?;
            TrayIconBuilder::with_id("main")
                .icon(icon)
                .icon_as_template(true)
                .tooltip("Capturra")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "studio" => open_studio(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                        let app = tray.app_handle();
                        let rec = app.state::<Arc<Recorder>>();
                        // While a recording runs, the icon is the stop button.
                        if rec.is_active() {
                            rec.stop();
                        } else if let Some(window) = app.get_webview_window("main") {
                            toggle_popover(&window, rect);
                        }
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // Click anywhere else and the popover goes away, like a native menu-bar popover.
            ("main", WindowEvent::Focused(false)) => {
                let _ = window.hide();
            }
            (label, WindowEvent::Destroyed) if region_picker::is_picker(label) => {
                region_picker::on_window_closed(window.app_handle());
            }
            // Closing the Studio hides it, so it opens again instantly from the tray menu.
            ("studio", WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Capturra");
}

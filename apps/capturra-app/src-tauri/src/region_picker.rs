//! The on-screen region picker. `pick_region` captures the chosen display with `screencapture`,
//! opens a borderless window (`pages/region.html`) that shows it dimmed, and returns the rectangle the
//! user drags, in points from the display's top-left: the numbers `--region` takes.
//!
//! The picture is a screenshot, not a transparent overlay, so no private macOS API is needed. The
//! window cannot cover the menu bar, so the page is told where it actually landed and lines the
//! screenshot up with real screen positions; the menu-bar strip itself cannot be picked.

use capturra_recorder::geometry::{fit_region, PtRect};
use serde::Serialize;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// Smallest region worth recording, in points.
pub const MIN_SIZE: f64 = 16.0;
const LABEL: &str = "region-picker";

/// A drag from `a` to `b` (display points, any direction, may leave the display) as a region the
/// recorder accepts: clamped to the display, whole even points, at least `MIN_SIZE`.
pub fn region_from_drag(a: (f64, f64), b: (f64, f64), display: (f64, f64)) -> Result<[f64; 4], String> {
    let clamp = |v: f64, max: f64| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
    let (ax, ay, bx, by) = (clamp(a.0, display.0), clamp(a.1, display.1), clamp(b.0, display.0), clamp(b.1, display.1));
    let (x, y, w, h) = (ax.min(bx), ay.min(by), (ax - bx).abs(), (ay - by).abs());
    if w < MIN_SIZE || h < MIN_SIZE {
        return Err(format!("that region is too small ({w:.0} x {h:.0} points); drag at least {MIN_SIZE:.0} x {MIN_SIZE:.0}"));
    }
    let r = fit_region(PtRect::new(x, y, w, h), display.0, display.1)?;
    Ok([r.x, r.y, r.w, r.h])
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding, for the screenshot's data URL.
pub fn base64(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

/// What the picker page needs to draw itself.
#[derive(Debug, Clone, Serialize)]
pub struct PickerInfo {
    /// The screenshot as a `data:image/png;base64,...` URL.
    pub image: String,
    /// The display's size in points.
    pub width: f64,
    pub height: f64,
    /// Where the page's top-left sits on the display, in points. Not 0 when the window could not
    /// cover the menu bar.
    pub offset_x: f64,
    pub offset_y: f64,
}

struct Session {
    image: String,
    display: (f64, f64, f64, f64), // global x, y, width, height in points
    done: Sender<Option<[f64; 4]>>,
}

#[derive(Default)]
pub struct PickerState(Mutex<Option<Session>>);

fn capture(bounds: (f64, f64, f64, f64)) -> Result<String, String> {
    let path = std::env::temp_dir().join(format!("capturra-picker-{}.png", std::process::id()));
    let status = std::process::Command::new("screencapture")
        .arg("-x")
        .arg(format!("-R{},{},{},{}", bounds.0, bounds.1, bounds.2, bounds.3))
        .arg(&path)
        .status()
        .map_err(|e| format!("could not run screencapture: {e}"))?;
    if !status.success() || !path.exists() {
        return Err("screencapture failed. Screen Recording permission may be missing (System Settings > Privacy & Security).".to_string());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("reading the screenshot: {e}"));
    let _ = std::fs::remove_file(&path);
    Ok(format!("data:image/png;base64,{}", base64(&bytes?)))
}

/// Let the user drag a region on display `display`. `Ok(None)` if they cancelled.
#[tauri::command]
pub async fn pick_region(app: AppHandle, window: WebviewWindow, display: usize) -> Result<Option<[f64; 4]>, String> {
    let sources = capturra_recorder::source_list().map_err(|e| format!("{e:#}"))?;
    let d = sources.displays.iter().find(|d| d.index == display).ok_or_else(|| format!("display {display} not found"))?;
    let bounds = (d.x, d.y, d.width, d.height);

    // Hide the window that asked (the popover or the Studio) so it is not in the screenshot, and
    // give the screen a moment to redraw.
    let _ = window.hide();
    std::thread::sleep(std::time::Duration::from_millis(250));
    let shot = tauri::async_runtime::spawn_blocking(move || capture(bounds)).await.map_err(|e| e.to_string());
    let image = match shot.and_then(|r| r) {
        Ok(image) => image,
        Err(e) => {
            let _ = window.show();
            return Err(e);
        }
    };

    let (tx, rx) = channel();
    {
        let state = app.state::<PickerState>();
        let mut slot = state.0.lock().unwrap();
        if slot.is_some() {
            drop(slot);
            let _ = window.show();
            return Err("a region picker is already open".to_string());
        }
        *slot = Some(Session { image, display: bounds, done: tx });
    }
    let built = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::App("pages/region.html".into()))
        .title("Select a region")
        .position(bounds.0, bounds.1)
        .inner_size(bounds.2, bounds.3)
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(true)
        .build();
    let picker = match built {
        Ok(w) => w,
        Err(e) => {
            *app.state::<PickerState>().0.lock().unwrap() = None;
            let _ = window.show();
            return Err(format!("could not open the region picker: {e}"));
        }
    };

    let result = tauri::async_runtime::spawn_blocking(move || rx.recv().unwrap_or(None)).await.map_err(|e| e.to_string())?;
    *app.state::<PickerState>().0.lock().unwrap() = None;
    let _ = picker.destroy();
    let _ = window.show();
    let _ = window.set_focus();
    Ok(result)
}

/// The picker page asks for its picture and where its window landed.
#[tauri::command]
pub fn picker_info(window: WebviewWindow, state: tauri::State<'_, PickerState>) -> Result<PickerInfo, String> {
    let slot = state.0.lock().unwrap();
    let s = slot.as_ref().ok_or("no region picker is open")?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let inner = window.inner_position().map_err(|e| e.to_string())?;
    let (gx, gy) = (inner.x as f64 / scale, inner.y as f64 / scale);
    Ok(PickerInfo {
        image: s.image.clone(),
        width: s.display.2,
        height: s.display.3,
        offset_x: gx - s.display.0,
        offset_y: gy - s.display.1,
    })
}

/// The page reports the two corners of the drag. A region that is too small is an error the page
/// can show; the picker stays open.
#[tauri::command]
pub fn picker_submit(a: [f64; 2], b: [f64; 2], state: tauri::State<'_, PickerState>) -> Result<[f64; 4], String> {
    let slot = state.0.lock().unwrap();
    let s = slot.as_ref().ok_or("no region picker is open")?;
    let region = region_from_drag((a[0], a[1]), (b[0], b[1]), (s.display.2, s.display.3))?;
    let _ = s.done.send(Some(region));
    Ok(region)
}

#[tauri::command]
pub fn picker_cancel(state: tauri::State<'_, PickerState>) {
    if let Some(s) = state.0.lock().unwrap().as_ref() {
        let _ = s.done.send(None);
    }
}

/// Closing the picker window any other way counts as cancelling.
pub fn on_window_closed(app: &AppHandle) {
    if let Some(s) = app.state::<PickerState>().0.lock().unwrap().as_ref() {
        let _ = s.done.send(None);
    }
}

pub fn is_picker(label: &str) -> bool {
    label == LABEL
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: (f64, f64) = (2048.0, 1280.0);

    #[test]
    fn a_drag_in_any_direction_gives_the_same_region() {
        let r = region_from_drag((100.0, 200.0), (500.0, 600.0), D).unwrap();
        assert_eq!(r, [100.0, 200.0, 400.0, 400.0]);
        assert_eq!(region_from_drag((500.0, 600.0), (100.0, 200.0), D).unwrap(), r);
        assert_eq!(region_from_drag((500.0, 200.0), (100.0, 600.0), D).unwrap(), r);
    }

    #[test]
    fn odd_sizes_snap_to_whole_even_points() {
        let r = region_from_drag((10.4, 20.6), (511.2, 331.9), D).unwrap();
        assert!(r.iter().all(|v| v.fract() == 0.0), "{r:?}");
        assert!(r[2] % 2.0 == 0.0 && r[3] % 2.0 == 0.0, "{r:?}");
    }

    #[test]
    fn a_drag_past_the_display_edge_is_clamped_not_refused() {
        let r = region_from_drag((1800.0, 1000.0), (5000.0, 9000.0), D).unwrap();
        assert_eq!(r, [1800.0, 1000.0, 248.0, 280.0]);
        let r = region_from_drag((-300.0, -50.0), (400.0, 300.0), D).unwrap();
        assert_eq!(r, [0.0, 0.0, 400.0, 300.0]);
    }

    #[test]
    fn tiny_or_nonsense_drags_are_refused_with_a_message() {
        assert!(region_from_drag((10.0, 10.0), (12.0, 12.0), D).unwrap_err().contains("too small"));
        assert!(region_from_drag((10.0, 10.0), (500.0, 20.0), D).unwrap_err().contains("too small"));
        assert!(region_from_drag((f64::NAN, 0.0), (f64::INFINITY, 10.0), D).is_err());
    }

    #[test]
    fn every_picked_region_is_one_the_recorder_accepts_unchanged() {
        for (a, b) in [
            ((13.0, 7.0), (999.0, 777.0)),
            ((2047.0, 1279.0), (1000.0, 600.0)),
            ((0.0, 0.0), (2048.0, 1280.0)),
            ((101.5, 99.5), (333.3, 444.4)),
        ] {
            let [x, y, w, h] = region_from_drag(a, b, D).unwrap();
            let fit = fit_region(PtRect::new(x, y, w, h), D.0, D.1).unwrap();
            assert_eq!([fit.x, fit.y, fit.w, fit.h], [x, y, w, h]);
            assert!(w >= MIN_SIZE && h >= MIN_SIZE);
        }
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd, 0x00]), "//79AA==");
    }
}

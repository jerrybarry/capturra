use cocoa::appkit::NSScreen;
use cocoa::base::{id, nil};
use cocoa::foundation::NSString;
use core_graphics_helmer_fork::display::{CGDirectDisplayID, CGDisplay, CGMainDisplayID};
use core_graphics_helmer_fork::window::CGWindowID;
use objc::{msg_send, sel, sel_impl};
use screencapturekit::sc_shareable_content::SCShareableContent;

use super::{Display, Target};

fn get_display_name(display_id: CGDirectDisplayID) -> String {
    unsafe {
        // Get all screens
        let screens: id = NSScreen::screens(nil);
        let count: u64 = msg_send![screens, count];

        for i in 0..count {
            let screen: id = msg_send![screens, objectAtIndex: i];
            let device_description: id = msg_send![screen, deviceDescription];
            let display_id_number: id = msg_send![device_description, objectForKey: NSString::alloc(nil).init_str("NSScreenNumber")];
            let display_id_number: u32 = msg_send![display_id_number, unsignedIntValue];

            if display_id_number == display_id {
                let localized_name: id = msg_send![screen, localizedName];
                let name: *const i8 = msg_send![localized_name, UTF8String];
                return std::ffi::CStr::from_ptr(name)
                    .to_string_lossy()
                    .into_owned();
            }
        }

        format!("Unknown Display {}", display_id)
    }
}

pub fn get_all_targets() -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();

    let content = SCShareableContent::current();

    // Add displays to targets
    for display in content.displays {
        let id: CGDirectDisplayID = display.display_id;
        let raw_handle = CGDisplay::new(id);
        let title = get_display_name(id);

        let target = Target::Display(super::Display {
            id,
            title,
            raw_handle,
        });

        targets.push(target);
    }

    // Add windows to targets
    for window in content.windows {
        if window.title.is_some() {
            let id = window.window_id;
            let title = window.title.expect("Window title not found");
            let raw_handle: CGWindowID = id;

            let target = Target::Window(super::Window {
                id,
                title,
                raw_handle,
            });
            targets.push(target);
        }
    }

    targets
}

pub fn get_main_display() -> Display {
    let id = unsafe { CGMainDisplayID() };
    let title = get_display_name(id);

    Display {
        id,
        title,
        raw_handle: CGDisplay::new(id),
    }
}

// --- Capturra patch (see CAPTURRA-PATCH.md) -------------------------------------------------
// Upstream sized a window by asking NSApp for an NSWindow with the window's number. That only
// finds windows of THIS process, so every other app's window came back as 0 x 0 with scale 0.
// Here the window's frame comes from ScreenCaptureKit (the same source the capture itself uses)
// and the scale from the display the window mostly sits on. The Display arms are untouched.

use screencapturekit_sys::os_types::geometry::CGRect as SckRect;
use screencapturekit_sys::shareable_content::UnsafeSCShareableContent;

/// (frame in global points, scale of the display holding most of it) for a window, or None.
fn sck_window_frame_and_scale(window_id: u32) -> Option<(SckRect, f64)> {
    let content = UnsafeSCShareableContent::get().ok()?;
    let frame = content
        .windows()
        .iter()
        .find(|w| w.get_window_id() == window_id)
        .map(|w| w.get_frame())?;
    let overlap = |d: &SckRect| -> f64 {
        let w = (frame.origin.x + frame.size.width).min(d.origin.x + d.size.width) - frame.origin.x.max(d.origin.x);
        let h = (frame.origin.y + frame.size.height).min(d.origin.y + d.size.height) - frame.origin.y.max(d.origin.y);
        w.max(0.0) * h.max(0.0)
    };
    let display = content
        .displays()
        .iter()
        .map(|d| (d.get_display_id(), overlap(&d.get_frame())))
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| id)?;
    let mode = CGDisplay::new(display).display_mode()?;
    let scale = mode.pixel_width() as f64 / mode.width() as f64;
    Some((frame, scale))
}

pub fn get_scale_factor(target: &Target) -> f64 {
    match target {
        Target::Window(window) => sck_window_frame_and_scale(window.id).map(|(_, s)| s).unwrap_or(1.0),
        Target::Display(display) => {
            let mode = display.raw_handle.display_mode().unwrap();
            (mode.pixel_width() / mode.width()) as f64
        }
    }
}

pub fn get_target_dimensions(target: &Target) -> (u64, u64) {
    match target {
        Target::Window(window) => sck_window_frame_and_scale(window.id)
            .map(|(f, _)| (f.size.width as u64, f.size.height as u64))
            .unwrap_or((0, 0)),
        Target::Display(display) => {
            let mode = display.raw_handle.display_mode().unwrap();
            (mode.width(), mode.height())
        }
    }
}

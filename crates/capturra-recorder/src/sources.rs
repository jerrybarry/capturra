//! What can be recorded on this Mac: displays and windows, with their geometry in global points.
//! Uses CoreGraphics only (no `scap`), so the geometry that drives the coordinate mapping comes
//! from the same source as the input events (CGEvent coordinates).

use crate::geometry::PtRect;
use crate::quality::{classify, ModeInfo, Sharpness};
use anyhow::{anyhow, bail, Result};
use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;
use core_graphics::display::CGDisplay;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};
use std::ffi::c_void;

#[derive(Debug, Clone)]
pub struct DisplayGeom {
    pub id: u32,
    /// Global points: the origin is relative to the main display's top-left.
    pub bounds: PtRect,
    pub pixels: (u64, u64),
    pub is_main: bool,
    pub sharpness: Sharpness,
}

#[derive(Debug, Clone)]
pub struct WindowGeom {
    pub id: u32,
    pub owner: String,
    /// Process id of the app that owns the window.
    pub pid: i64,
    pub title: String,
    /// Global points.
    pub bounds: PtRect,
    pub layer: i64,
    pub on_screen: bool,
}

/// Every mode a display offers, as (size in points, size in pixels). The `core-graphics` crate's own
/// `CGDisplayMode::all_display_modes` takes ownership of each element of the array while the array also
/// releases it, which frees every mode twice and crashes (seen as a SIGSEGV in tests). So the array is
/// read directly with the plain CoreGraphics calls, which only look at the modes and own nothing.
fn offered_modes(display: u32) -> Vec<ModeInfo> {
    // macOS hides the Retina (2x) variants of each mode unless this flag is set, which would make every
    // display look like it has nothing sharper to offer.
    let options = CFDictionary::from_CFType_pairs(&[(
        unsafe { CFString::wrap_under_get_rule(kCGDisplayShowDuplicateLowResolutionModes) },
        CFBoolean::true_value(),
    )]);
    unsafe {
        let array = CGDisplayCopyAllDisplayModes(display, options.as_concrete_TypeRef());
        if array.is_null() {
            return Vec::new();
        }
        let modes = (0..CFArrayGetCount(array))
            .map(|i| {
                let mode = CFArrayGetValueAtIndex(array, i);
                ModeInfo { width: CGDisplayModeGetWidth(mode) as u64, pixel_width: CGDisplayModeGetPixelWidth(mode) as u64 }
            })
            .collect();
        CFRelease(array as *const c_void); // we created (copied) the array, so we release it
        modes
    }
}

/// Judge a display from its current mode and the modes it offers.
fn display_sharpness(d: &CGDisplay) -> Sharpness {
    let Some(current) = d.display_mode().map(|m| ModeInfo { width: m.width(), pixel_width: m.pixel_width() }) else {
        return Sharpness::Standard;
    };
    let all = offered_modes(d.id);
    if std::env::var_os("CAPTURRA_DEBUG_MODES").is_some() {
        eprintln!("display {} current mode: {current:?} ({:.2} px/pt)", d.id, current.scale());
        for m in &all {
            eprintln!("    offers {} pt -> {} px ({:.2} px/pt)", m.width, m.pixel_width, m.scale());
        }
    }
    classify(current, &all)
}

/// Displays in `--list` order: the main display first (index 0), then the rest by id.
pub fn displays() -> Result<Vec<DisplayGeom>> {
    let ids = CGDisplay::active_displays().map_err(|e| anyhow!("could not list displays (CGError {e})"))?;
    let mut out: Vec<DisplayGeom> = ids
        .into_iter()
        .map(|id| {
            let d = CGDisplay::new(id);
            let b = d.bounds();
            let sharpness = display_sharpness(&d);
            DisplayGeom {
                id,
                bounds: PtRect::new(b.origin.x, b.origin.y, b.size.width, b.size.height),
                pixels: (d.pixels_wide(), d.pixels_high()),
                is_main: d.is_main(),
                sharpness,
            }
        })
        .collect();
    out.sort_by_key(|d| (!d.is_main, d.id));
    if out.is_empty() {
        bail!("no active displays found");
    }
    Ok(out)
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    /// The option key that makes `CGDisplayCopyAllDisplayModes` include the Retina variants of each mode.
    static kCGDisplayShowDuplicateLowResolutionModes: core_foundation::string::CFStringRef;
    fn CGDisplayCopyAllDisplayModes(display: u32, options: core_foundation::dictionary::CFDictionaryRef) -> CFArrayRef;
    fn CGDisplayModeGetWidth(mode: *const c_void) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: *const c_void) -> usize;
    /// Returns a retained CFArray of CFDictionary (toll-free bridged to NSArray/NSDictionary).
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const c_void;
}
const LIST_ON_SCREEN_ONLY: u32 = 1 << 0;
const LIST_INCLUDING_WINDOW: u32 = 1 << 3;
const LIST_EXCLUDE_DESKTOP_ELEMENTS: u32 = 1 << 4;

type Dict = NSDictionary<NSString, AnyObject>;

fn num(d: &Dict, key: &str) -> Option<f64> {
    d.objectForKey(&NSString::from_str(key))?.downcast_ref::<NSNumber>().map(|n| n.as_f64())
}

fn text(d: &Dict, key: &str) -> Option<String> {
    d.objectForKey(&NSString::from_str(key))?.downcast_ref::<NSString>().map(|s| s.to_string())
}

fn window_list(option: u32, relative_to: u32) -> Result<Vec<WindowGeom>> {
    // SAFETY: CGWindowListCopyWindowInfo follows the CF "Copy" rule (+1), which is what
    // Retained::from_raw takes ownership of; the array holds CFDictionary = NSDictionary.
    let raw = unsafe { CGWindowListCopyWindowInfo(option, relative_to) };
    let array: Retained<NSArray<Dict>> =
        unsafe { Retained::from_raw(raw as *mut NSArray<Dict>) }.ok_or_else(|| anyhow!("CGWindowListCopyWindowInfo returned NULL"))?;
    let mut out = Vec::new();
    for d in array.iter() {
        let (Some(id), Some(bounds)) = (num(&d, "kCGWindowNumber"), d.objectForKey(&NSString::from_str("kCGWindowBounds"))) else {
            continue;
        };
        let Some(b) = bounds.downcast_ref::<NSDictionary>() else { continue };
        // SAFETY: the key/value type parameters of NSDictionary are phantom; same layout.
        let b: &Dict = unsafe { &*(b as *const NSDictionary as *const Dict) };
        let (Some(x), Some(y), Some(w), Some(h)) = (num(b, "X"), num(b, "Y"), num(b, "Width"), num(b, "Height")) else {
            continue;
        };
        out.push(WindowGeom {
            id: id as u32,
            owner: text(&d, "kCGWindowOwnerName").unwrap_or_default(),
            pid: num(&d, "kCGWindowOwnerPID").unwrap_or(-1.0) as i64,
            title: text(&d, "kCGWindowName").unwrap_or_default(),
            bounds: PtRect::new(x, y, w, h),
            layer: num(&d, "kCGWindowLayer").unwrap_or(0.0) as i64,
            on_screen: num(&d, "kCGWindowIsOnscreen").unwrap_or(0.0) != 0.0,
        });
    }
    Ok(out)
}

/// Normal application windows currently on screen (layer 0, not tiny), front to back.
pub fn windows() -> Result<Vec<WindowGeom>> {
    let mut v = window_list(LIST_ON_SCREEN_ONLY | LIST_EXCLUDE_DESKTOP_ELEMENTS, 0)?;
    v.retain(|w| w.layer == 0 && w.bounds.w >= 50.0 && w.bounds.h >= 50.0);
    Ok(v)
}

/// The ids in `list` that belong to process `pid`.
pub fn ids_owned_by(list: &[WindowGeom], pid: i64) -> Vec<u32> {
    list.iter().filter(|w| w.pid == pid).map(|w| w.id).collect()
}

/// Every window of THIS app that is on screen right now, of any layer and with or without a title:
/// the Studio (which holds the countdown and the recording bar), the popover, and the menu-bar item
/// with its elapsed timer. These are left out of display and region recordings, so Capturra's own
/// controls never appear in the video.
pub fn own_window_ids() -> Result<Vec<u32>> {
    Ok(ids_owned_by(&window_list(LIST_ON_SCREEN_ONLY, 0)?, std::process::id() as i64))
}

/// Current bounds of one window in global points; `None` if it no longer exists (closed).
pub fn window_geom(id: u32) -> Option<WindowGeom> {
    window_list(LIST_INCLUDING_WINDOW, id).ok()?.into_iter().find(|w| w.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(id: u32, pid: i64, layer: i64) -> WindowGeom {
        WindowGeom {
            id,
            owner: String::new(),
            pid,
            title: String::new(),
            bounds: PtRect::new(0.0, 0.0, 10.0, 10.0),
            layer,
            on_screen: true,
        }
    }

    #[test]
    fn only_windows_of_the_given_process_are_picked_whatever_their_layer_or_title() {
        // a normal window (layer 0), the menu-bar item (layer 25, untitled) and another app's window
        let list = [geom(1, 500, 0), geom(2, 777, 0), geom(3, 500, 25), geom(4, 888, 25)];
        assert_eq!(ids_owned_by(&list, 500), vec![1, 3]);
        assert_eq!(ids_owned_by(&list, 999), Vec::<u32>::new());
        assert_eq!(ids_owned_by(&[], 500), Vec::<u32>::new());
    }

    #[test]
    fn this_process_can_list_its_own_windows_without_error() {
        // a test binary has no windows; the point is that the call works and returns only our own
        let own = own_window_ids().unwrap();
        let all = window_list(LIST_ON_SCREEN_ONLY, 0).unwrap();
        assert!(own.iter().all(|id| all.iter().any(|w| w.id == *id && w.pid == std::process::id() as i64)));
    }

    #[test]
    fn displays_list_the_main_display_first_at_the_global_origin() {
        let d = displays().unwrap();
        assert!(d[0].is_main, "index 0 must be the main display");
        assert_eq!(d.iter().filter(|x| x.is_main).count(), 1);
        // global event coordinates are relative to the main display's top-left corner
        assert_eq!((d[0].bounds.x, d[0].bounds.y), (0.0, 0.0));
        assert!(d.iter().all(|x| x.bounds.w > 0.0 && x.bounds.h > 0.0 && x.pixels.0 > 0));
    }

    /// Listing a display's modes over and over must not crash. (The crate's own helper double-freed every
    /// mode, which showed up as a SIGSEGV after a few calls.)
    #[test]
    fn display_modes_can_be_listed_many_times_and_include_the_current_one() {
        let id = displays().unwrap()[0].id;
        for _ in 0..300 {
            let modes = offered_modes(id);
            assert!(!modes.is_empty(), "a display always offers at least one mode");
            assert!(modes.iter().all(|m| m.width > 0 && m.pixel_width >= m.width), "a mode has at least one pixel per point");
        }
        let current = CGDisplay::new(id).display_mode().map(|m| m.width()).unwrap();
        assert!(offered_modes(id).iter().any(|m| m.width == current), "the current mode's width is among the offered ones");
        // and the verdict is stable, whatever it is
        let first = displays().unwrap()[0].sharpness;
        for _ in 0..50 {
            assert_eq!(displays().unwrap()[0].sharpness, first);
        }
    }

    #[test]
    fn windows_are_normal_application_windows_and_unknown_ids_are_none() {
        for w in windows().unwrap() {
            assert_eq!(w.layer, 0);
            assert!(w.bounds.w >= 50.0 && w.bounds.h >= 50.0);
        }
        // window ids are never 0 or u32::MAX in practice
        assert!(window_geom(u32::MAX).is_none());
    }

    #[test]
    fn a_listed_window_can_be_looked_up_again_with_the_same_bounds_shape() {
        if let Some(w) = windows().unwrap().into_iter().next() {
            let again = window_geom(w.id).expect("a window just listed is found by id");
            assert_eq!(again.id, w.id);
            assert!(again.bounds.w > 0.0);
        }
    }
}

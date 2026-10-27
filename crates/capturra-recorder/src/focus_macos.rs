//! The macOS focus reader: the Accessibility API's system-wide focused element, kept only if it is a
//! text field, as its frame in global points. This is the only file that touches the AX API. It needs the
//! Accessibility permission the input listener needs anyway.

use crate::focus::{FocusReading, FocusSource};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;

type AXUIElementRef = *const c_void;
type AXError = i32;

#[repr(C)]
#[derive(Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Default)]
struct CGSize {
    width: f64,
    height: f64,
}

const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
const AX_SUCCESS: AXError = 0;
const AX_API_DISABLED: AXError = -25211;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(element: AXUIElementRef, attribute: CFStringRef, value: *mut CFTypeRef) -> AXError;
    fn AXValueGetValue(value: CFTypeRef, the_type: u32, ptr: *mut c_void) -> u8;
}

/// Roles that mean "the user is typing here". (A web page or a whole editor is not one of these, and a
/// field that fills most of the screen is ignored later anyway.)
const TEXT_ROLES: [&str; 3] = ["AXTextField", "AXTextArea", "AXComboBox"];

pub struct AxFocusSource {
    system: AXUIElementRef,
}

// the system-wide element is a plain CF object used from the one sampling thread
unsafe impl Send for AxFocusSource {}

impl AxFocusSource {
    pub fn new() -> Self {
        Self { system: unsafe { AXUIElementCreateSystemWide() } }
    }
}

impl Drop for AxFocusSource {
    fn drop(&mut self) {
        if !self.system.is_null() {
            unsafe { CFRelease(self.system) };
        }
    }
}

/// The process id of the app in front, from the workspace.
fn frontmost_pid() -> Option<i32> {
    use objc2_app_kit::NSWorkspace;
    NSWorkspace::sharedWorkspace().frontmostApplication().map(|app| app.processIdentifier())
}

/// The element with the keyboard focus. The system-wide element is asked first; some contexts answer
/// "cannot complete" to it, so the frontmost app is asked directly as well.
fn focused_element(system: AXUIElementRef) -> Result<CFTypeRef, AXError> {
    match attribute(system, "AXFocusedUIElement") {
        Ok(e) => Ok(e),
        Err(AX_API_DISABLED) => Err(AX_API_DISABLED),
        Err(first) => {
            let Some(pid) = frontmost_pid() else { return Err(first) };
            let app = unsafe { AXUIElementCreateApplication(pid) };
            if app.is_null() {
                return Err(first);
            }
            let found = attribute(app, "AXFocusedUIElement");
            unsafe { CFRelease(app) };
            found
        }
    }
}

/// One attribute of an element, retained; the caller releases it.
fn attribute(element: AXUIElementRef, name: &str) -> Result<CFTypeRef, AXError> {
    let key = CFString::new(name);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(element, key.as_concrete_TypeRef(), &mut out) };
    if err == AX_SUCCESS && !out.is_null() {
        Ok(out)
    } else {
        Err(err)
    }
}

fn role_of(element: AXUIElementRef) -> Option<String> {
    let value = attribute(element, "AXRole").ok()?;
    let role = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) }.to_string();
    Some(role)
}

fn frame_of(element: AXUIElementRef) -> Option<(f64, f64, f64, f64)> {
    let pos = attribute(element, "AXPosition").ok()?;
    let size = attribute(element, "AXSize");
    let (mut p, mut s) = (CGPoint::default(), CGSize::default());
    let got_pos = unsafe { AXValueGetValue(pos, AX_VALUE_CGPOINT, &mut p as *mut _ as *mut c_void) } != 0;
    unsafe { CFRelease(pos) };
    let size = size.ok()?;
    let got_size = unsafe { AXValueGetValue(size, AX_VALUE_CGSIZE, &mut s as *mut _ as *mut c_void) } != 0;
    unsafe { CFRelease(size) };
    (got_pos && got_size && s.width > 0.0 && s.height > 0.0).then_some((p.x, p.y, s.width, s.height))
}

impl FocusSource for AxFocusSource {
    fn read(&mut self) -> FocusReading {
        if !crate::ax::trusted() {
            return FocusReading::Unavailable;
        }
        let focused = match focused_element(self.system) {
            Ok(e) => e,
            Err(AX_API_DISABLED) => return FocusReading::Unavailable,
            Err(_) => return FocusReading::NoField, // nothing is focused, or the app does not say
        };
        let reading = match role_of(focused) {
            Some(role) if TEXT_ROLES.contains(&role.as_str()) => match frame_of(focused) {
                Some((x, y, w, h)) => FocusReading::Field { x, y, w, h },
                None => FocusReading::NoField,
            },
            _ => FocusReading::NoField,
        };
        unsafe { CFRelease(focused) };
        reading
    }
}

/// `capturra focus-probe`: what the reader sees right now, with the role, for checking it on a real Mac.
pub fn probe_once() -> String {
    if !crate::ax::trusted() {
        return "Accessibility is not granted to this app, so the focus cannot be read".to_string();
    }
    let system = unsafe { AXUIElementCreateSystemWide() };
    let text = match focused_element(system) {
        Err(e) => format!("no focused element (AX error {e})"),
        Ok(focused) => {
            let role = role_of(focused).unwrap_or_else(|| "?".to_string());
            let frame = frame_of(focused);
            unsafe { CFRelease(focused) };
            match frame {
                Some((x, y, w, h)) => format!(
                    "{role}: x={x:.0} y={y:.0} w={w:.0} h={h:.0}{}",
                    if TEXT_ROLES.contains(&role.as_str()) { "  <- text field" } else { "" }
                ),
                None => format!("{role}: no frame"),
            }
        }
    };
    unsafe { CFRelease(system) };
    text
}

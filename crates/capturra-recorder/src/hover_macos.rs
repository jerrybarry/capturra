//! The macOS hover reader: the Accessibility element under the pointer, and whether it (or something it sits
//! in, a couple of levels up) is a button, link, tab or menu item. The only file that does this hit test.
//! It needs the Accessibility permission the input listener needs anyway.

use crate::hover::{HoverReading, HoverSource};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use std::ffi::c_void;

type AXUIElementRef = *const c_void;
type AXError = i32;

const AX_SUCCESS: AXError = 0;
const AX_API_DISABLED: AXError = -25211;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCopyElementAtPosition(application: AXUIElementRef, x: f32, y: f32, element: *mut AXUIElementRef) -> AXError;
    fn AXUIElementCopyAttributeValue(element: AXUIElementRef, attribute: CFStringRef, value: *mut CFTypeRef) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, seconds: f32) -> AXError;
}

/// Roles whose hover shows the pointing hand in this app: things you press.
const CLICKABLE_ROLES: [&str; 11] = [
    "AXButton",
    "AXLink",
    "AXMenuItem",
    "AXMenuBarItem",
    "AXMenuButton",
    "AXPopUpButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXTab",
    "AXDisclosureTriangle",
    "AXToolbarButton",
];
/// A link's text or a button's icon is a child of the button: look this many levels up too.
const LEVELS_UP: usize = 3;
/// A slow app must not stall the sampler.
const AX_TIMEOUT_SECS: f32 = 0.15;
/// A pointer that has not moved is asked again only this often (a menu may have opened under it).
const RECHECK_EVERY: u32 = 8;

pub struct AxHoverSource {
    system: AXUIElementRef,
    source: Option<CGEventSource>,
    last_point: Option<(f64, f64)>,
    last: HoverReading,
    still: u32,
}

// the system-wide element is a plain CF object used from the one sampling thread
unsafe impl Send for AxHoverSource {}

impl AxHoverSource {
    pub fn new() -> Self {
        let system = unsafe { AXUIElementCreateSystemWide() };
        unsafe { AXUIElementSetMessagingTimeout(system, AX_TIMEOUT_SECS) };
        Self {
            system,
            source: CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok(),
            last_point: None,
            last: HoverReading::NotClickable,
            still: 0,
        }
    }
}

impl Drop for AxHoverSource {
    fn drop(&mut self) {
        if !self.system.is_null() {
            unsafe { CFRelease(self.system) };
        }
    }
}

/// The pointer's position in global points (origin top-left of the main display, like the AX API).
pub fn pointer_position(source: &CGEventSource) -> Option<(f64, f64)> {
    CGEvent::new(source.clone()).ok().map(|e| {
        let p = e.location();
        (p.x, p.y)
    })
}

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
    Some(unsafe { CFString::wrap_under_create_rule(value as CFStringRef) }.to_string())
}

/// The roles of `element` and the `LEVELS_UP` elements above it, nearest first. `element` is not released.
fn role_chain(element: AXUIElementRef) -> Vec<String> {
    let mut roles = Vec::new();
    if let Some(r) = role_of(element) {
        roles.push(r);
    }
    let mut current: Option<CFTypeRef> = None; // a parent we own and must release
    let mut at = element;
    for _ in 0..LEVELS_UP {
        let Ok(parent) = attribute(at, "AXParent") else { break };
        if let Some(prev) = current.replace(parent) {
            unsafe { CFRelease(prev) };
        }
        at = parent;
        match role_of(at) {
            Some(r) if r == "AXWindow" || r == "AXApplication" => break,
            Some(r) => roles.push(r),
            None => break,
        }
    }
    if let Some(prev) = current {
        unsafe { CFRelease(prev) };
    }
    roles
}

fn clickable(roles: &[String]) -> bool {
    roles.iter().any(|r| CLICKABLE_ROLES.contains(&r.as_str()))
}

/// The chain of roles under the point, or the AX error.
fn roles_at(system: AXUIElementRef, x: f64, y: f64) -> Result<Vec<String>, AXError> {
    let mut element: AXUIElementRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyElementAtPosition(system, x as f32, y as f32, &mut element) };
    if err != AX_SUCCESS || element.is_null() {
        return Err(err);
    }
    let roles = role_chain(element);
    unsafe { CFRelease(element) };
    Ok(roles)
}

impl HoverSource for AxHoverSource {
    fn read(&mut self) -> HoverReading {
        if !crate::ax::trusted() {
            return HoverReading::Unavailable;
        }
        let Some(point) = self.source.as_ref().and_then(pointer_position) else { return self.last };
        if self.last_point == Some(point) {
            self.still += 1;
            if !self.still.is_multiple_of(RECHECK_EVERY) {
                return self.last;
            }
        } else {
            self.still = 0;
            self.last_point = Some(point);
        }
        self.last = match roles_at(self.system, point.0, point.1) {
            Ok(roles) if clickable(&roles) => HoverReading::Clickable,
            Ok(_) => HoverReading::NotClickable,
            Err(AX_API_DISABLED) => HoverReading::Unavailable,
            Err(_) => HoverReading::NotClickable, // nothing there, or the app does not say
        };
        self.last
    }
}

/// `capturra hover-probe`: what is under the pointer right now, for checking on a real Mac.
pub fn probe_once() -> String {
    if !crate::ax::trusted() {
        return "Accessibility is not granted to this app, so the element under the pointer cannot be read".to_string();
    }
    let Some(source) = CGEventSource::new(CGEventSourceStateID::HIDSystemState).ok() else { return "cannot read the pointer".to_string() };
    let Some((x, y)) = pointer_position(&source) else { return "cannot read the pointer".to_string() };
    let system = unsafe { AXUIElementCreateSystemWide() };
    unsafe { AXUIElementSetMessagingTimeout(system, AX_TIMEOUT_SECS) };
    let text = match roles_at(system, x, y) {
        Ok(roles) => {
            format!("pointer ({x:.0}, {y:.0}): {}{}", roles.join(" < "), if clickable(&roles) { "   <- clickable: hand" } else { "" })
        }
        Err(e) => format!("pointer ({x:.0}, {y:.0}): no element (AX error {e})"),
    };
    unsafe { CFRelease(system) };
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn buttons_links_and_things_inside_them_are_clickable_text_and_fields_are_not() {
        assert!(clickable(&roles(&["AXButton"])));
        assert!(clickable(&roles(&["AXStaticText", "AXLink"])), "the text of a link");
        assert!(clickable(&roles(&["AXImage", "AXGroup", "AXMenuItem"])));
        assert!(!clickable(&roles(&["AXStaticText", "AXGroup"])));
        assert!(!clickable(&roles(&["AXTextField"])), "a text field shows the I-beam, not the hand");
        assert!(!clickable(&[]));
    }

    /// Reading the element under the pointer goes through the Accessibility API and must not crash,
    /// whatever is there (or nothing).
    #[test]
    fn the_element_under_the_pointer_can_be_read_without_crashing() {
        let mut source = AxHoverSource::new();
        for _ in 0..3 {
            let _ = source.read();
        }
        eprintln!("{}", probe_once());
    }
}

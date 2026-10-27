//! The Accessibility API entry points shared by the input listener and the focus and hover readers.

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    /// Accessibility permission for this process (or the app that launched it). Does not show a prompt.
    fn AXIsProcessTrusted() -> bool;
}

/// Whether macOS lets this process (or the app that launched it) use the Accessibility API.
pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

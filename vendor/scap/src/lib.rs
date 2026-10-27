#![allow(warnings)] // Capturra: vendored upstream code; its warnings are not ours
//! Cross Platform, Performant and High Quality screen recordings

pub mod capturer;
pub mod frame;
mod targets;
mod utils;

// Helper Methods
pub use targets::get_all_targets;
pub use targets::Target;
// Capturra patch: export the window type so a window can be named by id (to exclude it from a capture)
pub use targets::Window;
pub use utils::has_permission;
pub use utils::is_supported;
pub use utils::request_permission;

#[cfg(target_os = "macos")]
pub mod engine {
    pub use crate::capturer::engine::mac;
}

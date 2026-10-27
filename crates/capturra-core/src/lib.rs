//! Platform-independent brains of the recorder: input event model,
//! cursor smoothing, and the auto-zoom engine. No OS APIs in here, so it
//! builds and tests anywhere.

pub mod cursor;
pub mod easing;
pub mod events;
pub mod zoom;
pub mod zoom_stats;

pub use cursor::{CursorTimeline, CursorTrack};
pub use events::{CursorImage, DisplayInfo, EventLog, Hotspot, InputEvent, MouseButton, SizePt};
pub use zoom::{Rect, Viewport, ZoomConfig, ZoomSegment, ZoomTimeline};
pub use zoom_stats::ZoomStats;

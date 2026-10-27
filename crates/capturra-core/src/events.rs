use serde::{Deserialize, Serialize};

/// Display geometry. `width`/`height` are in the SAME units as the event
/// coordinates (points on macOS). `scale` is pixels-per-point (2.0 on Retina).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other,
}

/// Privacy note: key events carry timing ONLY, never which key was pressed.
///
/// `CursorChange` says which entry of `EventLog::cursors` the system cursor switched to
/// at `t_ms`. It carries no position and is not user input.
///
/// `Hover` (version 3) says whether the pointer is over something clickable (a button, link, tab or menu
/// item) from `t_ms` on. It is a yes/no only: no element names, roles or text.
///
/// `Focus` (version 3) is where the keyboard focus is: the frame of the focused text field, in the
/// same units as the pointer, from the Accessibility API. `w` and `h` of 0 say the focus left every
/// text field. It carries geometry only: no text, no field names, no window titles.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    Move { t_ms: u64, x: f64, y: f64 },
    Down { t_ms: u64, x: f64, y: f64, button: MouseButton },
    Up { t_ms: u64, x: f64, y: f64, button: MouseButton },
    Key { t_ms: u64 },
    CursorChange { t_ms: u64, id: u32 },
    Focus { t_ms: u64, x: f64, y: f64, w: f64, h: f64 },
    Hover { t_ms: u64, clickable: bool },
}

impl InputEvent {
    pub fn t_ms(&self) -> u64 {
        match *self {
            InputEvent::Move { t_ms, .. }
            | InputEvent::Down { t_ms, .. }
            | InputEvent::Up { t_ms, .. }
            | InputEvent::Key { t_ms }
            | InputEvent::CursorChange { t_ms, .. }
            | InputEvent::Focus { t_ms, .. }
            | InputEvent::Hover { t_ms, .. } => t_ms,
        }
    }

    pub fn position(&self) -> Option<(f64, f64)> {
        match *self {
            InputEvent::Move { x, y, .. } | InputEvent::Down { x, y, .. } | InputEvent::Up { x, y, .. } => Some((x, y)),
            InputEvent::Key { .. } | InputEvent::CursorChange { .. } | InputEvent::Focus { .. } | InputEvent::Hover { .. } => None,
        }
    }

    fn shifted(&self, offset: u64) -> InputEvent {
        let mut e = self.clone();
        match &mut e {
            InputEvent::Move { t_ms, .. }
            | InputEvent::Down { t_ms, .. }
            | InputEvent::Up { t_ms, .. }
            | InputEvent::Key { t_ms }
            | InputEvent::CursorChange { t_ms, .. }
            | InputEvent::Focus { t_ms, .. }
            | InputEvent::Hover { t_ms, .. } => *t_ms = t_ms.saturating_sub(offset),
        }
        e
    }
}

/// A point in a cursor image, in points from the image's top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Hotspot {
    pub x: f64,
    pub y: f64,
}

/// A size in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SizePt {
    pub w: f64,
    pub h: f64,
}

/// One distinct cursor image captured during the recording (the `cursors` table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorImage {
    pub id: u32,
    /// PNG with alpha, relative to the directory holding `events.json`.
    pub file: String,
    pub hotspot: Hotspot,
    /// Pixels per point of the PNG (pixel size / `size`).
    pub scale: f64,
    /// Display size in points: the PNG's pixel size divided by `scale`.
    pub size: SizePt,
    /// What a cursor is for, when it was added for a purpose rather than seen in use. Version 3:
    /// `"pointing_hand"` is the system hand the renderer shows while the pointer is over something clickable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl CursorImage {
    pub const POINTING_HAND: &'static str = "pointing_hand";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLog {
    pub version: u32,
    pub display: DisplayInfo,
    /// How long after the recording clock started the FIRST VIDEO FRAME arrived.
    /// Event times are on the recording clock until `to_video_time()` is applied.
    pub video_offset_ms: u64,
    pub events: Vec<InputEvent>,
    /// Cursor images referenced by `CursorChange` events. Added in version 2; a version 1
    /// file has none, and the renderer falls back to a plain circle.
    #[serde(default)]
    pub cursors: Vec<CursorImage>,
}

impl EventLog {
    /// Current schema version. 1 = input events only; 2 = adds `cursors` + `CursorChange`;
    /// 3 = adds `Focus` (the focused text field's frame).
    pub const VERSION: u32 = 3;

    pub fn new(display: DisplayInfo) -> Self {
        Self { version: Self::VERSION, display, video_offset_ms: 0, events: vec![], cursors: vec![] }
    }

    pub fn duration_ms(&self) -> u64 {
        self.events.iter().map(|e| e.t_ms()).max().unwrap_or(0)
    }

    /// Whether a point lies inside the captured area (edges count). Positions in an
    /// `events.json` are relative to the area, so a pointer elsewhere is outside the video.
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= 0.0 && y >= 0.0 && x <= self.display.width && y <= self.display.height
    }

    /// Map event coordinates to 0..=1 across the display.
    pub fn normalize(&self, x: f64, y: f64) -> (f64, f64) {
        ((x / self.display.width).clamp(0.0, 1.0), (y / self.display.height).clamp(0.0, 1.0))
    }

    pub fn sorted(&self) -> Vec<InputEvent> {
        let mut v = self.events.clone();
        v.sort_by_key(|e| e.t_ms());
        v
    }

    /// Put the events in time order. Stable, so events sharing a millisecond keep their order.
    /// The recorder merges the input log and the cursor-change events from two threads, so the
    /// merged list is not sorted until this runs.
    pub fn sort_events(&mut self) {
        self.events.sort_by_key(|e| e.t_ms());
    }

    /// A copy without the events that happen after `end_ms` (video time). Events past the end of
    /// the video cannot be shown; left in, they stretch the cursor track and can start zooms that
    /// never appear.
    pub fn clipped_to(&self, end_ms: u64) -> EventLog {
        EventLog {
            events: self.events.iter().filter(|e| e.t_ms() <= end_ms).cloned().collect(),
            cursors: self.cursors.clone(),
            ..self.clone()
        }
    }

    /// Shift every event so t=0 is the first video frame.
    pub fn to_video_time(&self) -> EventLog {
        EventLog {
            version: self.version,
            display: self.display,
            video_offset_ms: 0,
            events: self.events.iter().map(|e| e.shifted(self.video_offset_ms)).collect(),
            cursors: self.cursors.clone(),
        }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Parses version 1, 2 and 3 files. A file from a newer version is rejected rather than
    /// half-understood.
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        use serde::de::Error;
        let log: EventLog = serde_json::from_str(s)?;
        if log.version == 0 || log.version > Self::VERSION {
            return Err(serde_json::Error::custom(format!(
                "unsupported events.json version {} (this build reads 1..={})",
                log.version,
                Self::VERSION
            )));
        }
        Ok(log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display() -> DisplayInfo {
        DisplayInfo { width: 1000.0, height: 500.0, scale: 2.0 }
    }

    #[test]
    fn hover_events_and_the_hand_role_roundtrip_and_older_cursor_tables_still_load() {
        let mut log = EventLog::new(display());
        log.video_offset_ms = 50;
        let mut hand = sample_cursor(1);
        hand.role = Some(CursorImage::POINTING_HAND.to_string());
        log.cursors = vec![sample_cursor(0), hand];
        log.events.push(InputEvent::Hover { t_ms: 400, clickable: true });
        log.events.push(InputEvent::Hover { t_ms: 900, clickable: false });
        let json = log.to_json().unwrap();
        assert!(json.contains("\"type\": \"hover\"") && json.contains("\"role\": \"pointing_hand\""), "{json}");
        assert_eq!(json.matches("\"role\"").count(), 1, "only the hand has a role: {json}");
        assert_eq!(EventLog::from_json(&json).unwrap(), log);
        assert_eq!(log.events[0].position(), None);
        assert_eq!(log.to_video_time().events[0].t_ms(), 350);
        // a cursors table written before roles existed has none
        let old: CursorImage =
            serde_json::from_str(r#"{"id":0,"file":"cursors/0.png","hotspot":{"x":4.0,"y":4.0},"scale":2.0,"size":{"w":17.0,"h":23.0}}"#)
                .unwrap();
        assert_eq!(old.role, None);
    }

    #[test]
    fn json_roundtrip() {
        let mut log = EventLog::new(display());
        log.events.push(InputEvent::Move { t_ms: 1, x: 2.0, y: 3.0 });
        log.events.push(InputEvent::Down { t_ms: 5, x: 2.0, y: 3.0, button: MouseButton::Left });
        log.events.push(InputEvent::Key { t_ms: 9 });
        let back = EventLog::from_json(&log.to_json().unwrap()).unwrap();
        assert_eq!(log, back);
    }

    #[test]
    fn rebase_to_video_time() {
        let mut log = EventLog::new(display());
        log.video_offset_ms = 100;
        log.events.push(InputEvent::Key { t_ms: 50 });
        log.events.push(InputEvent::Key { t_ms: 400 });
        let v = log.to_video_time();
        assert_eq!(v.events[0].t_ms(), 0);
        assert_eq!(v.events[1].t_ms(), 300);
        assert_eq!(v.video_offset_ms, 0);
    }

    fn sample_cursor(id: u32) -> CursorImage {
        CursorImage {
            id,
            file: format!("cursors/{id}.png"),
            hotspot: Hotspot { x: 4.0, y: 4.0 },
            scale: 10.0,
            size: SizePt { w: 17.0, h: 23.0 },
            role: None,
        }
    }

    #[test]
    fn new_logs_roundtrip_with_cursors() {
        let mut log = EventLog::new(display());
        assert_eq!(log.version, EventLog::VERSION);
        log.cursors.push(sample_cursor(0));
        log.cursors.push(sample_cursor(1));
        log.events.push(InputEvent::CursorChange { t_ms: 0, id: 0 });
        log.events.push(InputEvent::CursorChange { t_ms: 700, id: 1 });
        let json = log.to_json().unwrap();
        assert!(json.contains("\"type\": \"cursor_change\""), "{json}");
        assert_eq!(EventLog::from_json(&json).unwrap(), log);
    }

    #[test]
    fn version_1_file_still_loads_without_cursors() {
        let v1 = r#"{
            "version": 1,
            "display": { "width": 1000.0, "height": 500.0, "scale": 2.0 },
            "video_offset_ms": 40,
            "events": [
                { "type": "move", "t_ms": 1, "x": 2.0, "y": 3.0 },
                { "type": "key", "t_ms": 9 }
            ]
        }"#;
        let log = EventLog::from_json(v1).unwrap();
        assert_eq!(log.version, 1);
        assert!(log.cursors.is_empty());
        assert_eq!(log.events.len(), 2);
        assert_eq!(log.video_offset_ms, 40);
    }

    #[test]
    fn newer_or_zero_version_is_rejected() {
        let mk = |v: u32| {
            format!(r#"{{"version": {v}, "display": {{"width": 1.0, "height": 1.0, "scale": 1.0}}, "video_offset_ms": 0, "events": []}}"#)
        };
        assert!(EventLog::from_json(&mk(4)).unwrap_err().to_string().contains("unsupported events.json version 4"));
        assert!(EventLog::from_json(&mk(0)).is_err());
        assert!(EventLog::from_json(&mk(2)).is_ok());
        assert!(EventLog::from_json(&mk(3)).is_ok());
    }

    #[test]
    fn version_2_file_still_loads_and_focus_events_roundtrip_in_version_3() {
        let v2 = r#"{ "version": 2, "display": { "width": 1000.0, "height": 500.0, "scale": 2.0 }, "video_offset_ms": 0,
            "events": [ { "type": "key", "t_ms": 9 } ], "cursors": [] }"#;
        assert_eq!(EventLog::from_json(v2).unwrap().version, 2);
        let mut log = EventLog::new(display());
        log.video_offset_ms = 100;
        log.events.push(InputEvent::Focus { t_ms: 300, x: 10.0, y: 20.0, w: 300.0, h: 24.0 });
        log.events.push(InputEvent::Focus { t_ms: 900, x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
        let json = log.to_json().unwrap();
        assert!(json.contains("\"type\": \"focus\""), "{json}");
        assert!(!json.contains("title") && !json.contains("role"), "geometry only: {json}");
        assert_eq!(EventLog::from_json(&json).unwrap(), log);
        // a focus change is not a pointer position, and rebases with everything else
        assert_eq!(log.events[0].position(), None);
        assert_eq!(log.to_video_time().events[0].t_ms(), 200);
    }

    #[test]
    fn cursor_change_is_not_a_position_and_rebases_with_the_rest() {
        let mut log = EventLog::new(display());
        log.video_offset_ms = 100;
        log.cursors.push(sample_cursor(0));
        log.events.push(InputEvent::CursorChange { t_ms: 20, id: 0 });
        log.events.push(InputEvent::CursorChange { t_ms: 500, id: 0 });
        assert_eq!(log.events[0].position(), None);
        let v = log.to_video_time();
        assert_eq!(v.events[0], InputEvent::CursorChange { t_ms: 0, id: 0 });
        assert_eq!(v.events[1], InputEvent::CursorChange { t_ms: 400, id: 0 });
        assert_eq!(v.cursors, log.cursors, "rebasing must keep the cursors table");
    }

    #[test]
    fn sort_events_orders_by_time_and_keeps_same_millisecond_order() {
        let mut log = EventLog::new(display());
        log.events.push(InputEvent::Key { t_ms: 30 });
        log.events.push(InputEvent::CursorChange { t_ms: 10, id: 0 });
        log.events.push(InputEvent::Move { t_ms: 10, x: 1.0, y: 1.0 });
        log.events.push(InputEvent::Key { t_ms: 5 });
        log.sort_events();
        let order: Vec<u64> = log.events.iter().map(|e| e.t_ms()).collect();
        assert_eq!(order, vec![5, 10, 10, 30]);
        assert!(matches!(log.events[1], InputEvent::CursorChange { .. }), "same-ms events keep their order");
        assert!(matches!(log.events[2], InputEvent::Move { .. }));
    }

    #[test]
    fn clipped_to_drops_only_events_after_the_end() {
        let mut log = EventLog::new(display());
        log.cursors.push(sample_cursor(0));
        log.events.push(InputEvent::Key { t_ms: 100 });
        log.events.push(InputEvent::Key { t_ms: 200 });
        log.events.push(InputEvent::CursorChange { t_ms: 201, id: 0 });
        let c = log.clipped_to(200);
        assert_eq!(c.events.len(), 2, "an event exactly at the end stays");
        assert_eq!(c.cursors, log.cursors);
        assert_eq!(c.display, log.display);
        assert_eq!(log.events.len(), 3, "the original is untouched");
    }

    #[test]
    fn contains_includes_the_edges_and_excludes_everything_else() {
        let log = EventLog::new(display());
        assert!(log.contains(0.0, 0.0) && log.contains(1000.0, 500.0) && log.contains(500.0, 250.0));
        assert!(!log.contains(-0.1, 10.0) && !log.contains(10.0, 500.1) && !log.contains(1000.1, 0.0));
    }

    #[test]
    fn normalize_clamps() {
        let log = EventLog::new(display());
        assert_eq!(log.normalize(500.0, 250.0), (0.5, 0.5));
        assert_eq!(log.normalize(-10.0, 9999.0), (0.0, 1.0));
    }
}

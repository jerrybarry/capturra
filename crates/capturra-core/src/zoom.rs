//! Auto-zoom: turn clicks and typing bursts into smooth zoom keyframes.
//!
//! Model: each trigger becomes a segment [start, end]. The camera eases in
//! during the `ramp_in_ms` BEFORE `start` (so the zoom is already in place when
//! the click lands), holds, then eases out over `ramp_out_ms` after `end`.
//! Triggers that are close together merge into one segment, and the camera
//! pans between their positions instead of zooming out and in again.
//!
//! Beyond that the engine keeps the picture watchable:
//! - a burst of rapid clicks is ONE trigger (at their centre), and clicks that cannot be on purpose
//!   (a side button, the very first moments) are ignored;
//! - zooms that are close in time AND place merge, so the camera pans instead of pumping out and in;
//! - while zoomed, the camera follows the pointer when it nears the edge of the view;
//! - each zoom's level is fitted to what it has to show: a wide area or a big text field zooms less,
//!   a small target zooms a bit more than the nominal level.
//!
//! Everything is deterministic, so the same recording always gives the same zooms.

use crate::easing::{clamp01, ease_in_out_cubic, lerp};
use crate::events::{EventLog, InputEvent, MouseButton};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct ZoomConfig {
    /// The nominal zoom level. Fitting never goes much above it and never below `min_level`.
    pub zoom_level: f64,
    /// Camera finishes zooming this long before the trigger.
    pub lead_ms: u64,
    /// Camera stays zoomed this long after the trigger.
    pub hold_ms: u64,
    pub ramp_in_ms: u64,
    pub ramp_out_ms: u64,
    /// Triggers closer than this (after hold) join the same zoom. Must exceed the two ramps
    /// together, or the camera starts leaving and then turns round.
    pub merge_gap_ms: u64,
    /// Triggers further apart than this (normalized) never join one zoom: the camera would have to
    /// sweep across the screen. They become two zooms.
    pub merge_distance: f64,
    pub pan_ms: u64,
    /// Key presses closer than this count as one typing burst.
    pub key_burst_gap_ms: u64,
    /// Ignore pans smaller than this (normalized distance) to avoid jitter.
    pub min_pan_distance: f64,
    /// Clicks closer together than this are one click (a double click, rapid tapping).
    pub cluster_ms: u64,
    /// Clicks in the first moments of the recording are the end of starting it, not content.
    pub ignore_start_ms: u64,
    /// Follow the pointer while zoomed so it never leaves the view.
    pub follow: bool,
    /// The pointer is "near the edge" when it is within this share of the view's size from it.
    pub follow_margin: f64,
    /// ...and the camera only moves for it if it is still out there this long after (a wiggle that
    /// comes straight back is not worth chasing).
    pub follow_dwell_ms: u64,
    /// Fit each zoom's level to its target.
    pub fit_level: bool,
    /// Never zoom less than this (a zoom this shallow is not worth doing).
    pub min_level: f64,
    /// A small known target may zoom up to this many times the nominal level.
    pub max_boost: f64,
    /// How much of the view a known target should fill (0..1).
    pub target_fill: f64,
    /// A focused field covering more than this share of the screen (a whole editor or web page) says
    /// nothing about where typing happens, so it is ignored and the pointer is used instead.
    pub max_focus_area: f64,
}

impl Default for ZoomConfig {
    fn default() -> Self {
        Self {
            zoom_level: 2.0,
            lead_ms: 150,
            hold_ms: 1200,
            ramp_in_ms: 450,
            ramp_out_ms: 600,
            merge_gap_ms: 2000,
            merge_distance: 0.45,
            pan_ms: 450,
            key_burst_gap_ms: 800,
            min_pan_distance: 0.04,
            cluster_ms: 500,
            ignore_start_ms: 400,
            follow: true,
            follow_margin: 0.15,
            follow_dwell_ms: 400,
            fit_level: true,
            min_level: 1.3,
            max_boost: 1.35,
            target_fill: 0.6,
            max_focus_area: 0.35,
        }
    }
}

impl ZoomConfig {
    /// A named style: "gentle", "standard" (the default) or "punchy". The Studio's style picker uses these,
    /// so the editor and the exporter always agree on what a style means.
    pub fn preset(name: &str) -> Option<ZoomConfig> {
        let base = ZoomConfig::default();
        match name {
            "standard" => Some(base),
            "gentle" => Some(ZoomConfig {
                zoom_level: 1.6,
                lead_ms: 250,
                hold_ms: 1600,
                ramp_in_ms: 700,
                ramp_out_ms: 800,
                merge_gap_ms: 2400,
                pan_ms: 600,
                ..base
            }),
            "punchy" => Some(ZoomConfig {
                zoom_level: 2.6,
                lead_ms: 100,
                hold_ms: 1000,
                ramp_in_ms: 300,
                ramp_out_ms: 420,
                merge_gap_ms: 1500,
                pan_ms: 320,
                ..base
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Waypoint {
    pub t_ms: u64,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZoomSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub waypoints: Vec<Waypoint>,
    /// Overrides for this segment, set by an editor. `None` = the timeline's `ZoomConfig` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_level: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_in_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ramp_out_ms: Option<u64>,
}

impl ZoomSegment {
    /// An automatic segment: every setting comes from the config.
    pub fn new(start_ms: u64, end_ms: u64, waypoints: Vec<Waypoint>) -> Self {
        Self { start_ms, end_ms, waypoints, zoom_level: None, ramp_in_ms: None, ramp_out_ms: None }
    }
}

/// Normalized rectangle (0..=1) of the source frame that should fill the output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub scale: f64,
    pub cx: f64,
    pub cy: f64,
}

impl Viewport {
    pub const FULL: Viewport = Viewport { scale: 1.0, cx: 0.5, cy: 0.5 };

    /// Visible region, clamped so it never leaves the frame.
    pub fn rect(&self) -> Rect {
        let w = 1.0 / self.scale;
        let h = 1.0 / self.scale;
        let x = (self.cx - w / 2.0).clamp(0.0, 1.0 - w);
        let y = (self.cy - h / 2.0).clamp(0.0, 1.0 - h);
        Rect { x, y, w, h }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoomTimeline {
    pub config: ZoomConfig,
    pub segments: Vec<ZoomSegment>,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Click,
    Key,
}

struct Trigger {
    t_ms: u64,
    x: f64,
    y: f64,
    kind: Kind,
    /// Size (normalized) of what is being worked on, when known (the focused text field).
    target: Option<(f64, f64)>,
}

fn triggers(log: &EventLog, cfg: &ZoomConfig) -> Vec<Trigger> {
    let mut pos = (0.5, 0.5);
    // Input aimed outside the captured area (region and window recordings) is not in the video,
    // so it must not pull the camera to the nearest edge.
    let mut inside = true;
    let mut last_key: Option<u64> = None;
    let mut out: Vec<Trigger> = Vec::new();
    // the focused text field, as (centre x, centre y, width, height), all normalized
    let mut focus: Option<(f64, f64, f64, f64)> = None;
    for ev in log.sorted() {
        match ev {
            InputEvent::Focus { t_ms, x, y, w, h } => {
                focus = None;
                let (cx, cy) = (x + w / 2.0, y + h / 2.0);
                if w > 0.0 && h > 0.0 && log.contains(cx, cy) {
                    let (nc, nw, nh) = (log.normalize(cx, cy), (w / log.display.width).min(1.0), (h / log.display.height).min(1.0));
                    if nw * nh <= cfg.max_focus_area {
                        focus = Some((nc.0, nc.1, nw, nh));
                        // clicking into a field: the field's size says how much to zoom
                        if let Some(prev) = out.last_mut() {
                            let inside_field = (prev.x - nc.0).abs() <= nw / 2.0 + 0.02 && (prev.y - nc.1).abs() <= nh / 2.0 + 0.02;
                            if prev.kind == Kind::Click && t_ms.saturating_sub(prev.t_ms) <= 600 && inside_field {
                                prev.target = Some((nw, nh));
                            }
                        }
                        // moving between fields while typing (Tab): follow the new one
                        let typing = last_key.is_some_and(|k| t_ms.saturating_sub(k) <= cfg.key_burst_gap_ms);
                        let moved = out.last().is_none_or(|p| dist((p.x, p.y), nc) >= cfg.min_pan_distance);
                        if typing && moved {
                            out.push(Trigger { t_ms, x: nc.0, y: nc.1, kind: Kind::Key, target: Some((nw, nh)) });
                        }
                    }
                }
            }
            InputEvent::Move { x, y, .. } | InputEvent::Up { x, y, .. } => {
                pos = log.normalize(x, y);
                inside = log.contains(x, y);
            }
            InputEvent::CursorChange { .. } | InputEvent::Hover { .. } => {}
            InputEvent::Down { t_ms, x, y, button } => {
                pos = log.normalize(x, y);
                inside = log.contains(x, y);
                // a side or other button is not a click anyone meant to show; so is whatever happens
                // while recording is still starting
                let meant = matches!(button, MouseButton::Left | MouseButton::Right) && t_ms >= cfg.ignore_start_ms;
                if inside && meant {
                    // rapid clicks (a double click, tapping) are one trigger, at their centre
                    match out.last_mut() {
                        Some(prev) if prev.kind == Kind::Click && t_ms.saturating_sub(prev.t_ms) <= cfg.cluster_ms => {
                            prev.x = (prev.x + pos.0) / 2.0;
                            prev.y = (prev.y + pos.1) / 2.0;
                            // the cluster keeps ending at the latest click so a long tapping run stays one
                            prev.t_ms = prev.t_ms.max(t_ms.saturating_sub(cfg.cluster_ms / 2));
                        }
                        _ => out.push(Trigger { t_ms, x: pos.0, y: pos.1, kind: Kind::Click, target: None }),
                    }
                }
            }
            InputEvent::Key { t_ms } => {
                let new_burst = last_key.is_none_or(|k| t_ms.saturating_sub(k) > cfg.key_burst_gap_ms);
                last_key = Some(t_ms);
                if new_burst {
                    match focus {
                        // typing happens in the focused field, wherever the pointer is (version 3 recordings)
                        Some((fx, fy, fw, fh)) => out.push(Trigger { t_ms, x: fx, y: fy, kind: Kind::Key, target: Some((fw, fh)) }),
                        // older recordings: typing is assumed to be near the pointer
                        None if inside => out.push(Trigger { t_ms, x: pos.0, y: pos.1, kind: Kind::Key, target: None }),
                        None => {}
                    }
                }
            }
        }
    }
    out
}

/// A zoom being built: its segment plus the smallest level any known target allows.
struct Building {
    seg: ZoomSegment,
    target_level: Option<f64>,
}

fn dist(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

impl ZoomTimeline {
    pub fn new(log: &EventLog, config: ZoomConfig) -> Self {
        let mut building: Vec<Building> = Vec::new();
        for tr in triggers(log, &config) {
            let start = tr.t_ms.saturating_sub(config.lead_ms);
            let end = tr.t_ms + config.hold_ms;
            let wp = Waypoint { t_ms: start, x: tr.x, y: tr.y };
            let level_for_target = tr.target.map(|(w, h)| config.target_fill / w.max(h).max(0.02));
            match building.last_mut() {
                Some(cur)
                    if start <= cur.seg.end_ms + config.merge_gap_ms
                        && dist((cur.seg.waypoints.last().unwrap().x, cur.seg.waypoints.last().unwrap().y), (wp.x, wp.y))
                            <= config.merge_distance =>
                {
                    cur.seg.end_ms = cur.seg.end_ms.max(end);
                    let last = cur.seg.waypoints.last().unwrap();
                    if dist((last.x, last.y), (wp.x, wp.y)) >= config.min_pan_distance {
                        cur.seg.waypoints.push(wp);
                    }
                    cur.target_level = match (cur.target_level, level_for_target) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    };
                }
                _ => building.push(Building { seg: ZoomSegment::new(start, end, vec![wp]), target_level: level_for_target }),
            }
        }
        let path = pointer_path(log);
        for b in &mut building {
            if config.follow {
                follow_pointer(&mut b.seg, &path, &config);
            }
            if config.fit_level {
                let level = fitted_level(&b.seg, b.target_level, &config);
                if (level - config.zoom_level).abs() > 1e-6 {
                    b.seg.zoom_level = Some(level);
                }
            }
        }
        Self { config, segments: building.into_iter().map(|b| b.seg).collect() }
    }

    fn factor(&self, seg: &ZoomSegment, t: f64) -> f64 {
        let (s, e) = (seg.start_ms as f64, seg.end_ms as f64);
        let ri = seg.ramp_in_ms.unwrap_or(self.config.ramp_in_ms).max(1) as f64;
        let ro = seg.ramp_out_ms.unwrap_or(self.config.ramp_out_ms).max(1) as f64;
        if t < s - ri || t > e + ro {
            0.0
        } else if t < s {
            ease_in_out_cubic((t - (s - ri)) / ri)
        } else if t <= e {
            1.0
        } else {
            1.0 - ease_in_out_cubic((t - e) / ro)
        }
    }

    fn focus(&self, seg: &ZoomSegment, t: f64) -> (f64, f64) {
        focus_at(seg, t, self.config.pan_ms)
    }

    /// Camera state at time `t_ms` (video time).
    pub fn sample(&self, t_ms: u64) -> Viewport {
        let t = t_ms as f64;
        let mut best: Option<(&ZoomSegment, f64)> = None;
        for seg in &self.segments {
            let f = self.factor(seg, t);
            if f > 0.0 && best.is_none_or(|(_, bf)| f > bf) {
                best = Some((seg, f));
            }
        }
        match best {
            None => Viewport::FULL,
            Some((seg, f)) => {
                let (cx, cy) = self.focus(seg, t);
                let level = seg.zoom_level.unwrap_or(self.config.zoom_level);
                Viewport { scale: 1.0 + (level - 1.0) * f, cx, cy }
            }
        }
    }
}

fn focus_at(seg: &ZoomSegment, t: f64, pan_ms: u64) -> (f64, f64) {
    let pan = pan_ms.max(1) as f64;
    let first = seg.waypoints[0];
    let mut c = (first.x, first.y);
    for wp in &seg.waypoints[1..] {
        let p = ease_in_out_cubic(clamp01((t - (wp.t_ms as f64 - pan)) / pan));
        c = (lerp(c.0, wp.x, p), lerp(c.1, wp.y, p));
    }
    c
}

/// Where the pointer is, over time, while it is inside the captured area.
fn pointer_path(log: &EventLog) -> Vec<(u64, f64, f64)> {
    log.sorted()
        .into_iter()
        .filter_map(|e| match e {
            InputEvent::Move { t_ms, x, y } | InputEvent::Down { t_ms, x, y, .. } | InputEvent::Up { t_ms, x, y, .. }
                if log.contains(x, y) =>
            {
                let (nx, ny) = log.normalize(x, y);
                Some((t_ms, nx, ny))
            }
            _ => None,
        })
        .collect()
}

/// While the segment holds, pan after the pointer whenever it gets near the edge of the view. The new
/// focus is where the pointer will be when the pan arrives (the whole recording is known, so the
/// camera can lead instead of chasing).
fn follow_pointer(seg: &mut ZoomSegment, path: &[(u64, f64, f64)], cfg: &ZoomConfig) {
    let level = cfg.zoom_level.max(1.0);
    let span = 1.0 / level;
    let reach = span * (0.5 - cfg.follow_margin);
    let outside = |seg: &ZoomSegment, t: u64, x: f64, y: f64| {
        let (fx, fy) = focus_at(seg, t as f64, cfg.pan_ms);
        // the view's centre cannot go past the frame
        let (cx, cy) = (fx.clamp(span / 2.0, 1.0 - span / 2.0), fy.clamp(span / 2.0, 1.0 - span / 2.0));
        (x - cx).abs() > reach || (y - cy).abs() > reach
    };
    let mut busy_until = 0;
    for (i, &(t, x, y)) in path.iter().enumerate() {
        if t < seg.start_ms || t > seg.end_ms || t < busy_until {
            continue;
        }
        if !outside(seg, t, x, y) {
            continue;
        }
        // still out there a moment later?
        let later = path[i..].iter().take_while(|p| p.0 <= t + cfg.follow_dwell_ms).last().copied().unwrap_or((t, x, y));
        if !outside(seg, later.0, later.1, later.2) {
            continue;
        }
        let arrive = t + cfg.pan_ms;
        let ahead = path[i..].iter().take_while(|p| p.0 <= arrive).last().copied().unwrap_or((t, x, y));
        let last = seg.waypoints.last().copied().unwrap();
        if dist((last.x, last.y), (ahead.1, ahead.2)) < cfg.min_pan_distance {
            continue;
        }
        let at = seg.waypoints.iter().position(|w| w.t_ms > arrive).unwrap_or(seg.waypoints.len());
        seg.waypoints.insert(at, Waypoint { t_ms: arrive, x: ahead.1, y: ahead.2 });
        busy_until = arrive;
    }
}

/// The level this zoom should use: the nominal level, lowered if its waypoints are spread over a
/// wide area, and set by the size of a known target (a text field) when there is one.
fn fitted_level(seg: &ZoomSegment, target_level: Option<f64>, cfg: &ZoomConfig) -> f64 {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for w in &seg.waypoints {
        x0 = x0.min(w.x);
        x1 = x1.max(w.x);
        y0 = y0.min(w.y);
        y1 = y1.max(w.y);
    }
    let extent = (x1 - x0).max(y1 - y0).max(0.0);
    // a view of 1/level must hold the spread of the waypoints plus room around them
    let for_spread = 0.8 / (extent + 0.2);
    let ceiling = cfg.zoom_level * cfg.max_boost;
    let wanted = target_level.map_or(cfg.zoom_level, |t| t.min(ceiling));
    let level = wanted.min(for_spread);
    level.max(cfg.min_level.min(cfg.zoom_level)).min(ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{DisplayInfo, MouseButton};

    fn log(events: Vec<InputEvent>) -> EventLog {
        let mut l = EventLog::new(DisplayInfo { width: 1000.0, height: 1000.0, scale: 1.0 });
        l.events = events;
        l
    }

    fn click(t_ms: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Down { t_ms, x, y, button: MouseButton::Left }
    }

    #[test]
    fn single_click_zooms_in_and_out() {
        let tl = ZoomTimeline::new(&log(vec![click(5000, 800.0, 200.0)]), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1);
        assert_eq!(tl.sample(4000).scale, 1.0);
        let peak = tl.sample(5000);
        assert!((peak.scale - 2.0).abs() < 1e-9);
        assert!((peak.cx - 0.8).abs() < 1e-9 && (peak.cy - 0.2).abs() < 1e-9);
        assert_eq!(tl.sample(8000).scale, 1.0);
        // Ramp is gradual, not a jump.
        let mid = tl.sample(4600).scale;
        assert!(mid > 1.0 && mid < 2.0);
    }

    #[test]
    fn a_segment_can_override_its_own_zoom_level_and_ramps() {
        let mut tl = ZoomTimeline::new(&log(vec![click(5000, 200.0, 200.0), click(15_000, 800.0, 800.0)]), ZoomConfig::default());
        tl.segments[1].zoom_level = Some(3.0);
        tl.segments[1].ramp_in_ms = Some(100);
        assert!((tl.sample(5000).scale - 2.0).abs() < 1e-9, "the first segment keeps the config level");
        assert!((tl.sample(15_000).scale - 3.0).abs() < 1e-9);
        // a 100 ms ramp has not started 300 ms before the segment; the default 450 ms ramp has
        assert_eq!(tl.sample(14_550).scale, 1.0);
        assert!(tl.sample(4_600).scale > 1.0);
    }

    #[test]
    fn segments_without_overrides_serialize_as_before() {
        let tl = ZoomTimeline::new(&log(vec![click(5000, 200.0, 200.0)]), ZoomConfig::default());
        let json = serde_json::to_string(&tl.segments[0]).unwrap();
        assert!(!json.contains("zoom_level") && !json.contains("ramp"));
        let old: ZoomSegment = serde_json::from_str(&json).unwrap();
        assert_eq!(old, tl.segments[0]);
    }

    #[test]
    fn close_clicks_merge_and_pan() {
        let tl = ZoomTimeline::new(
            &log(vec![click(5000, 200.0, 200.0), click(6000, 450.0, 350.0)]),
            ZoomConfig { fit_level: false, ..Default::default() },
        );
        assert_eq!(tl.segments.len(), 1);
        assert_eq!(tl.segments[0].waypoints.len(), 2);
        // Stays zoomed while panning between them.
        assert!((tl.sample(5700).scale - 2.0).abs() < 1e-9);
        let end = tl.sample(6500);
        assert!((end.cx - 0.45).abs() < 1e-9);
    }

    fn mv(t_ms: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Move { t_ms, x, y }
    }

    fn key(t_ms: u64) -> InputEvent {
        InputEvent::Key { t_ms }
    }

    #[test]
    fn rapid_clicks_are_one_trigger_at_their_centre() {
        // a double click, then three taps in a row, all within half a second of each other
        let tl = ZoomTimeline::new(
            &log(vec![click(5000, 200.0, 200.0), click(5200, 220.0, 200.0), click(5400, 240.0, 220.0), click(5600, 240.0, 220.0)]),
            ZoomConfig::default(),
        );
        assert_eq!(tl.segments.len(), 1);
        assert_eq!(tl.segments[0].waypoints.len(), 1, "no panning between taps: {:?}", tl.segments[0].waypoints);
        // slow clicks are separate triggers, even close together
        let slow = ZoomTimeline::new(&log(vec![click(5000, 200.0, 200.0), click(6000, 300.0, 250.0)]), ZoomConfig::default());
        assert_eq!(slow.segments[0].waypoints.len(), 2);
    }

    #[test]
    fn clicks_nobody_meant_are_ignored() {
        let side = |t_ms| InputEvent::Down { t_ms, x: 500.0, y: 500.0, button: MouseButton::Middle };
        let other = |t_ms| InputEvent::Down { t_ms, x: 500.0, y: 500.0, button: MouseButton::Other };
        let right = |t_ms| InputEvent::Down { t_ms, x: 500.0, y: 500.0, button: MouseButton::Right };
        assert!(ZoomTimeline::new(&log(vec![side(5000), other(9000)]), ZoomConfig::default()).segments.is_empty());
        assert_eq!(
            ZoomTimeline::new(&log(vec![right(5000)]), ZoomConfig::default()).segments.len(),
            1,
            "a right click opens a menu: worth showing"
        );
        // the click that ended the start of the recording
        assert!(ZoomTimeline::new(&log(vec![click(120, 500.0, 500.0)]), ZoomConfig::default()).segments.is_empty());
        assert_eq!(ZoomTimeline::new(&log(vec![click(900, 500.0, 500.0)]), ZoomConfig::default()).segments.len(), 1);
    }

    #[test]
    fn a_short_gap_pans_instead_of_zooming_out_and_in_and_a_long_one_does_not() {
        let cfg = ZoomConfig::default();
        // second click 1.6 s after the first zoom ends: the two ramps (1.05 s) would not even finish
        let first_end = 5000 + cfg.hold_ms;
        let near = log(vec![click(5000, 200.0, 200.0), click(first_end + 1600 + cfg.lead_ms, 300.0, 250.0)]);
        assert_eq!(ZoomTimeline::new(&near, cfg).segments.len(), 1);
        let far_in_time = log(vec![click(5000, 200.0, 200.0), click(first_end + cfg.merge_gap_ms + 500 + cfg.lead_ms, 300.0, 250.0)]);
        assert_eq!(ZoomTimeline::new(&far_in_time, cfg).segments.len(), 2);
        assert!(cfg.merge_gap_ms > cfg.ramp_in_ms + cfg.ramp_out_ms, "the merge gap must exceed both ramps");
    }

    #[test]
    fn triggers_far_apart_on_screen_do_not_share_one_zoom_even_if_close_in_time() {
        let tl = ZoomTimeline::new(&log(vec![click(5000, 100.0, 100.0), click(6500, 900.0, 900.0)]), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 2, "sweeping the camera across the screen is worse than zooming out and in");
        let near = ZoomTimeline::new(&log(vec![click(5000, 100.0, 100.0), click(6500, 400.0, 300.0)]), ZoomConfig::default());
        assert_eq!(near.segments.len(), 1);
    }

    #[test]
    fn the_camera_follows_a_pointer_that_leaves_the_view_for_good_but_not_a_wiggle() {
        // click top-left, then the pointer settles bottom-right
        let go = log(vec![click(5000, 200.0, 200.0), mv(5400, 500.0, 500.0), mv(5800, 800.0, 800.0), mv(6000, 800.0, 800.0)]);
        let tl = ZoomTimeline::new(&go, ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1);
        assert!(tl.segments[0].waypoints.len() >= 2, "{:?}", tl.segments[0].waypoints);
        let end = tl.sample(tl.segments[0].end_ms);
        assert!(end.cx > 0.6 && end.cy > 0.6, "the view followed the pointer: {end:?}");
        // the same path without following leaves the pointer out of view
        let fixed = ZoomTimeline::new(&go, ZoomConfig { follow: false, ..Default::default() });
        assert_eq!(fixed.segments[0].waypoints.len(), 1);
        // a quick excursion that comes straight back is not chased
        let wiggle = log(vec![click(5000, 200.0, 200.0), mv(5400, 800.0, 800.0), mv(5550, 200.0, 200.0)]);
        assert_eq!(ZoomTimeline::new(&wiggle, ZoomConfig::default()).segments[0].waypoints.len(), 1);
        // a pointer that stays near the click never moves the camera
        let near = log(vec![click(5000, 200.0, 200.0), mv(5400, 260.0, 240.0), mv(5800, 230.0, 190.0)]);
        assert_eq!(ZoomTimeline::new(&near, ZoomConfig::default()).segments[0].waypoints.len(), 1);
    }

    #[test]
    fn the_level_fits_what_the_zoom_has_to_show() {
        // one click: the nominal level, and nothing stored on the segment
        let one = ZoomTimeline::new(&log(vec![click(5000, 500.0, 500.0)]), ZoomConfig::default());
        assert_eq!(one.segments[0].zoom_level, None);
        // two clicks 0.35 apart: the view must hold both with room around them
        let two = ZoomTimeline::new(&log(vec![click(5000, 200.0, 300.0), click(6000, 550.0, 300.0)]), ZoomConfig::default());
        let level = two.segments[0].zoom_level.expect("lowered to fit");
        assert!((level - 0.8 / 0.55).abs() < 1e-9, "{level}");
        assert!((two.sample(5500).scale - level).abs() < 1e-9);
        // never shallower than min_level
        let wide = ZoomTimeline::new(&log(vec![click(5000, 100.0, 300.0), click(6000, 540.0, 300.0)]), ZoomConfig::default());
        assert!(wide.segments[0].zoom_level.unwrap() >= ZoomConfig::default().min_level - 1e-9);
        // and fitting can be switched off
        let off = ZoomTimeline::new(
            &log(vec![click(5000, 200.0, 300.0), click(6000, 550.0, 300.0)]),
            ZoomConfig { fit_level: false, ..Default::default() },
        );
        assert_eq!(off.segments[0].zoom_level, None);
    }

    fn focus(t_ms: u64, x: f64, y: f64, w: f64, h: f64) -> InputEvent {
        InputEvent::Focus { t_ms, x, y, w, h }
    }

    #[test]
    fn typing_zooms_to_the_focused_field_not_to_the_pointer() {
        // the pointer rests top-left; the field being typed in is at the bottom right
        let events = vec![mv(0, 100.0, 100.0), focus(2000, 600.0, 700.0, 200.0, 40.0), key(5000), key(5100)];
        let tl = ZoomTimeline::new(&log(events), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1);
        let peak = tl.sample(5000);
        assert!((peak.cx - 0.7).abs() < 1e-9 && (peak.cy - 0.72).abs() < 1e-9, "the field's centre: {peak:?}");
        // without the focus event the same typing zooms to the pointer, as before
        let old = ZoomTimeline::new(&log(vec![mv(0, 100.0, 100.0), key(5000), key(5100)]), ZoomConfig::default());
        let p = old.sample(5000);
        assert!((p.cx - 0.1).abs() < 1e-9 && (p.cy - 0.1).abs() < 1e-9);
    }

    #[test]
    fn typing_zooms_even_with_the_pointer_outside_the_area_when_the_field_is_inside() {
        let events = vec![mv(0, 1500.0, 500.0), focus(1000, 300.0, 300.0, 300.0, 30.0), key(3000)];
        let tl = ZoomTimeline::new(&log(events), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1, "the field is in the video, so typing there is worth showing");
        // focus cleared (it left every text field): back to "type near the pointer", which is outside: nothing
        let cleared = vec![mv(0, 1500.0, 500.0), focus(1000, 300.0, 300.0, 300.0, 30.0), focus(2000, 0.0, 0.0, 0.0, 0.0), key(3000)];
        assert!(ZoomTimeline::new(&log(cleared), ZoomConfig::default()).segments.is_empty());
    }

    #[test]
    fn the_zoom_level_fits_the_size_of_the_field() {
        let level_for = |w: f64, h: f64| {
            let tl = ZoomTimeline::new(&log(vec![focus(1000, 300.0, 400.0, w, h), key(3000)]), ZoomConfig::default());
            tl.segments[0].zoom_level.unwrap_or(ZoomConfig::default().zoom_level)
        };
        let huge = level_for(550.0, 300.0); // 0.6 / 0.55 is barely a zoom: held at the minimum
        assert!((huge - ZoomConfig::default().min_level).abs() < 1e-9, "{huge}");
        let big = level_for(400.0, 300.0); // a big text area: 0.6 / 0.4
        let mid = level_for(300.0, 40.0); // 0.6 / 0.3 = 2.0, the nominal level
        let small = level_for(80.0, 30.0); // small: more, but only up to the boost ceiling
        assert!((big - 1.5).abs() < 1e-9, "{big}");
        assert!((mid - 2.0).abs() < 1e-9, "{mid}");
        assert!((small - 2.0 * 1.35).abs() < 1e-9, "{small}");
        assert!(big < mid && mid < small);
    }

    #[test]
    fn a_field_that_fills_most_of_the_screen_is_ignored() {
        // a whole editor or web page: 900 x 800 of a 1000 x 1000 display = 72% of the screen
        let events = vec![mv(0, 100.0, 100.0), focus(1000, 50.0, 50.0, 900.0, 800.0), key(3000)];
        let tl = ZoomTimeline::new(&log(events), ZoomConfig::default());
        let p = tl.sample(3000);
        assert!((p.cx - 0.1).abs() < 1e-9, "fell back to the pointer: {p:?}");
    }

    #[test]
    fn clicking_into_a_field_sizes_the_zoom_to_it_and_tabbing_to_the_next_field_pans() {
        // a click inside a wide field, then the field's frame arrives 100 ms later
        let events = vec![click(2000, 500.0, 400.0), focus(2100, 150.0, 380.0, 700.0, 40.0)];
        let tl = ZoomTimeline::new(&log(events), ZoomConfig::default());
        let level = tl.segments[0].zoom_level.expect("a wide field zooms less");
        assert!((level - 0.6 / 0.7).abs() < 1e-9 || level == ZoomConfig::default().min_level, "{level}");
        // typing in one field, Tab to the next one lower down
        let tab = vec![focus(1000, 200.0, 200.0, 300.0, 40.0), key(2000), key(2300), focus(2500, 200.0, 400.0, 300.0, 40.0), key(2700)];
        let t = ZoomTimeline::new(&log(tab), ZoomConfig::default());
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].waypoints.len(), 2, "the camera moves to the new field: {:?}", t.segments[0].waypoints);
        assert!((t.sample(t.segments[0].end_ms).cy - 0.42).abs() < 0.03);
    }

    #[test]
    fn the_named_styles_exist_and_none_of_them_pumps() {
        assert!(ZoomConfig::preset("nonsense").is_none());
        for name in ["gentle", "standard", "punchy"] {
            let c = ZoomConfig::preset(name).unwrap();
            assert!(c.merge_gap_ms > c.ramp_in_ms + c.ramp_out_ms, "{name}: the merge gap must exceed both ramps");
            assert!(c.zoom_level >= c.min_level, "{name}");
        }
        let (g, s, p) =
            (ZoomConfig::preset("gentle").unwrap(), ZoomConfig::preset("standard").unwrap(), ZoomConfig::preset("punchy").unwrap());
        assert!(g.zoom_level < s.zoom_level && s.zoom_level < p.zoom_level);
        assert_eq!(s.zoom_level, ZoomConfig::default().zoom_level);
    }

    #[test]
    fn the_same_recording_always_gives_the_same_zooms() {
        let events =
            vec![click(900, 200.0, 200.0), mv(1500, 700.0, 600.0), key(3000), key(3200), click(9000, 800.0, 100.0), mv(9500, 100.0, 900.0)];
        let a = serde_json::to_string(&ZoomTimeline::new(&log(events.clone()), ZoomConfig::default())).unwrap();
        let b = serde_json::to_string(&ZoomTimeline::new(&log(events), ZoomConfig::default())).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn far_apart_clicks_are_separate() {
        let tl = ZoomTimeline::new(&log(vec![click(5000, 200.0, 200.0), click(15000, 800.0, 800.0)]), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 2);
    }

    #[test]
    fn typing_burst_is_one_trigger() {
        let keys = vec![1000, 1200, 1500, 5000].into_iter().map(|t_ms| InputEvent::Key { t_ms }).collect();
        let tl = ZoomTimeline::new(&log(keys), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 2);
    }

    #[test]
    fn clicks_outside_the_captured_area_do_not_zoom() {
        let tl = ZoomTimeline::new(
            &log(vec![click(5000, 1200.0, 200.0), click(9000, -5.0, 200.0), click(15000, 300.0, 1000.5)]),
            ZoomConfig::default(),
        );
        assert!(tl.segments.is_empty(), "{:?}", tl.segments);
        // an edge click is inside and still zooms
        let tl = ZoomTimeline::new(&log(vec![click(5000, 1000.0, 0.0)]), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1);
    }

    #[test]
    fn typing_while_the_pointer_is_outside_does_not_zoom_but_inside_does() {
        let mv = |t_ms, x, y| InputEvent::Move { t_ms, x, y };
        let key = |t_ms| InputEvent::Key { t_ms };
        // pointer parked outside, a burst of typing there: nothing to show
        let tl = ZoomTimeline::new(&log(vec![mv(0, 1500.0, 500.0), key(1000), key(1200)]), ZoomConfig::default());
        assert!(tl.segments.is_empty());
        // pointer comes back in, then a NEW burst: zooms to where the pointer is
        let tl = ZoomTimeline::new(&log(vec![mv(0, 1500.0, 500.0), key(1000), mv(3000, 250.0, 750.0), key(8000)]), ZoomConfig::default());
        assert_eq!(tl.segments.len(), 1);
        let peak = tl.sample(8000);
        assert!((peak.cx - 0.25).abs() < 1e-9 && (peak.cy - 0.75).abs() < 1e-9);
    }

    #[test]
    fn viewport_rect_stays_in_frame() {
        for &(cx, cy) in &[(0.0, 0.0), (1.0, 1.0), (0.5, 0.5), (0.95, 0.05)] {
            let r = Viewport { scale: 2.5, cx, cy }.rect();
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 1.0 + 1e-12 && r.y + r.h <= 1.0 + 1e-12);
        }
    }

    #[test]
    fn no_events_no_zoom() {
        let tl = ZoomTimeline::new(&log(vec![]), ZoomConfig::default());
        assert_eq!(tl.sample(1234), Viewport::FULL);
    }
}

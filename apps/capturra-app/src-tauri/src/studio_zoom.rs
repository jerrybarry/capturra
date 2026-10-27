//! Zoom segments in the Studio's shape, and back to the core's timeline for export.
//!
//! The core (`capturra-core::zoom`) works in normalized 0..1 positions with one `ZoomConfig`.
//! The Studio shows and edits segments as `{id, level, enabled, manual, t0, p0, p1, t1, focus}`
//! with positions in display points: ramp in `t0 -> p0`, hold `p0 -> p1`, ramp out `p1 -> t1`.
//! This mirrors `C.buildSegments` in `src/studio/zoom.ts`, so a project looks the same whether
//! the Studio or Rust built its segments. Rust stays the authority when a project has none saved.

use capturra_core::zoom::Waypoint;
use capturra_core::{EventLog, ZoomConfig, ZoomSegment, ZoomTimeline};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Focus {
    pub t: u64,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StudioSegment {
    pub id: String,
    pub level: f64,
    pub enabled: bool,
    pub manual: Option<Point>,
    pub t0: u64,
    pub p0: u64,
    pub p1: u64,
    pub t1: u64,
    pub focus: Vec<Focus>,
}

/// The core's segments for `log` (video time), expressed for the Studio.
pub fn studio_segments(log: &EventLog, config: ZoomConfig, duration_ms: u64) -> Vec<StudioSegment> {
    let timeline = ZoomTimeline::new(log, config);
    let (w, h) = (log.display.width, log.display.height);
    let mut segs: Vec<StudioSegment> = timeline
        .segments
        .iter()
        .enumerate()
        .map(|(i, s)| StudioSegment {
            id: format!("z{}", i + 1),
            level: s.zoom_level.unwrap_or(config.zoom_level),
            enabled: true,
            manual: None,
            t0: s.start_ms.saturating_sub(config.ramp_in_ms),
            p0: s.start_ms,
            p1: s.end_ms,
            t1: (s.end_ms + config.ramp_out_ms).min(duration_ms),
            focus: s.waypoints.iter().map(|wp| Focus { t: wp.t_ms, x: wp.x * w, y: wp.y * h }).collect(),
        })
        .collect();
    // Two segments' ramps must not overlap: split the gap between them in the middle.
    for i in 1..segs.len() {
        if segs[i].t0 < segs[i - 1].t1 {
            let mid = (segs[i - 1].p1 + segs[i].p0) / 2;
            segs[i].t0 = segs[i].p0.min(mid);
            segs[i - 1].t1 = segs[i - 1].p1.max(mid);
        }
    }
    for s in &mut segs {
        s.p1 = s.p1.min(s.t1);
        s.p0 = s.p0.min(s.p1);
    }
    segs
}

/// What the renderer needs from the Studio's edited segments: only the enabled ones, a manual
/// focus replacing the automatic path, and each segment's own zoom level and ramp lengths.
pub fn timeline_from_studio(segments: &[StudioSegment], display: (f64, f64)) -> ZoomTimeline {
    let mut on: Vec<&StudioSegment> = segments.iter().filter(|s| s.enabled).collect();
    on.sort_by_key(|s| s.p0);
    let norm = |x: f64, y: f64| ((x / display.0).clamp(0.0, 1.0), (y / display.1).clamp(0.0, 1.0));
    let segments = on
        .iter()
        .filter(|s| s.p1 > s.p0)
        .filter_map(|s| {
            let waypoints: Vec<Waypoint> = match s.manual {
                Some(m) => {
                    let (x, y) = norm(m.x, m.y);
                    vec![Waypoint { t_ms: s.p0, x, y }]
                }
                None => s
                    .focus
                    .iter()
                    .map(|f| {
                        let (x, y) = norm(f.x, f.y);
                        Waypoint { t_ms: f.t, x, y }
                    })
                    .collect(),
            };
            (!waypoints.is_empty()).then(|| ZoomSegment {
                zoom_level: Some(s.level),
                ramp_in_ms: Some(s.p0.saturating_sub(s.t0)),
                ramp_out_ms: Some(s.t1.saturating_sub(s.p1)),
                ..ZoomSegment::new(s.p0, s.p1, waypoints)
            })
        })
        .collect();
    ZoomTimeline { config: ZoomConfig::default(), segments }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capturra_core::{DisplayInfo, InputEvent, MouseButton};

    fn log_with_clicks(clicks: &[(u64, f64, f64)]) -> EventLog {
        let mut log = EventLog::new(DisplayInfo { width: 1000.0, height: 500.0, scale: 1.0 });
        for &(t, x, y) in clicks {
            log.events.push(InputEvent::Down { t_ms: t, x, y, button: MouseButton::Left });
        }
        log
    }

    #[test]
    fn a_click_becomes_a_segment_with_ramps_and_a_focus_in_points() {
        let cfg = ZoomConfig::default();
        let segs = studio_segments(&log_with_clicks(&[(5000, 500.0, 250.0)]), cfg, 20_000);
        assert_eq!(segs.len(), 1);
        let s = &segs[0];
        assert_eq!((s.p0, s.p1), (5000 - cfg.lead_ms, 5000 + cfg.hold_ms));
        assert_eq!((s.t0, s.t1), (s.p0 - cfg.ramp_in_ms, s.p1 + cfg.ramp_out_ms));
        assert_eq!(s.focus, vec![Focus { t: s.p0, x: 500.0, y: 250.0 }]);
        assert!(s.enabled && s.manual.is_none());
        assert_eq!(s.level, cfg.zoom_level);
    }

    #[test]
    fn ramps_are_clamped_to_the_video_and_never_before_zero() {
        let cfg = ZoomConfig::default();
        let segs = studio_segments(&log_with_clicks(&[(500, 10.0, 10.0), (9900, 900.0, 400.0)]), cfg, 10_000);
        assert_eq!(segs[0].t0, 0);
        let last = segs.last().unwrap();
        assert_eq!(last.t1, 10_000);
        assert!(last.p1 <= last.t1 && last.p0 <= last.p1);
    }

    #[test]
    fn neighbouring_ramps_never_overlap() {
        let cfg = ZoomConfig { merge_gap_ms: 0, ..Default::default() };
        // far enough apart not to merge, close enough that the ramps would collide
        let segs = studio_segments(&log_with_clicks(&[(2000, 100.0, 100.0), (4300, 900.0, 400.0)]), cfg, 20_000);
        assert_eq!(segs.len(), 2);
        assert!(segs[0].t1 <= segs[1].t0, "{:?}", segs);
        assert!(segs[0].p1 <= segs[0].t1 && segs[1].t0 <= segs[1].p0);
    }

    #[test]
    fn no_input_means_no_segments() {
        assert!(studio_segments(&log_with_clicks(&[]), ZoomConfig::default(), 5000).is_empty());
    }

    #[test]
    fn studio_segments_map_back_to_a_normalized_timeline() {
        let cfg = ZoomConfig::default();
        let mut segs = studio_segments(&log_with_clicks(&[(5000, 500.0, 250.0), (12_000, 100.0, 50.0)]), cfg, 20_000);
        let tl = timeline_from_studio(&segs, (1000.0, 500.0));
        assert_eq!(tl.segments.len(), 2);
        assert_eq!((tl.segments[0].waypoints[0].x, tl.segments[0].waypoints[0].y), (0.5, 0.5));
        assert_eq!(tl.segments[0].ramp_in_ms, Some(cfg.ramp_in_ms));
        // disabled segments are not rendered; a manual focus replaces the automatic path
        segs[0].enabled = false;
        segs[1].manual = Some(Point { x: 800.0, y: 100.0 });
        let tl = timeline_from_studio(&segs, (1000.0, 500.0));
        assert_eq!(tl.segments.len(), 1);
        assert_eq!(tl.segments[0].waypoints, vec![Waypoint { t_ms: segs[1].p0, x: 0.8, y: 0.2 }]);
    }

    #[test]
    fn each_segment_keeps_its_own_level_and_ramps() {
        let mut segs = studio_segments(&log_with_clicks(&[(5000, 500.0, 250.0), (15_000, 100.0, 50.0)]), ZoomConfig::default(), 30_000);
        segs[0].level = 1.5;
        segs[1].level = 3.0;
        segs[1].t0 = segs[1].p0 - 200;
        let tl = timeline_from_studio(&segs, (1000.0, 500.0));
        assert!((tl.sample(5000).scale - 1.5).abs() < 1e-9);
        assert!((tl.sample(15_000).scale - 3.0).abs() < 1e-9);
        assert_eq!(tl.segments[1].ramp_in_ms, Some(200));
    }

    #[test]
    fn an_empty_or_all_disabled_edit_gives_an_empty_timeline() {
        assert!(timeline_from_studio(&[], (1000.0, 500.0)).segments.is_empty());
        let mut segs = studio_segments(&log_with_clicks(&[(5000, 1.0, 1.0)]), ZoomConfig::default(), 20_000);
        segs[0].enabled = false;
        assert!(timeline_from_studio(&segs, (1000.0, 500.0)).segments.is_empty());
    }

    #[test]
    fn studio_json_shape_is_what_the_front_end_reads() {
        let segs = studio_segments(&log_with_clicks(&[(5000, 500.0, 250.0)]), ZoomConfig::default(), 20_000);
        let v = serde_json::to_value(&segs[0]).unwrap();
        for key in ["id", "level", "enabled", "manual", "t0", "p0", "p1", "t1", "focus"] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert!(v["manual"].is_null());
        assert_eq!(serde_json::from_value::<StudioSegment>(v).unwrap(), segs[0]);
    }
}

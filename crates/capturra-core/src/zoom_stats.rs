//! Measuring a zoom timeline against the recording it was made from, so the engine can be tuned on
//! real data instead of by feel. Everything here is deterministic.

use crate::events::{EventLog, InputEvent};
use crate::zoom::ZoomTimeline;

/// How often the camera is sampled, in ms of video time.
const STEP_MS: u64 = 50;
/// Below this scale the camera counts as "not zoomed".
const ZOOMED: f64 = 1.05;
/// A zoom-in this soon after a zoom-out is "pumping": the camera went out and straight back in.
const PUMP_GAP_MS: u64 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ZoomStats {
    pub duration_ms: u64,
    pub zoom_ins: usize,
    pub zoom_ins_per_min: f64,
    /// Share of the video that is zoomed in.
    pub coverage: f64,
    /// Share of the ZOOMED time during which the pointer is outside the visible area, i.e. the viewer
    /// cannot see the cursor.
    pub cursor_hidden: f64,
    /// Zoom-ins that start within `PUMP_GAP_MS` of the previous zoom ending.
    pub pumping: usize,
    /// Longest single zoom, ms.
    pub longest_ms: u64,
}

impl ZoomTimeline {
    pub fn stats(&self, log: &EventLog, duration_ms: u64) -> ZoomStats {
        let moves: Vec<(u64, f64, f64)> = log
            .sorted()
            .into_iter()
            .filter_map(|e| match e {
                InputEvent::Move { t_ms, x, y } | InputEvent::Down { t_ms, x, y, .. } | InputEvent::Up { t_ms, x, y, .. } => {
                    let (nx, ny) = log.normalize(x, y);
                    Some((t_ms, nx, ny))
                }
                _ => None,
            })
            .collect();
        let (mut zoomed, mut hidden, mut at) = (0u64, 0u64, 0usize);
        let mut t = 0;
        while t < duration_ms {
            let vp = self.sample(t);
            if vp.scale > ZOOMED {
                zoomed += STEP_MS;
                while at + 1 < moves.len() && moves[at + 1].0 <= t {
                    at += 1;
                }
                if let Some(&(mt, px, py)) = moves.get(at).filter(|m| m.0 <= t) {
                    let _ = mt;
                    let r = vp.rect();
                    if px < r.x || px > r.x + r.w || py < r.y || py > r.y + r.h {
                        hidden += STEP_MS;
                    }
                }
            }
            t += STEP_MS;
        }
        let mut segs: Vec<_> = self.segments.iter().collect();
        segs.sort_by_key(|s| s.start_ms);
        let pumping = segs.windows(2).filter(|w| w[1].start_ms.saturating_sub(w[0].end_ms) <= PUMP_GAP_MS).count();
        let minutes = (duration_ms as f64 / 60_000.0).max(1e-9);
        ZoomStats {
            duration_ms,
            zoom_ins: segs.len(),
            zoom_ins_per_min: segs.len() as f64 / minutes,
            coverage: if duration_ms == 0 { 0.0 } else { zoomed as f64 / duration_ms as f64 },
            cursor_hidden: if zoomed == 0 { 0.0 } else { hidden as f64 / zoomed as f64 },
            pumping,
            longest_ms: segs.iter().map(|s| s.end_ms - s.start_ms).max().unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{DisplayInfo, MouseButton};
    use crate::zoom::ZoomConfig;

    fn log(events: Vec<InputEvent>) -> EventLog {
        let mut l = EventLog::new(DisplayInfo { width: 1000.0, height: 1000.0, scale: 1.0 });
        l.events = events;
        l
    }

    fn click(t_ms: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Down { t_ms, x, y, button: MouseButton::Left }
    }

    #[test]
    fn no_events_means_no_zoom_and_nothing_to_report() {
        let l = log(vec![]);
        let s = ZoomTimeline::new(&l, ZoomConfig::default()).stats(&l, 10_000);
        assert_eq!((s.zoom_ins, s.pumping, s.longest_ms), (0, 0, 0));
        assert_eq!((s.coverage, s.cursor_hidden), (0.0, 0.0));
    }

    #[test]
    fn coverage_counts_the_zoomed_share_and_a_cursor_that_wanders_off_is_hidden() {
        // one click in the top-left, then the pointer runs to the bottom-right while the camera holds
        let l = log(vec![
            InputEvent::Move { t_ms: 0, x: 100.0, y: 100.0 },
            click(1000, 100.0, 100.0),
            InputEvent::Move { t_ms: 1200, x: 900.0, y: 900.0 },
        ]);
        let fixed = ZoomConfig { follow: false, ..Default::default() };
        let tl = ZoomTimeline::new(&l, fixed);
        let s = tl.stats(&l, 10_000);
        assert_eq!(s.zoom_ins, 1);
        assert!(s.coverage > 0.15 && s.coverage < 0.40, "{s:?}");
        assert!(s.cursor_hidden > 0.5, "the pointer left the zoomed area: {s:?}");
        // the camera that follows keeps it in view for most of that time
        let followed = ZoomTimeline::new(&l, ZoomConfig::default()).stats(&l, 10_000);
        assert!(followed.cursor_hidden < s.cursor_hidden / 2.0, "{followed:?} vs {s:?}");
        // the same recording with the pointer staying put keeps the cursor in view
        let l2 = log(vec![InputEvent::Move { t_ms: 0, x: 100.0, y: 100.0 }, click(1000, 100.0, 100.0)]);
        let s2 = ZoomTimeline::new(&l2, ZoomConfig::default()).stats(&l2, 10_000);
        assert_eq!(s2.cursor_hidden, 0.0, "{s2:?}");
    }

    #[test]
    fn two_zooms_close_together_are_pumping_and_far_apart_are_not() {
        let cfg = ZoomConfig { merge_gap_ms: 100, follow: false, ..Default::default() };
        let near = log(vec![click(1000, 200.0, 200.0), click(3500, 700.0, 700.0)]);
        assert_eq!(ZoomTimeline::new(&near, cfg).stats(&near, 12_000).pumping, 1);
        let far = log(vec![click(1000, 200.0, 200.0), click(9000, 700.0, 700.0)]);
        assert_eq!(ZoomTimeline::new(&far, cfg).stats(&far, 12_000).pumping, 0);
    }
}

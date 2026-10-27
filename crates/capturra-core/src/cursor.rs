use crate::easing::lerp;
use crate::events::{CursorImage, EventLog, InputEvent};

/// Smoothed cursor path sampled at a fixed step, in normalized 0..=1 coords.
/// The recording hides the real cursor; the renderer redraws it from this.
///
/// While the real pointer is outside the captured area (a region or window recording) the
/// cursor is not visible: `visible_at` says so and the path follows the pointer's nearest
/// point on the edge without smoothing, so it re-enters from where it left, not from afar.
pub struct CursorTrack {
    step_ms: u64,
    /// (x, y, pointer is inside the captured area)
    samples: Vec<(f64, f64, bool)>,
}

impl CursorTrack {
    /// `half_life_ms`: time for the smoothed cursor to cover half the distance to
    /// the raw cursor. Lower = snappier, higher = floatier. ~60ms feels good.
    pub fn build(log: &EventLog, duration_ms: u64, step_ms: u64, half_life_ms: f64) -> Self {
        let events = log.sorted();
        let n = (duration_ms / step_ms) as usize + 2;
        let alpha = 1.0 - 0.5f64.powf(step_ms as f64 / half_life_ms.max(1.0));

        let mut raw = (0.5, 0.5);
        let mut inside = true;
        let mut idx = 0;
        // Start from the first known position so we don't swoop in from center.
        if let Some(p) = events.iter().find_map(|e| e.position()) {
            raw = log.normalize(p.0, p.1);
            inside = log.contains(p.0, p.1);
        }
        let mut smooth = raw;
        let mut samples = Vec::with_capacity(n);

        for i in 0..n {
            let t = i as u64 * step_ms;
            while idx < events.len() && events[idx].t_ms() <= t {
                if let Some((x, y)) = events[idx].position() {
                    raw = log.normalize(x, y);
                    inside = log.contains(x, y);
                }
                idx += 1;
            }
            if inside {
                smooth.0 += (raw.0 - smooth.0) * alpha;
                smooth.1 += (raw.1 - smooth.1) * alpha;
            } else {
                smooth = raw;
            }
            samples.push((smooth.0, smooth.1, inside));
        }
        Self { step_ms, samples }
    }

    pub fn at(&self, t_ms: u64) -> (f64, f64) {
        let f = t_ms as f64 / self.step_ms as f64;
        let i = f.floor() as usize;
        let frac = f - f.floor();
        let a = self.samples[i.min(self.samples.len() - 1)];
        let b = self.samples[(i + 1).min(self.samples.len() - 1)];
        (lerp(a.0, b.0, frac), lerp(a.1, b.1, frac))
    }

    /// Whether the pointer is inside the captured area at `t_ms`. When it is not, draw no cursor.
    pub fn visible_at(&self, t_ms: u64) -> bool {
        let i = (t_ms / self.step_ms) as usize;
        self.samples[i.min(self.samples.len() - 1)].2
    }
}

/// Which cursor image is showing at a given time, from the log's `CursorChange` events.
pub struct CursorTimeline<'a> {
    cursors: &'a [CursorImage],
    /// (t_ms, index into `cursors`), sorted by time.
    changes: Vec<(u64, usize)>,
    /// (t_ms, pointer is over something clickable), sorted by time.
    hovers: Vec<(u64, bool)>,
    /// The pointing-hand image, if the recording carries one.
    hand: Option<usize>,
}

impl<'a> CursorTimeline<'a> {
    /// Changes naming an id that is not in the table are ignored.
    pub fn new(log: &'a EventLog) -> Self {
        let changes = log
            .sorted()
            .into_iter()
            .filter_map(|e| match e {
                InputEvent::CursorChange { t_ms, id } => log.cursors.iter().position(|c| c.id == id).map(|i| (t_ms, i)),
                _ => None,
            })
            .collect();
        let hovers = log
            .sorted()
            .into_iter()
            .filter_map(|e| match e {
                InputEvent::Hover { t_ms, clickable } => Some((t_ms, clickable)),
                _ => None,
            })
            .collect();
        let hand = log.cursors.iter().position(|c| c.role.as_deref() == Some(CursorImage::POINTING_HAND));
        Self { cursors: &log.cursors, changes, hovers, hand }
    }

    /// Whether the recording knows when the pointer is over something clickable, and has the hand to show
    /// for it. Older recordings do not.
    pub fn has_hand(&self) -> bool {
        self.hand.is_some() && !self.hovers.is_empty()
    }

    /// Whether the pointer is over something clickable at `t_ms`.
    pub fn clickable_at(&self, t_ms: u64) -> bool {
        let n = self.hovers.partition_point(|&(t, _)| t <= t_ms);
        n > 0 && self.hovers[n - 1].1
    }

    /// Like `at`, but with the pointing hand while the pointer is over something clickable, when
    /// `hand_over_clickable` is on and the recording has the data for it.
    pub fn styled_at(&self, t_ms: u64, hand_over_clickable: bool) -> Option<&'a CursorImage> {
        if hand_over_clickable && self.clickable_at(t_ms) {
            if let Some(h) = self.hand {
                return Some(&self.cursors[h]);
            }
        }
        self.at(t_ms)
    }

    /// The image showing at `t_ms`. Before the first change it is the first one recorded
    /// (the cursor must have looked like something at the start). `None` when the log has
    /// no cursor images at all, e.g. a version 1 file: draw the fallback circle.
    pub fn at(&self, t_ms: u64) -> Option<&'a CursorImage> {
        let first = self.changes.first()?;
        let n = self.changes.partition_point(|&(t, _)| t <= t_ms);
        let (_, idx) = if n == 0 { first } else { &self.changes[n - 1] };
        Some(&self.cursors[*idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{DisplayInfo, InputEvent};

    #[test]
    fn settles_on_final_position() {
        let mut log = EventLog::new(DisplayInfo { width: 1000.0, height: 1000.0, scale: 1.0 });
        log.events.push(InputEvent::Move { t_ms: 0, x: 0.0, y: 0.0 });
        log.events.push(InputEvent::Move { t_ms: 100, x: 500.0, y: 500.0 });
        log.events.push(InputEvent::Key { t_ms: 3000 });
        let track = CursorTrack::build(&log, 3000, 4, 60.0);
        let (x, y) = track.at(2900);
        assert!((x - 0.5).abs() < 0.01 && (y - 0.5).abs() < 0.01);
        // And it should lag behind the raw jump at t=110.
        assert!(track.at(110).0 < 0.4);
    }

    #[test]
    fn cursor_is_hidden_while_the_pointer_is_outside_the_area_and_returns_on_entry() {
        let mut log = EventLog::new(DisplayInfo { width: 100.0, height: 100.0, scale: 1.0 });
        log.events.push(InputEvent::Move { t_ms: 0, x: 50.0, y: 50.0 });
        log.events.push(InputEvent::Move { t_ms: 1000, x: 150.0, y: 50.0 }); // leaves to the right
        log.events.push(InputEvent::Move { t_ms: 2000, x: 150.0, y: 90.0 }); // still outside, lower
        log.events.push(InputEvent::Move { t_ms: 3000, x: 40.0, y: 90.0 }); // back inside
        let track = CursorTrack::build(&log, 4000, 4, 60.0);
        assert!(track.visible_at(500));
        assert!(!track.visible_at(1004) && !track.visible_at(2500));
        assert!(track.visible_at(3004));
        // Outside, the path tracks the nearest edge point immediately (no smoothing lag), so
        // after the 2000 ms move it is at the right edge, y = 0.9.
        let (x, y) = track.at(2008);
        assert!((x - 1.0).abs() < 1e-9 && (y - 0.9).abs() < 1e-9, "{x},{y}");
    }

    #[test]
    fn cursor_starting_outside_is_hidden_from_the_first_sample() {
        let mut log = EventLog::new(DisplayInfo { width: 100.0, height: 100.0, scale: 1.0 });
        log.events.push(InputEvent::Move { t_ms: 0, x: -30.0, y: 20.0 });
        let track = CursorTrack::build(&log, 100, 4, 60.0);
        assert!(!track.visible_at(0));
    }

    #[test]
    fn a_pointer_that_stays_inside_is_always_visible() {
        let mut log = EventLog::new(DisplayInfo { width: 100.0, height: 100.0, scale: 1.0 });
        log.events.push(InputEvent::Move { t_ms: 0, x: 0.0, y: 0.0 }); // the corner counts as inside
        log.events.push(InputEvent::Move { t_ms: 50, x: 100.0, y: 100.0 });
        let track = CursorTrack::build(&log, 200, 4, 60.0);
        assert!((0..200).step_by(4).all(|t| track.visible_at(t)));
    }

    fn img(id: u32) -> crate::events::CursorImage {
        crate::events::CursorImage {
            id,
            file: format!("cursors/{id}.png"),
            hotspot: crate::events::Hotspot { x: 0.0, y: 0.0 },
            scale: 1.0,
            size: crate::events::SizePt { w: 10.0, h: 10.0 },
            role: None,
        }
    }

    #[test]
    fn the_hand_shows_over_clickable_things_only_when_asked_and_only_if_the_recording_has_one() {
        let mut log = log_with(&[(0, 0)], &[0, 1]);
        log.cursors[1].role = Some(CursorImage::POINTING_HAND.to_string());
        log.events.push(InputEvent::Hover { t_ms: 1000, clickable: true });
        log.events.push(InputEvent::Hover { t_ms: 2000, clickable: false });
        let tl = CursorTimeline::new(&log);
        assert!(tl.has_hand());
        let id = |t, on| tl.styled_at(t, on).unwrap().id;
        assert_eq!(id(500, true), 0, "not over anything clickable yet");
        assert_eq!(id(1500, true), 1, "over a button: the hand");
        assert_eq!(id(1500, false), 0, "the user switched it off");
        assert_eq!(id(2500, true), 0, "left it again");
        assert!(tl.clickable_at(1000) && !tl.clickable_at(999) && !tl.clickable_at(2000));
        // a recording without hover data, or without the hand, never shows one
        let plain = log_with(&[(0, 0)], &[0, 1]);
        assert!(!CursorTimeline::new(&plain).has_hand());
        let mut no_sprite = log_with(&[(0, 0)], &[0]);
        no_sprite.events.push(InputEvent::Hover { t_ms: 0, clickable: true });
        assert_eq!(CursorTimeline::new(&no_sprite).styled_at(500, true).unwrap().id, 0);
        assert!(!CursorTimeline::new(&no_sprite).has_hand());
    }

    fn log_with(changes: &[(u64, u32)], table: &[u32]) -> EventLog {
        let mut log = EventLog::new(DisplayInfo { width: 100.0, height: 100.0, scale: 1.0 });
        log.cursors = table.iter().map(|&i| img(i)).collect();
        // Deliberately unsorted to prove the timeline sorts.
        for &(t_ms, id) in changes.iter().rev() {
            log.events.push(InputEvent::CursorChange { t_ms, id });
        }
        log
    }

    #[test]
    fn timeline_picks_latest_change_at_or_before_t() {
        let log = log_with(&[(0, 7), (500, 9), (900, 7)], &[7, 9]);
        let tl = CursorTimeline::new(&log);
        assert_eq!(tl.at(0).unwrap().id, 7);
        assert_eq!(tl.at(499).unwrap().id, 7);
        assert_eq!(tl.at(500).unwrap().id, 9);
        assert_eq!(tl.at(899).unwrap().id, 9);
        assert_eq!(tl.at(900).unwrap().id, 7);
        assert_eq!(tl.at(1_000_000).unwrap().id, 7);
    }

    #[test]
    fn timeline_before_first_change_uses_the_first_cursor() {
        let log = log_with(&[(300, 4), (800, 5)], &[4, 5]);
        assert_eq!(CursorTimeline::new(&log).at(0).unwrap().id, 4);
    }

    #[test]
    fn timeline_is_empty_for_logs_without_cursors() {
        let log = log_with(&[], &[]);
        assert!(CursorTimeline::new(&log).at(100).is_none());
        // A change naming an id missing from the table is ignored, not a panic.
        let log = log_with(&[(0, 3)], &[]);
        assert!(CursorTimeline::new(&log).at(100).is_none());
    }
}

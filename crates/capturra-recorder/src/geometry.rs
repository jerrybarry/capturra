//! Capture-area geometry. Platform independent, so the coordinate mapping is unit-tested anywhere.
//!
//! Input events arrive in GLOBAL display points (origin = top-left of the main display; a display
//! to the left of or above the main one has negative coordinates). The video only shows the
//! captured area: a whole display, a region of one, or a window. `events.json` must describe the
//! video, so events are converted to AREA-LOCAL points (origin = top-left of the video) and
//! `DisplayInfo.width/height` become the area's size in points. Renderers never need to know
//! which kind of source was recorded.

use capturra_core::InputEvent;

/// A rectangle in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PtRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PtRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Whether a point (same space as the rectangle) is inside it. Edges count.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
}

/// What to record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Source {
    /// The n-th display in `capturra record --list` (0 = the main display).
    Display(usize),
    /// A window by its id from `--list`.
    Window(u32),
    /// A rectangle of a display, in points from that display's top-left corner.
    Region { display: usize, rect: PtRect },
}

/// Parse `x,y,w,h` (points, relative to the display's top-left).
pub fn parse_region(s: &str) -> Result<PtRect, String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.len() != 4 {
        return Err(format!("expected x,y,w,h (four numbers separated by commas), got {s:?}"));
    }
    let mut n = [0.0f64; 4];
    for (slot, part) in n.iter_mut().zip(&parts) {
        *slot = part.parse::<f64>().map_err(|_| format!("{part:?} is not a number (in --region {s:?})"))?;
        if !slot.is_finite() {
            return Err(format!("{part:?} is not a finite number (in --region {s:?})"));
        }
    }
    let [x, y, w, h] = n;
    if x < 0.0 || y < 0.0 {
        return Err(format!("--region x and y must be 0 or more (points from the display's top-left), got {x},{y}"));
    }
    if w < 2.0 || h < 2.0 {
        return Err(format!("--region width and height must be at least 2 points, got {w}x{h}"));
    }
    Ok(PtRect::new(x, y, w, h))
}

/// Snap a region to whole points with an even width and height (video encoders need even sizes,
/// and the capture library would round an odd size up on its own) and check that it lies inside a
/// display of `display_w` x `display_h` points. Errors instead of clamping: a region that does
/// not fit is a typo, and silently recording something else would waste a take.
pub fn fit_region(r: PtRect, display_w: f64, display_h: f64) -> Result<PtRect, String> {
    let (x, y) = (r.x.round(), r.y.round());
    let even = |len: f64, start: f64, limit: f64| -> f64 {
        let n = len.round();
        if n % 2.0 == 0.0 {
            n
        } else if start + n + 1.0 <= limit {
            n + 1.0
        } else {
            n - 1.0
        }
    };
    let (w, h) = (even(r.w, x, display_w), even(r.h, y, display_h));
    if w < 2.0 || h < 2.0 {
        return Err(format!("region {}x{} points is too small", r.w, r.h));
    }
    if x + w > display_w || y + h > display_h {
        return Err(format!(
            "region {x},{y},{w},{h} extends past the display, which is {display_w} x {display_h} points \
             (x+w must be at most {display_w} and y+h at most {display_h})"
        ));
    }
    Ok(PtRect::new(x, y, w, h))
}

/// Where the top-left of the captured area is, in global points, over time. A display or region
/// never moves (one sample); a window can be dragged, so the recorder samples it.
#[derive(Debug, Clone, PartialEq)]
pub struct OriginTrack {
    /// (clock ms, global x, global y), in time order.
    samples: Vec<(u64, f64, f64)>,
}

impl OriginTrack {
    pub fn fixed(x: f64, y: f64) -> Self {
        Self { samples: vec![(0, x, y)] }
    }

    /// Build from samples; sorts them by time. Needs at least one.
    pub fn from_samples(mut samples: Vec<(u64, f64, f64)>) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        samples.sort_by_key(|s| s.0);
        Some(Self { samples })
    }

    /// Origin at `t_ms`: linear between the two samples around it, the first sample before the
    /// first, the last after the last.
    pub fn at(&self, t_ms: u64) -> (f64, f64) {
        let s = &self.samples;
        let i = s.partition_point(|&(t, _, _)| t <= t_ms);
        if i == 0 {
            return (s[0].1, s[0].2);
        }
        if i == s.len() {
            let l = s[i - 1];
            return (l.1, l.2);
        }
        let (a, b) = (s[i - 1], s[i]);
        let f = (t_ms - a.0) as f64 / (b.0 - a.0).max(1) as f64;
        (a.1 + (b.1 - a.1) * f, a.2 + (b.2 - a.2) * f)
    }

    /// True if the origin ever differs from its first value by more than half a point.
    pub fn moved(&self) -> bool {
        let first = self.samples[0];
        self.samples.iter().any(|s| (s.1 - first.1).abs() > 0.5 || (s.2 - first.2).abs() > 0.5)
    }
}

/// How long before the stop was noticed a click still counts as the click that stopped the
/// recording (press-to-release plus the pump noticing the flag).
pub const STOP_CLICK_WINDOW_MS: u64 = 1500;

/// Remove the click that stopped the recording: the last button press within `window_ms` before
/// `stop_ms`, and its release. Without this, clicking the menu-bar icon or Stop button becomes the
/// last click of the recording and the zoom pans to it. Returns how many events were removed.
/// Pointer moves are kept.
pub fn drop_stop_click(events: &mut Vec<InputEvent>, stop_ms: u64, window_ms: u64) -> usize {
    let Some(pos) = events.iter().rposition(|e| matches!(e, InputEvent::Down { .. })) else {
        return 0;
    };
    let InputEvent::Down { t_ms, button, .. } = events[pos] else { return 0 };
    if stop_ms.saturating_sub(t_ms) > window_ms {
        return 0;
    }
    let before = events.len();
    // The release, if it was captured, is the first Up of that button after the press.
    let up = events[pos + 1..].iter().position(|e| matches!(e, InputEvent::Up { button: b, .. } if *b == button)).map(|i| pos + 1 + i);
    if let Some(i) = up {
        events.remove(i);
    }
    events.remove(pos);
    before - events.len()
}

/// The input listener delivers whatever pointer movement was queued before it started in one
/// burst, all stamped with the same first millisecond. Those moves carry no timing (they are
/// earlier motion squeezed into one instant), so keep only the last of them: where the pointer
/// was when the recording began. Returns how many were removed.
pub fn collapse_start_burst(events: &mut Vec<InputEvent>) -> usize {
    let Some(first) = events.first().map(InputEvent::t_ms) else { return 0 };
    let run = events.iter().take_while(|e| e.t_ms() == first && matches!(e, InputEvent::Move { .. })).count();
    if run < 2 {
        return 0;
    }
    events.drain(..run - 1);
    run - 1
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub events: Vec<InputEvent>,
    /// Button presses and releases that happened outside the captured area. They are removed:
    /// the video shows nothing for them, and a click there must not zoom to the nearest edge.
    pub dropped_outside: u64,
    /// Pointer moves outside the area. Kept (the cursor leaving and re-entering is real) at their
    /// out-of-area coordinates; renderers clamp them to the edge.
    pub moves_outside: u64,
}

/// Convert events from global points to area-local points. `track` gives the area's origin at
/// each event's time (clock time, the same clock the events carry); `size` is the area's size in
/// points. Key and cursor-change events carry no position and pass through unchanged; a focus
/// frame moves with the area and becomes "no field" when it is not in the area.
pub fn map_events(events: &[InputEvent], track: &OriginTrack, size: (f64, f64)) -> Mapped {
    let area = PtRect::new(0.0, 0.0, size.0, size.1);
    let mut out = Vec::with_capacity(events.len());
    let (mut dropped, mut moves_outside) = (0, 0);
    for e in events {
        let mut e = e.clone();
        match &mut e {
            InputEvent::Move { t_ms, x, y } => {
                let (ox, oy) = track.at(*t_ms);
                (*x, *y) = (*x - ox, *y - oy);
                if !area.contains(*x, *y) {
                    moves_outside += 1;
                }
            }
            InputEvent::Down { t_ms, x, y, .. } | InputEvent::Up { t_ms, x, y, .. } => {
                let (ox, oy) = track.at(*t_ms);
                (*x, *y) = (*x - ox, *y - oy);
                if !area.contains(*x, *y) {
                    dropped += 1;
                    continue;
                }
            }
            InputEvent::Focus { t_ms, x, y, w, h } => {
                let (ox, oy) = track.at(*t_ms);
                (*x, *y) = (*x - ox, *y - oy);
                // A field whose centre is outside the area is not in the video: say "no field" instead
                // (dropping the event would leave the previous field looking focused).
                if !area.contains(*x + *w / 2.0, *y + *h / 2.0) {
                    (*x, *y, *w, *h) = (0.0, 0.0, 0.0, 0.0);
                }
            }
            InputEvent::Key { .. } | InputEvent::CursorChange { .. } | InputEvent::Hover { .. } => {}
        }
        out.push(e);
    }
    Mapped { events: out, dropped_outside: dropped, moves_outside }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capturra_core::MouseButton;

    fn down(t_ms: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Down { t_ms, x, y, button: MouseButton::Left }
    }

    fn up(t_ms: u64, x: f64, y: f64) -> InputEvent {
        InputEvent::Up { t_ms, x, y, button: MouseButton::Left }
    }

    #[test]
    fn stop_click_is_dropped_with_its_release_and_moves_are_kept() {
        let mut ev = vec![
            down(1000, 10.0, 10.0),
            up(1100, 10.0, 10.0),
            InputEvent::Move { t_ms: 2000, x: 900.0, y: 5.0 },
            down(2200, 900.0, 5.0),
            up(2350, 900.0, 5.0),
        ];
        assert_eq!(drop_stop_click(&mut ev, 2400, STOP_CLICK_WINDOW_MS), 2);
        assert_eq!(ev.len(), 3);
        assert!(matches!(ev[2], InputEvent::Move { t_ms: 2000, .. }));
        assert_eq!(ev.iter().filter(|e| matches!(e, InputEvent::Down { .. })).count(), 1);
    }

    #[test]
    fn a_click_long_before_the_stop_is_real_content() {
        let mut ev = vec![down(1000, 10.0, 10.0), up(1100, 10.0, 10.0)];
        assert_eq!(drop_stop_click(&mut ev, 1100 + STOP_CLICK_WINDOW_MS + 500, STOP_CLICK_WINDOW_MS), 0);
        assert_eq!(ev.len(), 2);
    }

    #[test]
    fn stop_click_without_a_captured_release_and_no_clicks_at_all() {
        let mut ev = vec![down(900, 1.0, 1.0)];
        assert_eq!(drop_stop_click(&mut ev, 1000, STOP_CLICK_WINDOW_MS), 1);
        assert!(ev.is_empty());
        let mut none: Vec<InputEvent> = vec![InputEvent::Move { t_ms: 5, x: 0.0, y: 0.0 }];
        assert_eq!(drop_stop_click(&mut none, 10, STOP_CLICK_WINDOW_MS), 0);
    }

    #[test]
    fn a_burst_of_moves_stamped_with_the_first_millisecond_collapses_to_the_last() {
        let mv = |t, x| InputEvent::Move { t_ms: t, x, y: 0.0 };
        let mut ev = vec![mv(0, 1.0), mv(0, 2.0), mv(0, 3.0), mv(3, 4.0), mv(11, 5.0)];
        assert_eq!(collapse_start_burst(&mut ev), 2);
        assert_eq!(ev, vec![mv(0, 3.0), mv(3, 4.0), mv(11, 5.0)]);
        // a single first move, a first click, or nothing at all is left alone
        let mut one = vec![mv(0, 1.0), mv(5, 2.0)];
        assert_eq!(collapse_start_burst(&mut one), 0);
        let mut click = vec![down(0, 1.0, 1.0), mv(0, 2.0)];
        assert_eq!(collapse_start_burst(&mut click), 0);
        assert_eq!(collapse_start_burst(&mut Vec::new()), 0);
        // a burst that is not at the very start (same timestamp later on) is real motion data
        let mut later = vec![mv(0, 1.0), mv(40, 2.0), mv(40, 3.0)];
        assert_eq!(collapse_start_burst(&mut later), 0);
    }

    #[test]
    fn region_parses_four_numbers() {
        assert_eq!(parse_region("100, 50,800,600").unwrap(), PtRect::new(100.0, 50.0, 800.0, 600.0));
        assert!(parse_region("1,2,3").unwrap_err().contains("four numbers"));
        assert!(parse_region("1,2,3,x").unwrap_err().contains("not a number"));
        assert!(parse_region("-1,0,10,10").unwrap_err().contains("0 or more"));
        assert!(parse_region("0,0,0,10").unwrap_err().contains("at least 2"));
        assert!(parse_region("0,0,NaN,10").is_err());
    }

    #[test]
    fn region_is_snapped_to_whole_even_points() {
        let r = fit_region(PtRect::new(10.4, 20.6, 801.0, 600.0), 2048.0, 1280.0).unwrap();
        assert_eq!(r, PtRect::new(10.0, 21.0, 802.0, 600.0), "odd width grows by one");
        // at the display edge the odd size shrinks instead of growing past it
        let r = fit_region(PtRect::new(1247.0, 0.0, 801.0, 100.0), 2048.0, 1280.0).unwrap();
        assert_eq!(r.w, 800.0);
    }

    #[test]
    fn region_past_the_display_is_an_error_not_a_clamp() {
        let e = fit_region(PtRect::new(1500.0, 0.0, 800.0, 600.0), 2048.0, 1280.0).unwrap_err();
        assert!(e.contains("extends past the display") && e.contains("2048 x 1280"), "{e}");
        assert!(fit_region(PtRect::new(0.0, 700.0, 100.0, 600.0), 2048.0, 1280.0).is_err());
        // exactly filling the display is fine
        assert!(fit_region(PtRect::new(0.0, 0.0, 2048.0, 1280.0), 2048.0, 1280.0).is_ok());
    }

    #[test]
    fn a_fixed_origin_shifts_every_position_by_the_area_origin() {
        // a region at (300, 200) on the main display
        let track = OriginTrack::fixed(300.0, 200.0);
        let events = vec![
            InputEvent::Move { t_ms: 1, x: 400.0, y: 250.0 },
            down(2, 500.0, 300.0),
            InputEvent::Key { t_ms: 3 },
            InputEvent::CursorChange { t_ms: 4, id: 7 },
        ];
        let m = map_events(&events, &track, (800.0, 600.0));
        assert_eq!(m.events[0], InputEvent::Move { t_ms: 1, x: 100.0, y: 50.0 });
        assert_eq!(m.events[1], down(2, 200.0, 100.0));
        assert_eq!(m.events[2], InputEvent::Key { t_ms: 3 });
        assert_eq!(m.events[3], InputEvent::CursorChange { t_ms: 4, id: 7 });
        assert_eq!((m.dropped_outside, m.moves_outside), (0, 0));
    }

    #[test]
    fn a_display_left_of_the_main_one_has_negative_global_origin() {
        // secondary display 1440 pt wide sitting to the left of the main display
        let track = OriginTrack::fixed(-1440.0, 0.0);
        let m = map_events(&[down(0, -1000.0, 300.0)], &track, (1440.0, 900.0));
        assert_eq!(m.events, vec![down(0, 440.0, 300.0)]);
    }

    #[test]
    fn clicks_outside_the_area_are_dropped_and_counted_moves_are_kept() {
        let track = OriginTrack::fixed(100.0, 100.0);
        let events = vec![
            InputEvent::Move { t_ms: 1, x: 50.0, y: 50.0 },                          // outside, kept
            down(2, 50.0, 50.0),                                                     // outside, dropped
            InputEvent::Up { t_ms: 3, x: 50.0, y: 50.0, button: MouseButton::Left }, // dropped
            down(4, 150.0, 150.0),                                                   // inside
            down(5, 100.0 + 400.0, 100.0 + 300.0),                                   // exactly on the far corner: inside
            down(6, 100.0 + 401.0, 150.0),                                           // one point past the right edge
        ];
        let m = map_events(&events, &track, (400.0, 300.0));
        assert_eq!(m.dropped_outside, 3);
        assert_eq!(m.moves_outside, 1);
        assert_eq!(m.events.len(), 3);
        assert_eq!(m.events[0], InputEvent::Move { t_ms: 1, x: -50.0, y: -50.0 });
        assert_eq!(m.events[1], down(4, 50.0, 50.0));
    }

    #[test]
    fn a_moving_window_is_followed_with_linear_interpolation() {
        // window dragged right by 100 pt between 1000 ms and 1100 ms
        let track = OriginTrack::from_samples(vec![(0, 200.0, 100.0), (1000, 200.0, 100.0), (1100, 300.0, 100.0)]).unwrap();
        assert_eq!(track.at(500), (200.0, 100.0));
        assert_eq!(track.at(1050), (250.0, 100.0));
        assert_eq!(track.at(1100), (300.0, 100.0));
        assert_eq!(track.at(9999), (300.0, 100.0), "after the last sample the window stays put");
        assert!(track.moved());
        // the pointer is held still on screen at x=450 while the window slides under it:
        // its position inside the window moves left as the window moves right
        let events = vec![
            InputEvent::Move { t_ms: 900, x: 450.0, y: 150.0 },
            InputEvent::Move { t_ms: 1050, x: 450.0, y: 150.0 },
            InputEvent::Move { t_ms: 1200, x: 450.0, y: 150.0 },
        ];
        let xs: Vec<f64> = map_events(&events, &track, (600.0, 400.0)).events.iter().map(|e| e.position().unwrap().0).collect();
        assert_eq!(xs, vec![250.0, 200.0, 150.0]);
    }

    #[test]
    fn before_the_first_sample_the_first_origin_is_used_and_a_static_track_never_moved() {
        let track = OriginTrack::from_samples(vec![(500, 10.0, 20.0), (0, 10.0, 20.0)]).unwrap();
        assert_eq!(track.at(0), (10.0, 20.0));
        assert!(!track.moved());
        assert!(OriginTrack::from_samples(vec![]).is_none());
        assert_eq!(OriginTrack::fixed(5.0, 6.0).at(123456), (5.0, 6.0));
    }

    #[test]
    fn a_click_normalizes_to_its_spot_in_the_region_video() {
        // The point of the mapping: a click on an on-screen button must normalize to the spot of
        // the VIDEO where that button appears. Button at global (700, 500); region (600,400,400,300).
        use capturra_core::{DisplayInfo, EventLog};
        let track = OriginTrack::fixed(600.0, 400.0);
        let m = map_events(&[down(0, 700.0, 500.0)], &track, (400.0, 300.0));
        let mut log = EventLog::new(DisplayInfo { width: 400.0, height: 300.0, scale: 2.0 });
        log.events = m.events;
        let (x, y) = log.events[0].position().unwrap();
        assert_eq!(log.normalize(x, y), (0.25, 100.0 / 300.0));
    }

    #[test]
    fn a_focus_frame_moves_with_the_area_and_is_cleared_when_it_is_not_in_it() {
        let track = OriginTrack::fixed(100.0, 50.0);
        let focus = |t_ms, x, y, w, h| InputEvent::Focus { t_ms, x, y, w, h };
        let m = map_events(
            &[focus(1, 300.0, 150.0, 200.0, 40.0), focus(2, 5000.0, 150.0, 200.0, 40.0), focus(3, 0.0, 0.0, 0.0, 0.0)],
            &track,
            (800.0, 600.0),
        );
        assert_eq!(m.events[0], focus(1, 200.0, 100.0, 200.0, 40.0), "shifted by the origin, size unchanged");
        assert_eq!(m.events[1], focus(2, 0.0, 0.0, 0.0, 0.0), "outside the area: no field");
        assert_eq!(m.events.len(), 3);
        assert_eq!(m.dropped_outside, 0);
    }
}

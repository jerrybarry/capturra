//! Where the keyboard focus is during a recording, as `Focus` events. Platform-independent: the macOS
//! reader (`focus_macos.rs`) is a `FocusSource`, and this turns what it says into events.
//!
//! Privacy: only geometry is kept. The focused element's role is used to decide whether it is a text
//! field and is thrown away; no text, no labels, no titles and no key identity are read.

use crate::clock::Clock;
use anyhow::Result;
use capturra_core::InputEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the focus is read.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
/// A frame that moves or resizes by no more than this (points) is the same field.
const SAME_FIELD_PT: f64 = 2.0;

/// What the source saw this time. Coordinates are global points, like the pointer's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FocusReading {
    /// A text field has the focus.
    Field { x: f64, y: f64, w: f64, h: f64 },
    /// Something else has it (a button, a list, nothing at all).
    NoField,
    /// The focus cannot be read at all (no Accessibility permission): sampling stops.
    Unavailable,
}

pub trait FocusSource {
    fn read(&mut self) -> FocusReading;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FocusStats {
    pub reads: u64,
    pub changes: u64,
}

/// Logs a `Focus` event when the focused field changes, and an empty one when it is lost.
pub struct FocusSampler<S: FocusSource> {
    source: S,
    last: Option<(f64, f64, f64, f64)>,
    pub events: Vec<InputEvent>,
    pub stats: FocusStats,
    /// Set when the source said it cannot read the focus.
    pub unavailable: bool,
}

impl<S: FocusSource> FocusSampler<S> {
    pub fn new(source: S) -> Self {
        Self { source, last: None, events: Vec::new(), stats: FocusStats::default(), unavailable: false }
    }

    /// Read once at clock time `t_ms`. Returns false when sampling should stop.
    pub fn poll(&mut self, t_ms: u64) -> bool {
        self.stats.reads += 1;
        match self.source.read() {
            FocusReading::Unavailable => {
                self.unavailable = true;
                return false;
            }
            FocusReading::NoField => {
                if self.last.take().is_some() {
                    self.events.push(InputEvent::Focus { t_ms, x: 0.0, y: 0.0, w: 0.0, h: 0.0 });
                    self.stats.changes += 1;
                }
            }
            FocusReading::Field { x, y, w, h } => {
                let same = self.last.is_some_and(|(lx, ly, lw, lh)| {
                    (lx - x).abs() <= SAME_FIELD_PT
                        && (ly - y).abs() <= SAME_FIELD_PT
                        && (lw - w).abs() <= SAME_FIELD_PT
                        && (lh - h).abs() <= SAME_FIELD_PT
                });
                if !same {
                    self.last = Some((x, y, w, h));
                    self.events.push(InputEvent::Focus { t_ms, x, y, w, h });
                    self.stats.changes += 1;
                }
            }
        }
        true
    }
}

pub struct FocusOutput {
    pub events: Vec<InputEvent>,
    pub stats: FocusStats,
    pub unavailable: bool,
}

/// Sample on its own thread, stamping from the shared clock, until `stop` is set. The source is built
/// on the thread by `make_source`.
pub fn spawn<S: FocusSource>(
    make_source: impl FnOnce() -> S + Send + 'static,
    clock: Clock,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<FocusOutput>> {
    Ok(std::thread::Builder::new().name("capturra-focus".into()).spawn(move || {
        let mut sampler = FocusSampler::new(make_source());
        while !stop.load(Ordering::SeqCst) {
            if !sampler.poll(clock.now_ms()) {
                break;
            }
            std::thread::sleep(SAMPLE_INTERVAL);
        }
        FocusOutput { events: sampler.events, stats: sampler.stats, unavailable: sampler.unavailable }
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Scripted(VecDeque<FocusReading>);

    impl FocusSource for Scripted {
        fn read(&mut self) -> FocusReading {
            self.0.pop_front().unwrap_or(FocusReading::NoField)
        }
    }

    fn field(x: f64) -> FocusReading {
        FocusReading::Field { x, y: 100.0, w: 300.0, h: 24.0 }
    }

    #[test]
    fn a_change_is_logged_once_and_jitter_is_not() {
        let mut s = FocusSampler::new(Scripted(VecDeque::from([field(10.0), field(10.5), field(11.9), field(10.0), field(200.0)])));
        for t in 0..5 {
            assert!(s.poll(t * 100));
        }
        assert_eq!(s.events.len(), 2, "{:?}", s.events);
        assert_eq!(s.events[0], InputEvent::Focus { t_ms: 0, x: 10.0, y: 100.0, w: 300.0, h: 24.0 });
        assert_eq!(s.events[1], InputEvent::Focus { t_ms: 400, x: 200.0, y: 100.0, w: 300.0, h: 24.0 });
        assert_eq!(s.stats, FocusStats { reads: 5, changes: 2 });
    }

    #[test]
    fn losing_the_field_is_logged_as_an_empty_frame_and_only_once() {
        let mut s = FocusSampler::new(Scripted(VecDeque::from([
            FocusReading::NoField,
            field(10.0),
            FocusReading::NoField,
            FocusReading::NoField,
            field(10.0),
        ])));
        for t in 0..5 {
            s.poll(t * 100);
        }
        // nothing at first (no field to forget), then field, cleared, field again
        let kinds: Vec<bool> = s.events.iter().map(|e| matches!(e, InputEvent::Focus { w, .. } if *w > 0.0)).collect();
        assert_eq!(kinds, vec![true, false, true], "{:?}", s.events);
    }

    #[test]
    fn a_source_that_cannot_read_stops_the_sampling() {
        let mut s = FocusSampler::new(Scripted(VecDeque::from([field(10.0), FocusReading::Unavailable, field(300.0)])));
        assert!(s.poll(0));
        assert!(!s.poll(100));
        assert!(s.unavailable);
        assert_eq!(s.events.len(), 1);
    }

    #[test]
    fn the_thread_samples_until_stopped_and_hands_back_its_events() {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn(|| Scripted(VecDeque::from([field(5.0), FocusReading::NoField])), Clock::start(), stop.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(350));
        stop.store(true, Ordering::SeqCst);
        let out = handle.join().unwrap();
        assert_eq!(out.events.len(), 2);
        assert!(!out.unavailable && out.stats.reads >= 3);
    }
}

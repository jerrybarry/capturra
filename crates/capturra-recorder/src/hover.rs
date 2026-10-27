//! Whether the pointer is over something clickable (a button, link, tab or menu item), as `Hover` events.
//! Platform-independent: the macOS reader (`hover_macos.rs`) is a `HoverSource`, and this turns what it
//! says into events. The renderer shows the hand pointer for those stretches.
//!
//! Privacy: a yes/no only. The element's role decides the answer and is thrown away.

use crate::clock::Clock;
use anyhow::Result;
use capturra_core::InputEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// How often the element under the pointer is read.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(60);
/// A new answer must be given this many reads in a row before it is believed: the edge of a button
/// flickers as the pointer crosses it, and a hand that blinks for one frame looks like a glitch.
const CONFIRM_READS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoverReading {
    Clickable,
    NotClickable,
    /// The element under the pointer cannot be read at all (no Accessibility permission): sampling stops.
    Unavailable,
}

pub trait HoverSource {
    fn read(&mut self) -> HoverReading;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct HoverStats {
    pub reads: u64,
    pub changes: u64,
    /// Reads that said "clickable".
    pub clickable_reads: u64,
}

/// Logs a `Hover` event when the answer changes, stamped with when the new answer first appeared.
pub struct HoverSampler<S: HoverSource> {
    source: S,
    /// What the log currently says.
    state: bool,
    /// A different answer seen since `.1` (clock ms), and how many reads in a row have said so.
    pending: Option<(bool, u64, u32)>,
    pub events: Vec<InputEvent>,
    pub stats: HoverStats,
    pub unavailable: bool,
}

impl<S: HoverSource> HoverSampler<S> {
    pub fn new(source: S) -> Self {
        Self { source, state: false, pending: None, events: Vec::new(), stats: HoverStats::default(), unavailable: false }
    }

    /// Read once at clock time `t_ms`. Returns false when sampling should stop.
    pub fn poll(&mut self, t_ms: u64) -> bool {
        self.stats.reads += 1;
        let now = match self.source.read() {
            HoverReading::Unavailable => {
                self.unavailable = true;
                return false;
            }
            HoverReading::Clickable => {
                self.stats.clickable_reads += 1;
                true
            }
            HoverReading::NotClickable => false,
        };
        if now == self.state {
            self.pending = None;
            return true;
        }
        let (since, count) = match self.pending {
            Some((p, since, n)) if p == now => (since, n + 1),
            _ => (t_ms, 1),
        };
        if count >= CONFIRM_READS {
            self.state = now;
            self.pending = None;
            self.events.push(InputEvent::Hover { t_ms: since, clickable: now });
            self.stats.changes += 1;
        } else {
            self.pending = Some((now, since, count));
        }
        true
    }
}

pub struct HoverOutput {
    pub events: Vec<InputEvent>,
    pub stats: HoverStats,
    pub unavailable: bool,
}

/// Sample on its own thread, stamping from the shared clock, until `stop` is set.
pub fn spawn<S: HoverSource>(
    make_source: impl FnOnce() -> S + Send + 'static,
    clock: Clock,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<HoverOutput>> {
    Ok(std::thread::Builder::new().name("capturra-hover".into()).spawn(move || {
        let mut sampler = HoverSampler::new(make_source());
        while !stop.load(Ordering::SeqCst) {
            if !sampler.poll(clock.now_ms()) {
                break;
            }
            std::thread::sleep(SAMPLE_INTERVAL);
        }
        HoverOutput { events: sampler.events, stats: sampler.stats, unavailable: sampler.unavailable }
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use HoverReading::{Clickable as C, NotClickable as N};

    struct Scripted(VecDeque<HoverReading>);

    impl HoverSource for Scripted {
        fn read(&mut self) -> HoverReading {
            self.0.pop_front().unwrap_or(HoverReading::NotClickable)
        }
    }

    fn run(script: &[HoverReading]) -> HoverSampler<Scripted> {
        let mut s = HoverSampler::new(Scripted(script.iter().copied().collect()));
        for (i, _) in script.iter().enumerate() {
            assert!(s.poll(i as u64 * 100));
        }
        s
    }

    #[test]
    fn a_change_is_believed_after_two_reads_and_stamped_with_when_it_first_appeared() {
        let s = run(&[N, N, C, C, C, N, N, N]);
        assert_eq!(s.events, vec![InputEvent::Hover { t_ms: 200, clickable: true }, InputEvent::Hover { t_ms: 500, clickable: false }]);
        assert_eq!(s.stats, HoverStats { reads: 8, changes: 2, clickable_reads: 3 });
    }

    #[test]
    fn a_one_read_flicker_is_not_a_change_and_the_start_is_not_clickable() {
        let s = run(&[N, C, N, C, N, N, C, N]);
        assert!(s.events.is_empty(), "{:?}", s.events);
        // already over a button when the recording starts
        let first = run(&[C, C, C]);
        assert_eq!(first.events, vec![InputEvent::Hover { t_ms: 0, clickable: true }]);
    }

    #[test]
    fn a_source_that_cannot_read_stops_the_sampling() {
        let mut s = HoverSampler::new(Scripted(VecDeque::from([C, HoverReading::Unavailable, C])));
        assert!(s.poll(0));
        assert!(!s.poll(100));
        assert!(s.unavailable);
    }

    #[test]
    fn the_thread_samples_until_stopped() {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = spawn(|| Scripted(VecDeque::from([C, C, C, C])), Clock::start(), stop.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        stop.store(true, Ordering::SeqCst);
        let out = handle.join().unwrap();
        assert_eq!(out.events.len(), 2, "on while the script says clickable, off after it runs out: {:?}", out.events);
        assert!(!out.unavailable);
    }
}

//! Cursor capture: where the cursor image comes from is a `CursorSource`; everything else here
//! (de-duplication, PNG files, change events) is platform-independent and unit-tested.

use crate::clock::Clock;
use anyhow::{Context, Result};
use capturra_core::{CursorImage, Hotspot, InputEvent, SizePt};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

/// ~60 samples per second.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(16);

/// A new cursor must stay put this long before a change is logged, so a pointer skimming past
/// a link or flicking over a window edge doesn't strobe between two cursors.
pub const DEBOUNCE_MS: u64 = 100;

/// A cursor image as a source reports it.
#[derive(Clone)]
pub struct CursorImageData {
    /// PNG with alpha.
    pub png: Vec<u8>,
    /// Hotspot in points from the image's top-left.
    pub hotspot: (f64, f64),
    /// Pixels per point of the PNG.
    pub scale: f64,
    pub width_px: u32,
    pub height_px: u32,
}

/// Anything that can say what the system cursor looks like right now.
pub trait CursorSource {
    /// `None` when the cursor cannot be read this time (e.g. the API returned nil).
    fn sample(&mut self) -> Option<CursorImageData>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CursorStats {
    pub samples: u64,
    pub nil: u64,
}

/// Turns a stream of samples into a cursors table, PNG files and change events.
pub struct CursorSampler<S: CursorSource> {
    source: S,
    dir: PathBuf,
    by_pixels: HashMap<u64, u32>,
    /// The cursor the log currently says is showing.
    last: Option<u32>,
    debounce_ms: u64,
    /// A different cursor seen since `.1` (shared-clock ms) that has not yet been stable long
    /// enough to log.
    pending: Option<(u32, u64)>,
    pub table: Vec<CursorImage>,
    pub events: Vec<InputEvent>,
    pub stats: CursorStats,
}

impl<S: CursorSource> CursorSampler<S> {
    /// PNGs are written to `<out_dir>/cursors/<id>.png`.
    pub fn new(source: S, out_dir: &Path) -> Result<Self> {
        let dir = out_dir.join("cursors");
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self {
            source,
            dir,
            by_pixels: HashMap::new(),
            last: None,
            debounce_ms: DEBOUNCE_MS,
            pending: None,
            table: vec![],
            events: vec![],
            stats: CursorStats::default(),
        })
    }

    /// Override the debounce window; 0 logs every change immediately.
    pub fn with_debounce_ms(mut self, ms: u64) -> Self {
        self.debounce_ms = ms;
        self
    }

    /// Swap the source, keeping everything collected so far (de-duplication state included).
    pub fn with_source<T: CursorSource>(self, source: T) -> CursorSampler<T> {
        CursorSampler {
            source,
            dir: self.dir,
            by_pixels: self.by_pixels,
            last: self.last,
            debounce_ms: self.debounce_ms,
            pending: self.pending,
            table: self.table,
            events: self.events,
            stats: self.stats,
        }
    }

    /// Take one sample at `t_ms` on the shared clock. A nil sample keeps the previous cursor.
    ///
    /// A change event is logged only when the image differs from the one before AND the new
    /// image has been showing for `debounce_ms`. The event is stamped with when the new image
    /// first appeared, not when it was confirmed, so debouncing adds no lag to the render. The
    /// very first cursor is logged at once. A change still unconfirmed when sampling stops is
    /// dropped (it lasted under the debounce window).
    pub fn poll(&mut self, t_ms: u64) -> Result<()> {
        self.stats.samples += 1;
        let Some(img) = self.source.sample() else {
            self.stats.nil += 1;
            return Ok(());
        };
        // Same pixels and same hotspot = same cursor. Same pixels with a different hotspot
        // would draw in a different place, so that is a distinct cursor.
        let mut h = DefaultHasher::new();
        img.png.hash(&mut h);
        img.hotspot.0.to_bits().hash(&mut h);
        img.hotspot.1.to_bits().hash(&mut h);
        img.scale.to_bits().hash(&mut h);
        let key = h.finish();

        let id = match self.by_pixels.get(&key) {
            Some(&id) => id,
            None => {
                let id = self.table.len() as u32;
                let file = format!("cursors/{id}.png");
                let path = self.dir.join(format!("{id}.png"));
                std::fs::write(&path, &img.png).with_context(|| format!("writing {}", path.display()))?;
                let scale = if img.scale > 0.0 { img.scale } else { 1.0 };
                self.table.push(CursorImage {
                    id,
                    file,
                    hotspot: Hotspot { x: img.hotspot.0, y: img.hotspot.1 },
                    scale,
                    size: SizePt { w: img.width_px as f64 / scale, h: img.height_px as f64 / scale },
                    role: None,
                });
                self.by_pixels.insert(key, id);
                id
            }
        };

        // A pending cursor that has already lasted long enough was real, even if it has since
        // been replaced: log it before dealing with the new sample.
        if let Some((pid, since)) = self.pending {
            if id != pid && t_ms.saturating_sub(since) >= self.debounce_ms {
                self.commit(pid, since);
            }
        }
        if self.last == Some(id) {
            self.pending = None; // flipped back before the window ended: no change at all
        } else if self.last.is_none() || self.debounce_ms == 0 {
            self.commit(id, t_ms);
        } else {
            match self.pending {
                Some((pid, since)) if pid == id => {
                    if t_ms.saturating_sub(since) >= self.debounce_ms {
                        self.commit(id, since);
                    }
                }
                _ => self.pending = Some((id, t_ms)),
            }
        }
        Ok(())
    }

    fn commit(&mut self, id: u32, t_ms: u64) {
        self.events.push(InputEvent::CursorChange { t_ms, id });
        self.last = Some(id);
        self.pending = None;
    }
}

pub struct CursorOutput {
    pub table: Vec<CursorImage>,
    pub events: Vec<InputEvent>,
    pub stats: CursorStats,
}

/// Sample on its own thread, stamping from the shared clock, until `stop` is set.
/// The source is built on the thread (it may not be `Send`) by `make_source`.
pub fn spawn<S: CursorSource>(
    make_source: impl FnOnce() -> S + Send + 'static,
    out_dir: &Path,
    clock: Clock,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<Result<CursorOutput>>> {
    let out_dir = out_dir.to_path_buf();
    Ok(std::thread::Builder::new().name("capturra-cursor".into()).spawn(move || {
        let mut sampler = CursorSampler::new(make_source(), &out_dir)?;
        while !stop.load(Ordering::SeqCst) {
            sampler.poll(clock.now_ms())?;
            std::thread::sleep(SAMPLE_INTERVAL);
        }
        Ok(CursorOutput { table: sampler.table, events: sampler.events, stats: sampler.stats })
    })?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Plays back a script: `Some(tag)` is a cursor whose PNG bytes are `[tag]`.
    struct Scripted(VecDeque<Option<u8>>);

    impl CursorSource for Scripted {
        fn sample(&mut self) -> Option<CursorImageData> {
            let tag = self.0.pop_front().flatten()?;
            Some(CursorImageData { png: vec![tag], hotspot: (tag as f64, 1.0), scale: 2.0, width_px: 40, height_px: 60 })
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("capturra_cursor_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn run(script: &[Option<u8>], name: &str) -> (CursorSampler<Scripted>, PathBuf) {
        let dir = tmp(name);
        let mut s = CursorSampler::new(Scripted(script.iter().copied().collect()), &dir).unwrap().with_debounce_ms(0);
        for (i, _) in script.iter().enumerate() {
            s.poll(i as u64 * 16).unwrap();
        }
        (s, dir)
    }

    #[test]
    fn logs_a_change_only_when_the_image_changes() {
        // arrow x3, text x2, arrow x1
        let (s, dir) = run(&[Some(1), Some(1), Some(1), Some(2), Some(2), Some(1)], "changes");
        assert_eq!(
            s.events,
            vec![
                InputEvent::CursorChange { t_ms: 0, id: 0 },
                InputEvent::CursorChange { t_ms: 48, id: 1 },
                InputEvent::CursorChange { t_ms: 80, id: 0 },
            ]
        );
        // Going back to the arrow reuses its id and file: two images, not three.
        assert_eq!(s.table.len(), 2);
        assert!(dir.join("cursors/0.png").exists() && dir.join("cursors/1.png").exists());
        assert!(!dir.join("cursors/2.png").exists());
    }

    #[test]
    fn nil_sample_keeps_the_last_cursor() {
        let (s, _) = run(&[Some(1), None, None, Some(1), None, Some(2)], "nil");
        assert_eq!(s.stats, CursorStats { samples: 6, nil: 3 });
        // A nil between two identical cursors is not a change.
        assert_eq!(s.events, vec![InputEvent::CursorChange { t_ms: 0, id: 0 }, InputEvent::CursorChange { t_ms: 80, id: 1 }]);
    }

    #[test]
    fn leading_nils_delay_the_first_event() {
        let (s, _) = run(&[None, None, Some(5)], "leading_nil");
        assert_eq!(s.events, vec![InputEvent::CursorChange { t_ms: 32, id: 0 }]);
    }

    #[test]
    fn all_nil_produces_nothing() {
        let (s, _) = run(&[None, None, None], "all_nil");
        assert!(s.events.is_empty() && s.table.is_empty());
    }

    #[test]
    fn table_entry_has_hotspot_scale_and_point_size() {
        let (s, dir) = run(&[Some(3)], "table");
        let c = &s.table[0];
        assert_eq!((c.id, c.file.as_str()), (0, "cursors/0.png"));
        assert_eq!((c.hotspot.x, c.hotspot.y), (3.0, 1.0));
        assert_eq!(c.scale, 2.0);
        // 40x60 px at 2 px/pt is 20x30 pt.
        assert_eq!((c.size.w, c.size.h), (20.0, 30.0));
        assert_eq!(std::fs::read(dir.join(&c.file)).unwrap(), vec![3]);
    }

    #[test]
    fn same_pixels_with_a_different_hotspot_is_a_different_cursor() {
        struct TwoHotspots(u8);
        impl CursorSource for TwoHotspots {
            fn sample(&mut self) -> Option<CursorImageData> {
                self.0 += 1;
                Some(CursorImageData { png: vec![9], hotspot: (self.0 as f64, 0.0), scale: 1.0, width_px: 8, height_px: 8 })
            }
        }
        let dir = tmp("hotspot");
        let mut s = CursorSampler::new(TwoHotspots(0), &dir).unwrap();
        s.poll(0).unwrap();
        s.poll(16).unwrap();
        assert_eq!(s.table.len(), 2);
    }

    /// Feed `(t_ms, tag)` samples through a sampler with the default 100 ms debounce.
    fn debounced(samples: &[(u64, Option<u8>)], name: &str) -> CursorSampler<Scripted> {
        let mut s = CursorSampler::new(Scripted(samples.iter().map(|&(_, c)| c).collect()), &tmp(name)).unwrap();
        for &(t, _) in samples {
            s.poll(t).unwrap();
        }
        s
    }

    fn change(t_ms: u64, id: u32) -> InputEvent {
        InputEvent::CursorChange { t_ms, id }
    }

    #[test]
    fn first_cursor_is_logged_immediately_even_with_debounce() {
        let s = debounced(&[(40, Some(1))], "deb_first");
        assert_eq!(s.events, vec![change(40, 0)]);
    }

    #[test]
    fn quick_flips_between_two_cursors_log_nothing() {
        // arrow, then text/arrow every 16 ms for ~100 ms: nothing is stable long enough.
        let mut v = vec![(0, Some(1))];
        for i in 1..=6u64 {
            v.push((i * 16, Some(if i % 2 == 1 { 2 } else { 1 })));
        }
        v.push((112, Some(1)));
        let s = debounced(&v, "deb_flicker");
        assert_eq!(s.events, vec![change(0, 0)]);
    }

    #[test]
    fn a_cursor_that_stays_is_logged_with_its_first_seen_time() {
        // text appears at 160 and is still there at 272 (112 ms later): logged at 160, not 272.
        let s = debounced(&[(0, Some(1)), (144, Some(1)), (160, Some(2)), (200, Some(2)), (272, Some(2))], "deb_stay");
        assert_eq!(s.events, vec![change(0, 0), change(160, 1)]);
    }

    #[test]
    fn exactly_the_debounce_window_counts_as_stable() {
        let s = debounced(&[(0, Some(1)), (50, Some(2)), (149, Some(2))], "deb_edge_short");
        assert_eq!(s.events, vec![change(0, 0)], "99 ms is not enough");
        let s = debounced(&[(0, Some(1)), (50, Some(2)), (150, Some(2))], "deb_edge_exact");
        assert_eq!(s.events, vec![change(0, 0), change(50, 1)], "100 ms is");
    }

    #[test]
    fn a_stable_cursor_replaced_by_another_is_still_logged() {
        // text was up for 120 ms, then the link cursor appears. Text must not be lost.
        let s = debounced(&[(0, Some(1)), (20, Some(2)), (140, Some(3)), (260, Some(3))], "deb_replaced");
        assert_eq!(s.events, vec![change(0, 0), change(20, 1), change(140, 2)]);
    }

    #[test]
    fn nil_samples_do_not_reset_a_pending_change() {
        let s = debounced(&[(0, Some(1)), (20, Some(2)), (60, None), (100, None), (130, Some(2))], "deb_nil");
        assert_eq!(s.events, vec![change(0, 0), change(20, 1)]);
    }

    #[test]
    fn thread_samples_until_stopped_and_returns_events() {
        let dir = tmp("thread");
        let stop = Arc::new(AtomicBool::new(false));
        // Endless alternation is not needed; one steady cursor is enough to prove the loop.
        let handle = spawn(|| Scripted(std::iter::repeat_n(Some(1), 10_000).collect()), &dir, Clock::start(), stop.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(120));
        stop.store(true, Ordering::SeqCst);
        let out = handle.join().unwrap().unwrap();
        assert!(out.stats.samples >= 3, "only {} samples in 120ms", out.stats.samples);
        assert_eq!(out.events.len(), 1);
    }
}

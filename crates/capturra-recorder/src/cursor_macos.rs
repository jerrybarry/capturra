//! The macOS cursor source: `NSCursor.currentSystemCursor`. This is the only file that touches
//! it, so replacing the source later (private CGS calls, a ScreenCaptureKit-based one) means
//! writing another `CursorSource` and changing one line in `capture.rs`.
//!
//! The API is deprecated and Apple says it will be nil in a future macOS; the sampler keeps the
//! last cursor on nil, and `capture.rs` warns loudly if no image was ever obtained.

use crate::clock::Clock;
use crate::cursor::{CursorImageData, CursorSource};
use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSBitmapImageFileType, NSBitmapImageRep, NSCursor, NSImageRep};
use objc2_foundation::{MainThreadMarker, NSDictionary};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Duration;

pub enum Sample {
    /// `currentSystemCursor` returned nil.
    Nil,
    /// A cursor was returned but no bitmap could be extracted from it.
    NoBitmap,
    Cursor(CursorImageData),
}

/// Samples the cursor of whatever app is under the pointer.
pub struct NsCursorSource;

impl CursorSource for NsCursorSource {
    fn sample(&mut self) -> Option<CursorImageData> {
        match sample_system_cursor() {
            Sample::Cursor(c) => Some(c),
            Sample::Nil | Sample::NoBitmap => None,
        }
    }
}

#[allow(deprecated)] // currentSystemCursor is deprecated; whether it still works is the question.
pub fn sample_system_cursor() -> Sample {
    match NSCursor::currentSystemCursor() {
        Some(cursor) => extract(&cursor),
        None => Sample::Nil,
    }
}

/// The app-level standard cursors, extracted exactly like a sampled one (same PNG, hotspot and
/// scale logic). For demos that need real sprites without moving the mouse.
///
/// AppKit hands these out only on the main thread of a process with an `NSApplication` (off it,
/// `+[NSCursor arrowCursor]` returns NULL and objc2 panics), so anywhere else this is empty.
#[allow(deprecated)]
pub fn standard_cursors() -> Vec<(&'static str, CursorImageData)> {
    let Some(mtm) = MainThreadMarker::new() else { return Vec::new() };
    let _app = NSApplication::sharedApplication(mtm);
    let all: [(&'static str, Retained<NSCursor>); 5] = [
        ("arrow", NSCursor::arrowCursor()),
        ("text", NSCursor::IBeamCursor()),
        ("link", NSCursor::pointingHandCursor()),
        ("resize", NSCursor::resizeLeftRightCursor()),
        ("hand", NSCursor::openHandCursor()),
    ];
    all.into_iter()
        .filter_map(|(name, c)| match extract(&c) {
            Sample::Cursor(img) => Some((name, img)),
            _ => None,
        })
        .collect()
}

fn extract(cursor: &NSCursor) -> Sample {
    let image = cursor.image();
    let hot = cursor.hotSpot();
    let size = image.size();

    // The image holds several representations (1x, 2x). Take the largest bitmap one.
    let mut best: Option<Retained<NSBitmapImageRep>> = None;
    for rep in image.representations().iter() {
        let rep: Retained<NSImageRep> = rep;
        if let Ok(bmp) = rep.downcast::<NSBitmapImageRep>() {
            if best.as_ref().is_none_or(|b| bmp.pixelsWide() > b.pixelsWide()) {
                best = Some(bmp);
            }
        }
    }
    let Some(bmp) = best else { return Sample::NoBitmap };

    let props = NSDictionary::new();
    let png = unsafe { bmp.representationUsingType_properties(NSBitmapImageFileType::PNG, &props) };
    let Some(png) = png else { return Sample::NoBitmap };
    let (w, h) = (bmp.pixelsWide().max(0) as u32, bmp.pixelsHigh().max(0) as u32);
    Sample::Cursor(CursorImageData {
        png: png.to_vec(),
        hotspot: (hot.x, hot.y),
        scale: if size.width > 0.0 { w as f64 / size.width } else { 1.0 },
        width_px: w,
        height_px: h,
    })
}

/// Sample the system cursor for `seconds` at ~60 Hz, dump each distinct cursor image to
/// `<out>/<n>.png`, and print every change. For verification only; writes no events.
pub fn probe(out: &Path, seconds: u64, init_app: bool) -> Result<()> {
    std::fs::create_dir_all(out)?;
    if init_app {
        let mtm = MainThreadMarker::new().context("--init-app must run on the main thread")?;
        let _app = NSApplication::sharedApplication(mtm);
        eprintln!("NSApplication initialised");
    }
    let clock = Clock::start();
    let mut ids: HashMap<u64, usize> = HashMap::new();
    let (mut samples, mut nil, mut nobmp) = (0u64, 0u64, 0u64);
    let mut last_id: Option<usize> = None;
    while clock.now_ms() < seconds * 1000 {
        samples += 1;
        match sample_system_cursor() {
            Sample::Nil => nil += 1,
            Sample::NoBitmap => nobmp += 1,
            Sample::Cursor(c) => {
                let mut h = DefaultHasher::new();
                c.png.hash(&mut h);
                let key = h.finish();
                let next = ids.len();
                let id = *ids.entry(key).or_insert_with(|| {
                    let path = out.join(format!("{next}.png"));
                    std::fs::write(&path, &c.png).expect("write cursor png");
                    eprintln!(
                        "  new image {next}: {}x{} px, scale {:.2}, hotspot ({:.1}, {:.1}) pt -> {}",
                        c.width_px,
                        c.height_px,
                        c.scale,
                        c.hotspot.0,
                        c.hotspot.1,
                        path.display()
                    );
                    next
                });
                if last_id != Some(id) {
                    println!("t={:>6} ms  cursor -> {id}", clock.now_ms());
                    last_id = Some(id);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    println!("{samples} samples: {} distinct cursor image(s), {nil} nil, {nobmp} without a bitmap", ids.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::CursorSampler;

    /// The recorder samples from a background thread (cargo runs tests on one too). The probe
    /// only proved the main thread works.
    #[test]
    fn current_system_cursor_works_off_the_main_thread() {
        let dir = std::env::temp_dir().join(format!("capturra_ns_cursor_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = CursorSampler::new(NsCursorSource, &dir).unwrap();
        for i in 0..5 {
            s.poll(i * 16).unwrap();
        }
        assert_eq!(s.stats.nil, 0, "currentSystemCursor returned nil off the main thread");
        let c = &s.table[0];
        // While something hides the pointer (a video playing, typing) the system cursor is a 1 x 1 clear image:
        // reading it worked, there is just nothing visible to measure.
        if c.size.w <= 1.0 && c.size.h <= 1.0 {
            eprintln!("the pointer is hidden right now (a 1x1 cursor); skipping the size check");
            return;
        }
        assert!(c.size.w > 4.0 && c.size.h > 4.0 && c.scale >= 1.0, "{c:?}");
        let png = std::fs::read(dir.join(&c.file)).unwrap();
        assert_eq!(&png[1..4], b"PNG");
    }

    #[test]
    fn standard_cursors_are_only_offered_on_the_main_thread() {
        // cargo runs tests off the main thread; asking there must be empty, not a panic.
        assert!(standard_cursors().is_empty());
    }
}

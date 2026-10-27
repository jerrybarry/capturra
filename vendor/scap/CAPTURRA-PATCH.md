# Why this copy of `scap` exists

This is `scap` 0.0.8 (MIT, https://github.com/helmerapp/scap) with ONE change, in
`src/targets/mac/mod.rs` (marked "Capturra patch"), plus a one-line `#![allow(warnings)]` in `src/lib.rs` to silence upstream warnings. Everything else is upstream.

## The problem
`get_target_dimensions` and `get_scale_factor` sized a **window** target by asking
`NSApp.windowWithWindowNumber:`. That only finds windows owned by the calling process, so for any
other app's window it returned nil and the size and scale came back as 0. Capturra got a
0 x 0 frame size for every `--window`. (`scap` 0.1.0-beta.1 has the same code, so upgrading
does not help.)

## The patch
For a window target, the frame comes from ScreenCaptureKit (`UnsafeSCShareableContent`, the same
source the capture uses) and the scale from the display that holds most of the window.
The `Target::Display` arms are unchanged, so display and region capture behave exactly as before.

## Plan
This is a stopgap. When Capturra adds audio and real ScreenCaptureKit presentation timestamps,
`scap` will not be enough (it hides both), and we will most likely replace it with
`screencapturekit` used directly. Delete this directory and the `[patch.crates-io]` entry in the
root `Cargo.toml` then.

## Second small change: exporting `Window`
`src/lib.rs` also does `pub use targets::Window;`. Upstream keeps the type private, so a window could only be
named by finding it in `get_all_targets()`, which does not list untitled windows such as the menu-bar item.
Capturra builds `Target::Window` by id to exclude its own windows from display and region captures
(`Options::excluded_targets`, which upstream already supports for display captures).

## Third change: exclude this whole app (`Options::exclude_current_app`)
Excluding windows by id only covers windows that exist when the capture is built; Capturra's windows come
and go. With `exclude_current_app: true` a display capture excludes this process's application, so all of
its windows (the Studio with the recording bar and countdown, the popover, the menu-bar item with the
timer) stay out of the video, also ones created mid-recording. If the app cannot be found in the shareable
content, the engine falls back to `excluded_targets`.

Gotcha: in `screencapturekit-sys` 0.2.8 the names of the two application variants are swapped. The variant
called `DisplayIncludingApplicationsExceptingWindows` sends `excludingApplications:exceptingWindows:`, so
THAT is the one that excludes. Not verified against a live recording: do one and check the video.

## Fourth change: system audio (`Options::captures_audio`)
With `captures_audio: true` (macOS 13+) the stream also delivers audio: 48 kHz, 2 channels, Capturra's own sound
excluded. `get_next_frame()` then returns `Frame::Audio(AudioFrame { display_time, sample_rate, channels })` between the
video frames, with one planar `Vec<f32>` per channel and the presentation time (ns, host clock) of the first sample.
`screencapturekit-sys` 0.2.8's `get_av_audio_buffer_list` panics on a CoreMedia error and mislabels `number_channels`,
so the patch calls it inside `catch_unwind` and treats each buffer as one channel. Not run against a live stream yet.

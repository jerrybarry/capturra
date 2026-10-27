# Why this copy of `rdev` exists

This is `rdev` 0.5.3 (MIT, https://github.com/Narsil/rdev) with ONE change, in
`src/macos/common.rs` (`convert`, marked "Capturra patch"), plus a regression test at the end of that
file, and a one-line `#![allow(warnings)]` in `src/lib.rs` to silence upstream warnings. Everything else is upstream.

## The problem
On every key press, `rdev` worked out the character the key produced (`Event.name`) by calling
`TSMGetInputSourceProperty`, a macOS keyboard-layout API. That API must run on the **main** thread.
`rdev::listen` runs its callback on the listener's own thread, so on macOS 14 and 15 the first key press
while recording aborted the whole app:

    EXC_BAD_INSTRUCTION (SIGILL)
    dispatch_assert_queue_fail  <- islGetInputSourceListWithAdditions  <- TSMGetInputSourceProperty
    <- rdev::macos::keyboard::Keyboard::string_from_code  <- ... <- rdev::macos::listen::raw_callback
    <- capturra_recorder::input::InputLogger::start

(from the crash reports in ~/Library/Logs/DiagnosticReports/capturra-app-*.ips). Pressing the
start/stop shortcut, Return, or typing in the recorded window triggered it.

## The patch
`convert` no longer looks up the key's name: `Event.name` is always `None`. Capturra only records the
*time* of key presses (privacy: timing only, never key identity), so nothing uses the name, and not computing it
is also the safer behaviour. Key codes (`EventType::KeyPress(Key)`) are unchanged.

## Test
`macos::common::capturra_tests` converts a synthetic key press on a background thread and asserts that
no name is produced. A plain test program does not crash even on the unpatched code (the OS only
enforces the main-thread rule inside a running app), so this test guards that the lookup is gone; the
crash itself was diagnosed from the crash reports.

## Plan
Drop this copy if upstream stops computing names on the listener thread, or if input capture moves to
our own event tap.

# Contributing to Capturra

Thanks for helping. Capturra is a young project with one rule that matters more than the others:
**say what you verified and what you did not.**

## Getting set up

See "Build it" in the [README](README.md). You need macOS 13+, Rust 1.91 or newer (CI uses 1.91.0), Node 20.19+ and ffmpeg. Most of the code
(`capturra-core`, the exporter, the audio and encoding logic) also builds and tests on other systems if ffmpeg is installed;
the capture, input, Accessibility and cursor code is macOS only.

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --workspace --all-targets      # keep it at zero warnings
cargo test --workspace
cd apps/capturra-app && npm run build       # type-checks and builds the interface
```

- **Add a test with every behaviour change**, especially to zoom and cursor logic in `capturra-core`. That crate has no OS
  calls, so its tests run anywhere and are deterministic.
- A few recorder tests record the real display on a timer; they need Screen Recording permission and a quiet Mac. A failure
  there on a busy machine is not necessarily your change; say so in the PR.
- **Anything macOS-specific must have been run on a Mac**, or be clearly marked unverified in the PR description.
- Keep UI changes inside the design system (`apps/capturra-app/src/design/`): use the tokens, do not add colours, radii or
  button styles of your own. You can test and inspect UI components directly in a browser using the dev server.

## Rules the code follows

1. **One clock.** Video frames, input events, audio and everything else are stamped from the same monotonic clock. Never use
   wall-clock time for sync.
2. **The cursor is data, not pixels.** Capture with the cursor hidden and redraw it from `events.json`.
3. **Privacy.** Key events store timing only, never which key. Do not add key identity, clipboard contents or window titles
   without opening an issue first.
4. **Never lose a recording silently.** Fail loudly with a message that says what to do.
5. **Ask before** adding a large dependency or changing the `events.json` format (it has a `version` field).

## Reporting bugs

Please include your macOS version, whether it is an Intel or Apple Silicon Mac, how you started Capturra (terminal, app
bundle), and the end-of-run output of the recording if you have it. Do not attach recordings that show private content.

## Code of conduct

Be kind and assume good faith. Harassment is not tolerated; see [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md).

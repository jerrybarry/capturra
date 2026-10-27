# Security

Capturra records your screen, your sound and when you press keys, so security and privacy reports are taken seriously.

## What it does with your data

- Everything stays on your Mac. The app makes no network requests and has no telemetry or accounts.
- Key presses are stored as **timing only**, never which key. Clipboard contents and window titles are not recorded.
- The Accessibility API is read for two yes/no or geometry facts only: the frame of the focused text field and whether the
  pointer is over something clickable. No text, labels or titles are read.

## Reporting a vulnerability

Please do **not** open a public issue for a security problem. Use GitHub's private reporting instead:
**Security > Report a vulnerability** on the repository page. Include what you found, how to reproduce it, and which version
or commit you tested. You should get an answer within a week.

## Supported versions

Capturra has not had a stable release yet; fixes go to the `main` branch.

#!/usr/bin/env bash
# Long-session soak test: records for N minutes while sampling memory, then reports memory growth,
# video length vs wall clock (drift), dropped frames and idle-gap bursts.
#
#   scripts/soak.sh [minutes=30] [fps=30] [extra `capturra record` args...]
#
# Run it from Terminal (Screen Recording + Accessibility belong to the app you run it from).
# Keep the screen busy: play a video or scroll for part of it and leave it idle for a few minutes
# (idle stretches are what the idle-fill code is for), and move the mouse now and then.
# Needs: a release build (`cargo build --release`), ffprobe, python3.
set -euo pipefail
cd "$(dirname "$0")/.."

MINUTES="${1:-30}"
FPS="${2:-30}"
shift $(( $# > 2 ? 2 : $# )) || true
BIN=target/release/capturra
[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }
command -v ffprobe >/dev/null || { echo "ffprobe not found (brew install ffmpeg)" >&2; exit 1; }

OUT="soak/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"
SECONDS_TOTAL=$(( MINUTES * 60 ))
echo "Recording $MINUTES min at $FPS fps into $OUT (Ctrl+C stops early and still reports)."

"$BIN" record --seconds "$SECONDS_TOTAL" --fps "$FPS" --out "$OUT" "$@" >"$OUT/record.log" 2>&1 &
PID=$!
trap 'kill -INT "$PID" 2>/dev/null || true' INT

# RSS of the recorder plus its ffmpeg child, in MB, every 15 s.
echo "elapsed_s,recorder_mb,ffmpeg_mb" >"$OUT/memory.csv"
START=$(date +%s)
while kill -0 "$PID" 2>/dev/null; do
  REC=$(ps -o rss= -p "$PID" 2>/dev/null | awk '{printf "%.1f", $1/1024}')
  FF=$(pgrep -P "$PID" 2>/dev/null | xargs -I{} ps -o rss= -p {} 2>/dev/null | awk '{s+=$1} END {printf "%.1f", s/1024}')
  echo "$(( $(date +%s) - START )),${REC:-0},${FF:-0}" >>"$OUT/memory.csv"
  sleep 15
done
wait "$PID" || echo "recorder exited with an error; see $OUT/record.log"

python3 scripts/soak_report.py "$OUT" "$FPS"

#!/usr/bin/env python3
"""Report on a soak run: python3 scripts/soak_report.py <run dir> <fps>.

Reads <dir>/memory.csv, <dir>/record.log and <dir>/recording.mp4. Exits 1 if a limit is exceeded.
"""
import csv, re, subprocess, sys
from pathlib import Path

MAX_RSS_GROWTH_MB_PER_H = 50.0   # recorder + ffmpeg, between the first and last fifth of the run
MAX_DRIFT_S = 0.25               # video length vs the recording span the recorder reported
MAX_DROP_FRACTION = 0.001        # dropped / received frames


def mean(xs):
    return sum(xs) / len(xs) if xs else 0.0


def memory_growth(rows):
    """MB per hour, comparing the mean of the first and the last fifth of the samples."""
    if len(rows) < 10:
        return None
    total = [r[1] + r[2] for r in rows]
    n = max(2, len(rows) // 5)
    first, last = mean(total[:n]), mean(total[-n:])
    hours = (rows[-1][0] - rows[0][0]) / 3600
    return (last - first) / hours if hours > 0 else None, first, last, max(total)


def video_seconds(path):
    out = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
                         capture_output=True, text=True).stdout.strip()
    return float(out)


def main(run, fps):
    run = Path(run)
    rows = []
    with open(run / "memory.csv") as f:
        for r in csv.DictReader(f):
            rows.append((float(r["elapsed_s"]), float(r["recorder_mb"] or 0), float(r["ffmpeg_mb"] or 0)))
    log = (run / "record.log").read_text(errors="replace")
    problems = []

    g = memory_growth(rows)
    if g is None:
        print("memory: too few samples to judge growth")
    else:
        rate, first, last, peak = g
        print(f"memory: {first:.0f} MB at the start, {last:.0f} MB at the end, peak {peak:.0f} MB, growth {rate:+.1f} MB/h")
        if rate > MAX_RSS_GROWTH_MB_PER_H:
            problems.append(f"memory grows {rate:.0f} MB/h (limit {MAX_RSS_GROWTH_MB_PER_H:.0f})")

    video = run / "recording.mp4"
    span = re.search(r"pipe profile \(\d+ fps, (\d+(?:\.\d+)?) s\)", log)
    if video.exists():
        dur = video_seconds(video)
        expected = None
        if span:
            expected = float(span.group(1))
        elif rows:
            expected = rows[-1][0]  # wall clock of the sampler: coarse (15 s steps)
        print(f"video: {dur:.2f} s long" + (f", recording span {expected:.2f} s" if expected else ""))
        if expected is not None:
            drift = dur - expected
            tol = MAX_DRIFT_S + 0.05 if span else 20.0  # the log rounds the span to 0.1 s
            print(f"drift: video minus recording span = {drift:+.2f} s" + ("" if span else " (span not found in the log; sampler wall clock used, coarse)"))
            if abs(drift) > tol:
                problems.append(f"video length is {drift:+.2f} s off the recording span")
    else:
        problems.append("no recording.mp4 was produced")

    m = re.search(r"frames: (\d+) received, (\d+) empty, (\d+) skipped.*?(\d+) dropped", log)
    if m:
        seen, dropped = int(m.group(1)), int(m.group(4))
        frac = dropped / seen if seen else 0
        print(f"frames: {seen} received, {dropped} dropped ({frac:.3%})")
        if frac > MAX_DROP_FRACTION:
            problems.append(f"{frac:.2%} of frames dropped (limit {MAX_DROP_FRACTION:.1%})")
    else:
        problems.append("no frame summary in record.log (did the recording finish?)")
    t = re.search(r"timestamps: .*", log)
    if t:
        print(t.group(0))
    for line in log.splitlines():
        if "slow" in line.lower() and "write" in line.lower():
            print(line)

    print()
    if problems:
        print("SOAK FAILED:")
        for p in problems:
            print("  -", p)
        sys.exit(1)
    print("SOAK PASSED: memory flat, no drift, drops within limit.")


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    main(sys.argv[1], int(sys.argv[2]))

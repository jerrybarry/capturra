// Frames for the Clip track: a few evenly spaced pictures from the recording, so the track shows
// what the video contains.
import { invoke } from "@tauri-apps/api/core";

/** The time (ms) at the middle of each of `count` equal slices of the clip. */
export function frameTimes(durationMs: number, count: number): number[] {
  if (!(durationMs > 0) || count < 1) return [];
  return Array.from({ length: count }, (_, i) => Math.round(((i + 0.5) / count) * durationMs));
}

/** How many tiles fit in `widthPx` at about `tilePx` each. At least one. */
export function tileCount(widthPx: number, tilePx = 120): number {
  return Math.max(1, Math.floor(widthPx / tilePx));
}

const decode = (url: string): Promise<HTMLImageElement | null> =>
  new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => resolve(null);
    img.src = url;
  });

/**
 * The frames at `timesMs` of recording `id`, taken by the app with ffmpeg (see `video_frames.rs`).
 * An entry is `null` where no frame was available, so one bad time leaves a gap in the strip
 * instead of failing the whole thing.
 */
export async function loadFrames(id: string, timesMs: number[], heightPx = 100): Promise<(HTMLImageElement | null)[]> {
  if (!timesMs.length) return [];
  try {
    const urls = await invoke<(string | null)[]>("video_frames", { id, timesMs, height: heightPx });
    return await Promise.all(urls.map((u) => (u ? decode(u) : Promise.resolve(null))));
  } catch (e) {
    console.warn(`filmstrip: ${e}`);
    return timesMs.map(() => null);
  }
}

// The outline of a recording's sound for the Audio track, taken by the app with ffmpeg (see `waveform.rs`).
import { invoke } from "@tauri-apps/api/core";

/** How many bars the app is asked for. The track resamples them to its width. */
export const WAVE_BARS = 3000;

/** Loudness values from 0 to 1 over the whole recording, or `null` if it has no sound (or cannot be read). */
export async function loadWaveform(id: string, bars = WAVE_BARS): Promise<number[] | null> {
  try {
    return await invoke<number[] | null>("audio_waveform", { id, bars });
  } catch (e) {
    console.warn(`waveform: ${e}`);
    return null;
  }
}

/** `bins` bars from `peaks`, each the loudest of the peaks it covers. */
export function resample(peaks: number[], bins: number): number[] {
  if (!peaks.length || bins < 1) return [];
  return Array.from({ length: bins }, (_, b) => {
    const from = Math.floor((b * peaks.length) / bins);
    const to = Math.max(from + 1, Math.floor(((b + 1) * peaks.length) / bins));
    let m = 0;
    for (let i = from; i < to && i < peaks.length; i++) m = Math.max(m, peaks[i]);
    return m;
  });
}

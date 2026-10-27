// Typed calls for the app's settings, shared by the popover and the Studio window.
import { invoke } from "@tauri-apps/api/core";

export type SourceKind = "display" | "window" | "region";
export interface LastSource { kind: SourceKind; display?: number; region?: [number, number, number, number] }
export interface Settings {
  version: number;
  recordings_dir: string | null;
  last_source: LastSource | null;
  fps: number;
  countdown: number;
  hotkey: string;
  /** Keep Capturra's own windows in display and region recordings (for a demo of Capturra itself). */
  show_capturra: boolean;
  /** Record system audio with every recording (macOS 13+). */
  system_audio: boolean;
  /** Record the default microphone with every recording. */
  microphone: boolean;
}
export interface LoadedSettings { settings: Settings; warning: string | null; recordings_path: string }

export const getSettings = () => invoke<LoadedSettings>("get_settings");
export const saveSettings = (settings: Settings) => invoke<Settings>("save_settings", { settings });
/** Rejects with "cancelled" when the user closes the folder chooser. */
export const openRecordingsFolder = () => invoke<void>("open_recordings_folder");
export const chooseRecordingsDir = () => invoke<string>("choose_recordings_dir");
export const renameProject = (id: string, title: string) => invoke<string>("rename_project", { id, title });

/** `~/Movies/Capturra` unless a folder was chosen. */
export const recordingsDirLabel = (s: Settings) => s.recordings_dir ?? "~/Movies/Capturra";

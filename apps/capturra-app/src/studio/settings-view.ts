// The Settings page: where recordings go, the start/stop shortcut, frame rate and countdown,
// permissions, appearance and a short About. The same settings the menu-bar popover edits.
import { getVersion } from "@tauri-apps/api/app";
import { chooseRecordingsDir, getSettings, openRecordingsFolder, saveSettings, type Settings } from "../settings-api";
import { pretty, toShortcut } from "../shortcut";
import { C } from "./capturra";

interface Permissions { screen_recording: boolean; accessibility: boolean; input_monitoring: boolean; responsible_app: string }

const FPS = [30, 60];
const COUNTDOWNS = [0, 3, 5, 10];
const THEMES: [string, string][] = [["system", "System"], ["light", "Light"], ["dark", "Dark"]];

let root: HTMLElement;
let settings: Settings | null = null;
let recordingsPath = "";
let perms: Permissions | null = null;
let version = "";
let problem = "";
let capturing = false;

const esc = (t: string) => C().esc(t);

function seg(name: string, options: [string, string][], current: string): string {
  return `<div class="seg" role="radiogroup" aria-label="${name}">${options
    .map(([value, label]) => `<button role="radio" aria-checked="${value === current}" data-${name}="${value}">${label}</button>`)
    .join("")}</div>`;
}

function row(title: string, detail: string, control: string): string {
  return `<div class="set-row"><div class="set-text"><div class="set-title">${title}</div>${detail ? `<div class="set-detail">${detail}</div>` : ""}</div><div class="set-control">${control}</div></div>`;
}

function permissionRow(allowed: boolean, label: string, pane: string, why: string): string {
  const control = allowed
    ? `<span class="chip ok">Allowed</span>`
    : `<span class="chip warn">Needed</span><button class="btn" data-pane="${pane}">Open Settings</button>`;
  return row(label, why, control);
}

function recordingCard(s: Settings): string {
  return `<section class="set-card"><h2>Recording</h2>
    ${row("Save recordings to", `<span class="set-path">${esc(recordingsPath)}</span>`,
      `<button class="btn" data-a="folder">Change…</button><button class="btn" data-a="open-folder">Show in Finder</button>${s.recordings_dir ? '<button class="btn" data-a="folder-reset">Use default</button>' : ""}`)}
    ${row("Start / stop shortcut", capturing ? "Press the keys you want… (Esc to cancel)" : "Works from any app. It records what you last recorded; press it again to stop.",
      `<kbd class="shortcut">${capturing ? "…" : esc(pretty(s.hotkey))}</kbd><button class="btn" data-a="shortcut">${capturing ? "Cancel" : "Change…"}</button>${s.hotkey && !capturing ? '<button class="btn" data-a="shortcut-clear">Remove</button>' : ""}`)}
    ${row("Frame rate", "60 fps is smoother but needs more from your Mac.", seg("fps", FPS.map((f) => [String(f), `${f} fps`]), String(s.fps)))}
    ${row("Countdown", "Time to get ready before recording starts.", seg("countdown", COUNTDOWNS.map((c) => [String(c), c === 0 ? "None" : `${c} s`]), String(s.countdown)))}
    ${row("Show Capturra in recordings", "Off: the recording bar, countdown and menu-bar timer stay out of display and region videos. Turn on to record Capturra itself, for a demo of the app.",
      `<label class="switch"><input type="checkbox" data-a="show-capturra" ${s.show_capturra ? "checked" : ""}><i></i></label>`)}
  </section>`;
}

function permissionsCard(): string {
  if (!perms) return "";
  const all = perms.screen_recording && perms.accessibility && perms.input_monitoring;
  return `<section class="set-card"><h2>Permissions</h2>
    ${permissionRow(perms.screen_recording, "Screen Recording", "screen", "Lets Capturra see your screen.")}
    ${permissionRow(perms.accessibility, "Accessibility", "accessibility", "Lets Capturra follow your pointer and clicks.")}
    ${permissionRow(perms.input_monitoring, "Input Monitoring", "input", "Lets Capturra notice clicks and typing for auto-zoom.")}
    <p class="set-note">${all ? "Everything is allowed." : `After allowing, quit and reopen <b>${esc(perms.responsible_app)}</b> (the app macOS gives these permissions to), then`}
      <a href="#" data-a="recheck">check again</a>.</p>
  </section>`;
}

function appearanceCard(): string {
  return `<section class="set-card"><h2>Appearance</h2>
    ${row("Theme", "Follow your Mac, or pick one.", seg("theme", THEMES, C().store.get("theme", "system")))}
  </section>`;
}

function aboutCard(): string {
  return `<section class="set-card"><h2>About</h2>
    <div class="set-row about-row">
      <svg class="about-mark" viewBox="0 0 920 810" aria-hidden="true"><use href="#i-mark"/></svg>
      <div class="grow">
        <div class="f">Capturra</div>
        <div class="hint">Screen recordings with a smooth cursor and automatic zoom.</div>
      </div>
      ${version ? `<span class="muted">Version ${esc(version)}</span>` : ""}
    </div>
    ${row("Privacy", "Your recordings stay on this Mac. Keystrokes are stored as timing only, never which keys you pressed.", "")}
    <div class="set-row about-links">
      <a href="https://github.com/jerrybarry/capturra" target="_blank" rel="noopener" class="about-link" data-external="true">
        <svg class="ic" viewBox="0 0 24 24" fill="currentColor" stroke="none"><path d="M12 2C6.477 2 2 6.484 2 12.017c0 4.425 2.865 8.18 6.839 9.504.5.092.682-.217.682-.483 0-.237-.008-.868-.013-1.703-2.782.605-3.369-1.343-3.369-1.343-.454-1.158-1.11-1.466-1.11-1.466-.908-.62.069-.608.069-.608 1.003.07 1.531 1.032 1.531 1.032.892 1.53 2.341 1.088 2.91.832.092-.647.35-1.088.636-1.338-2.22-.253-4.555-1.113-4.555-4.951 0-1.093.39-1.988 1.029-2.688-.103-.253-.446-1.272.098-2.65 0 0 .84-.27 2.75 1.026A9.564 9.564 0 0112 6.844a9.59 9.59 0 012.504.338c1.909-1.296 2.747-1.027 2.747-1.027.546 1.379.202 2.398.1 2.651.64.7 1.028 1.595 1.028 2.688 0 3.848-2.339 4.695-4.566 4.943.359.309.678.92.678 1.855 0 1.338-.012 2.419-.012 2.747 0 .268.18.58.688.482A10.02 10.02 0 0022 12.017C22 6.484 17.522 2 12 2z"/></svg>
        <span>GitHub</span>
      </a>
      <a href="https://x.com/iamjerrybarry" target="_blank" rel="noopener" class="about-link" data-external="true">
        <svg class="ic" viewBox="0 0 24 24" fill="currentColor" stroke="none"><path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"/></svg>
        <span>@iamjerrybarry</span>
      </a>
    </div>
  </section>`;
}

function draw() {
  const page = root.querySelector<HTMLElement>(".settings-page");
  if (!page) return;
  page.innerHTML = `${problem ? `<div class="notice warn"><div>${esc(problem)}</div></div>` : ""}
    ${settings ? recordingCard(settings) : '<p class="set-note">Loading settings…</p>'}
    ${permissionsCard()}${appearanceCard()}${aboutCard()}`;
}

async function change(patch: Partial<Settings>) {
  if (!settings) return;
  if (!C().isTauri) { settings = { ...settings, ...patch }; draw(); return; }
  try { settings = await saveSettings({ ...settings, ...patch }); recordingsPath = (await getSettings()).recordings_path; problem = ""; }
  catch (e) { problem = String(e); }
  draw();
}

function stopCapture() {
  capturing = false;
  window.removeEventListener("keydown", onKey, true);
}

function onKey(e: KeyboardEvent) {
  e.preventDefault();
  e.stopPropagation();
  if (e.key === "Escape") { stopCapture(); draw(); return; }
  const next = toShortcut(e);
  if (!next) return; // only a modifier so far, or a key we do not accept
  stopCapture();
  void change({ hotkey: next });
}

/** In a plain browser (no app behind it) the page shows example values so it can be designed and
 *  checked; nothing is saved. */
function previewData() {
  settings = { version: 1, recordings_dir: null, last_source: null, fps: 30, countdown: 3, hotkey: "CommandOrControl+Shift+2", show_capturra: false, system_audio: false, microphone: false };
  recordingsPath = "~/Movies/Capturra";
  perms = { screen_recording: true, accessibility: false, input_monitoring: true, responsible_app: "Terminal" };
  version = "0.1.0 (preview)";
  problem = "Preview only: you are not in the Capturra app, so nothing here is saved.";
}

async function load() {
  if (!C().isTauri) { previewData(); draw(); return; }
  try {
    const loaded = await getSettings();
    settings = loaded.settings;
    recordingsPath = loaded.recordings_path;
    problem = loaded.warning ?? "";
  } catch (e) { problem = `Could not load settings: ${e}`; }
  try { perms = await C().invoke<Permissions>("get_permissions"); } catch { perms = null; }
  try { version = await getVersion(); } catch { version = ""; }
  draw();
}

async function onClick(e: MouseEvent) {
  const target = e.target as HTMLElement;
  const ext = target.closest<HTMLAnchorElement>("a[data-external]");
  if (ext) {
    e.preventDefault();
    const url = ext.href;
    if (C().isTauri) {
      void C().invoke("open_url", { url }).catch(() => window.open(url, "_blank"));
    } else {
      window.open(url, "_blank");
    }
    return;
  }
  const el = target.closest<HTMLElement>("[data-fps],[data-countdown],[data-theme],[data-pane],[data-a]");
  if (!el) return;
  const d = el.dataset;
  if (d.fps) void change({ fps: Number(d.fps) });
  else if (d.countdown) void change({ countdown: Number(d.countdown) });
  else if (d.theme) { C().store.set("theme", d.theme); C().theme.apply(d.theme); draw(); }
  else if (d.pane) void C().invoke("open_privacy_settings", { pane: d.pane }).catch((err) => { problem = String(err); draw(); });
  else if (d.a === "recheck") { e.preventDefault(); perms = await C().invoke<Permissions>("get_permissions").catch(() => perms); draw(); }
  else if (d.a === "folder") {
    try { await change({ recordings_dir: await chooseRecordingsDir() }); }
    catch (err) { if (String(err) !== "cancelled") { problem = String(err); draw(); } }
  } else if (d.a === "folder-reset") void change({ recordings_dir: null });
  else if (d.a === "open-folder") void openRecordingsFolder().catch((err) => { problem = String(err); draw(); });
  else if (d.a === "show-capturra") void change({ show_capturra: (target as HTMLInputElement).checked });
  else if (d.a === "shortcut-clear") void change({ hotkey: "" });
  else if (d.a === "shortcut") {
    if (capturing) stopCapture();
    else { capturing = true; window.addEventListener("keydown", onKey, true); }
    draw();
  }
}

export function mountSettingsView(host: HTMLElement) {
  root = host;
  root.innerHTML = `<div class="topbar"><h1>Settings</h1></div>
    <div class="page-scroll"><div class="settings-page"></div></div>`;
  root.addEventListener("click", (e) => void onClick(e));
  draw();
}

/** Called each time the page is shown: permissions and settings may have changed meanwhile. */
export function refreshSettingsView() {
  stopCapture();
  void load();
}

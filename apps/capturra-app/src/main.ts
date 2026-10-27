import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { pretty, toShortcut } from "./shortcut";
import type { LastSource, LoadedSettings, Settings, SourceKind } from "./settings-api";
import { followTheme } from "./design/theme";
import "./design/tokens.css";
import "./design/components.css";
import "./style.css";

// ---- shapes sent by the Rust side (crates/capturra-recorder, src-tauri/src/main.rs) ----
interface DisplayItem { sharpness?: string; index: number; name: string; x: number; y: number; width: number; height: number; pixel_width: number; pixel_height: number; main: boolean }
interface WindowItem { id: number; app: string; title: string; width: number; height: number }
interface SourceList { displays: DisplayItem[]; windows: WindowItem[]; untitled_hidden: number }
interface Permissions { screen_recording: boolean; accessibility: boolean; input_monitoring: boolean; responsible_app: string }
type Progress =
  | { state: "countdown"; seconds: number }
  | { state: "recording" }
  | { state: "saved"; dir: string; video: string; events: string }
  | { state: "failed"; message: string };

type Kind = SourceKind;
interface StartRequest { kind: Kind; display?: number; window?: number; region?: [number, number, number, number]; fps?: number; countdown?: number; system_audio?: boolean; microphone?: boolean }

// ---- state ----
let perms: Permissions | null = null;
let sources: SourceList | null = null;
let sourcesError = "";
let kind: Kind = "display";
let display = 0;
let windowId: number | null = null;
let region: [number, number, number, number] = [100, 100, 1280, 720];
let progress: Progress | null = null;
let busyMessage = "";
let settings: Settings | null = null;
let settingsWarning = "";
let capturingShortcut = false;

const app = document.getElementById("app")!;
const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]!));

function request(): StartRequest {
  const rate = { fps: settings?.fps, countdown: settings?.countdown };
  if (kind === "window") return { kind, window: windowId ?? undefined, ...rate };
  if (kind === "region") return { kind, display, region, ...rate };
  return { kind, display, ...rate };
}

// ---- settings: remember the last source and where recordings go ----
function lastSource(): LastSource {
  // A window is not remembered: its id is different next time.
  return kind === "region" ? { kind, display, region } : { kind: kind === "window" ? "display" : kind, display };
}

async function saveSettings(change: Partial<Settings>) {
  if (!settings) return;
  try { settings = await invoke<Settings>("save_settings", { settings: { ...settings, ...change } }); settingsWarning = ""; }
  catch (e) { settingsWarning = String(e); }
}

async function loadSettings() {
  try {
    const loaded = await invoke<LoadedSettings>("get_settings");
    settings = loaded.settings;
    settingsWarning = loaded.warning ?? "";
    const last = settings.last_source;
    if (last) {
      kind = last.kind;
      if (last.display !== undefined) display = last.display;
      if (last.region) region = last.region;
    }
  } catch (e) { settingsWarning = String(e); }
}

function shortcutRow(): string {
  if (!settings) return "";
  const keys = settings.hotkey;
  return `<section class="card"><div class="line"><div class="grow"><div class="f">Shortcut</div>
      <div class="path">${capturingShortcut ? "Press the keys… (Esc to cancel)" : esc(pretty(keys))}</div></div>
      <div class="btns"><button class="sec" id="shortcut">${capturingShortcut ? "Cancel" : "Change…"}</button>
      ${keys && !capturingShortcut ? '<button class="sec" id="shortcut-clear">Remove</button>' : ""}</div></div></section>`;
}

function captureShortcut(e: KeyboardEvent) {
  e.preventDefault();
  if (e.key === "Escape") { capturingShortcut = false; window.removeEventListener("keydown", captureShortcut); render(); return; }
  const next = toShortcut(e);
  if (!next) return; // a modifier on its own, or a key we do not accept: keep listening
  capturingShortcut = false;
  window.removeEventListener("keydown", captureShortcut);
  void saveSettings({ hotkey: next }).then(render);
}

function folderRow(): string {
  const dir = settings ? settings.recordings_dir ?? "~/Movies/Capturra" : "";
  return `<section class="card"><div class="line"><div class="grow"><div class="f">Save recordings to</div>
      <div class="path" title="${esc(dir)}">${esc(dir)}</div></div>
      <div class="btns"><button class="sec" id="folder">Change…</button></div></div>
    ${settingsWarning ? `<p class="note err">${esc(settingsWarning)}</p>` : ""}</section>`;
}

function canRecord(): boolean {
  if (!perms?.screen_recording || !perms.accessibility || !perms.input_monitoring) return false;
  if (kind === "window") return windowId !== null;
  return true;
}

// ---- rendering ----
function permRow(ok: boolean, label: string, pane: string): string {
  return `<div class="perm ${ok ? "ok" : ""}"><i class="dot"></i><span>${label}</span>${
    ok ? "" : `<button class="sec" data-pane="${pane}">Open Settings</button>`
  }</div>`;
}

function permissionsCard(): string {
  if (!perms || (perms.screen_recording && perms.accessibility && perms.input_monitoring)) return "";
  return `<section class="card">
    ${permRow(perms.screen_recording, "Screen Recording", "screen")}
    ${permRow(perms.accessibility, "Accessibility", "accessibility")}
    ${permRow(perms.input_monitoring, "Input Monitoring", "input")}
    <p class="note">macOS grants these to <b>${esc(perms.responsible_app)}</b>, the app that launched Capturra.
    After enabling, quit and reopen that app, then <a href="#" id="recheck">check again</a>.</p>
  </section>`;
}

/** A Retina panel running a 1x mode records only the pixels the screen shows, so video looks soft. */
function softDisplayNote(): string {
  const low = sources?.displays.some((d) => d.sharpness === "low_resolution_mode" && (kind === "window" || d.index === display));
  if (!low) return "";
  return `<section class="card"><div class="line"><div class="grow"><div class="f">Display is in a low-resolution mode</div>
    <p class="note" style="margin:2px 0 0">Recordings will look soft. A Retina size (Default or More Space) records at twice the pixels.</p></div>
    <div class="btns"><button class="sec" id="displays">Open Displays</button></div></div></section>`;
}

function sourcePanel(): string {
  if (sourcesError) return `<section class="card err">${esc(sourcesError)}</section>`;
  if (!sources) return `<section class="card note">Loading displays and windows…</section>`;
  const displayOptions = sources.displays
    .map((d) => `<option value="${d.index}" ${d.index === display ? "selected" : ""}>${d.index}: ${esc(d.name || "Display")} (${d.width}×${d.height})${d.main ? " main" : ""}</option>`)
    .join("");
  if (kind === "display") {
    return `<section class="card"><label class="f">Display<select id="display">${displayOptions}</select></label></section>`;
  }
  if (kind === "window") {
    const rows = sources.windows
      .map(
        (w) => `<label class="row"><input type="radio" name="win" value="${w.id}" ${w.id === windowId ? "checked" : ""}/>
          <div><div class="t">${esc(w.title || "(untitled)")}</div><div class="s">${esc(w.app)} · ${w.width}×${w.height}</div></div></label>`,
      )
      .join("");
    return `<section class="card list">${rows || '<p class="note">No windows found.</p>'}
      ${sources.untitled_hidden ? `<p class="note">${sources.untitled_hidden} untitled window(s) hidden: only titled windows can be recorded.</p>` : ""}</section>
      <button class="sec" id="refresh">Refresh windows</button>`;
  }
  const f = (i: number, name: string) => `<label class="f">${name}<input type="number" min="0" data-r="${i}" value="${region[i]}" /></label>`;
  return `<section class="card">
      <label class="f">Display<select id="display">${displayOptions}</select></label>
      <div class="fields" style="margin-top:8px">${f(0, "x (points)")}${f(1, "y (points)")}${f(2, "width")}${f(3, "height")}</div>
      <button class="sec" id="pick" style="margin-top:10px">Select on screen…</button>
      <p class="note">Drag a rectangle over your screen. The numbers above are filled in for you, or type them yourself (points from the display's top-left corner).</p>
      <button class="sec" id="guide" style="margin-top:6px">Show where this region is</button>
    </section>`;
}

function footer(): string {
  if (progress?.state === "countdown") {
    return `<section class="card center"><div class="big">${progress.seconds}</div>Get the screen ready…</section>
      <button class="rec stop" id="stop">Cancel</button>`;
  }
  if (progress?.state === "recording") {
    return `<section class="card center">Recording. Click the menu-bar icon (or this button) to stop${settings?.hotkey ? `, or press ${esc(pretty(settings.hotkey))}` : ""}.</section>
      <button class="rec stop" id="stop">Stop</button>`;
  }
  const saved =
    progress?.state === "saved"
      ? `<section class="card"><b>Saved</b><div class="path">${esc(progress.dir)}</div>
          <button class="sec" id="reveal" data-path="${esc(progress.video)}" style="margin-top:8px">Show in Finder</button></section>`
      : "";
  const failed = progress?.state === "failed" ? `<section class="card err">${esc(progress.message)}</section>` : "";
  const busy = busyMessage ? `<p class="note err">${esc(busyMessage)}</p>` : "";
  return `${saved}${failed}${busy}<button class="rec" id="record" ${canRecord() ? "" : "disabled"}>Record</button>`;
}

function render() {
  const recording = progress?.state === "countdown" || progress?.state === "recording";
  app.innerHTML = `
    <h1><span class="brand-title"><svg class="mark" viewBox="0 0 920 810" aria-hidden="true"><use href="#i-mark"/></svg><span>Capturra</span></span><small>${recording ? "" : "menu-bar recorder"}</small></h1>
    <div class="body">
    ${permissionsCard()}
    ${
      recording
        ? ""
        : `<div class="seg">${(["display", "window", "region"] as Kind[])
            .map((k) => `<button data-kind="${k}" class="${k === kind ? "on" : ""}">${k[0].toUpperCase() + k.slice(1)}</button>`)
            .join("")}</div>${sourcePanel()}${softDisplayNote()}${folderRow()}${shortcutRow()}`
    }
    </div>
    <div class="foot">${footer()}</div>`;
  wire();
}

function wire() {
  app.querySelectorAll<HTMLButtonElement>("[data-kind]").forEach((b) => (b.onclick = () => { kind = b.dataset.kind as Kind; render(); }));
  app.querySelectorAll<HTMLButtonElement>("[data-pane]").forEach((b) => (b.onclick = () => invoke("open_privacy_settings", { pane: b.dataset.pane })));
  app.querySelector<HTMLAnchorElement>("#recheck")?.addEventListener("click", (e) => { e.preventDefault(); void refreshAll(); });
  app.querySelector<HTMLSelectElement>("#display")?.addEventListener("change", (e) => { display = Number((e.target as HTMLSelectElement).value); });
  app.querySelectorAll<HTMLInputElement>('input[name="win"]').forEach((r) => (r.onchange = () => { windowId = Number(r.value); render(); }));
  app.querySelectorAll<HTMLInputElement>("[data-r]").forEach((i) => (i.oninput = () => { region[Number(i.dataset.r)] = Number(i.value); }));
  app.querySelector("#refresh")?.addEventListener("click", () => void refreshSources());
  app.querySelector("#pick")?.addEventListener("click", async () => {
    busyMessage = "";
    try {
      const picked = await invoke<[number, number, number, number] | null>("pick_region", { display });
      if (picked) region = picked;
    } catch (e) { busyMessage = String(e); }
    render();
  });
  app.querySelector("#guide")?.addEventListener("click", async () => {
    busyMessage = "";
    try { await invoke("show_region_guide", { req: request() }); } catch (e) { busyMessage = String(e); render(); }
  });
  app.querySelector("#shortcut")?.addEventListener("click", () => {
    capturingShortcut = !capturingShortcut;
    if (capturingShortcut) window.addEventListener("keydown", captureShortcut, { once: false });
    else window.removeEventListener("keydown", captureShortcut);
    render();
  });
  app.querySelector("#shortcut-clear")?.addEventListener("click", () => void saveSettings({ hotkey: "" }).then(render));
  app.querySelector("#displays")?.addEventListener("click", () => void invoke("open_display_settings"));
  app.querySelector("#folder")?.addEventListener("click", async () => {
    try { await saveSettings({ recordings_dir: await invoke<string>("choose_recordings_dir") }); }
    catch (e) { if (String(e) !== "cancelled") settingsWarning = String(e); }
    render();
  });
  app.querySelector("#record")?.addEventListener("click", async () => {
    busyMessage = "";
    progress = null;
    void saveSettings({ last_source: lastSource() });
    try { await invoke("start_recording", { req: request() }); } catch (e) { busyMessage = String(e); render(); }
  });
  app.querySelector("#stop")?.addEventListener("click", () => void invoke("stop_recording"));
  app.querySelector<HTMLButtonElement>("#reveal")?.addEventListener("click", (e) => void invoke("reveal_in_finder", { path: (e.currentTarget as HTMLButtonElement).dataset.path }));
}

// ---- data ----
async function refreshSources() {
  try { sources = await invoke<SourceList>("get_sources"); sourcesError = ""; } catch (e) { sources = null; sourcesError = String(e); }
  if (windowId !== null && !sources?.windows.some((w) => w.id === windowId)) windowId = null;
  render();
}

async function refreshAll() {
  try { perms = await invoke<Permissions>("get_permissions"); } catch (e) {
    // Outside the Tauri shell (e.g. the Vite URL opened in a browser) there is no backend to call.
    sourcesError = "__TAURI_INTERNALS__" in window
      ? String(e)
      : "This page only works inside the Capturra window. Use the menu-bar icon that `npm run tauri dev` adds, not localhost:1420 in a browser.";
    render();
    return;
  }
  await refreshSources();
}

listen<Progress>("record", (e) => { progress = e.payload; render(); });
// The popover is a menu-bar window: re-read permissions and windows every time it comes forward.
window.addEventListener("focus", () => { if (progress?.state !== "recording" && progress?.state !== "countdown") void refreshAll(); });

followTheme();
render();
void loadSettings().then(() => { render(); void refreshAll(); });

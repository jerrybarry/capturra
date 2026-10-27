// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* bridge.js — the only file that talks to the Rust side.
 *
 * Inside Tauri (with app.withGlobalTauri = true) every call goes to a #[tauri::command].
 * In a plain browser (double-click index.html) the same calls hit the mock backend below, so
 * the whole UI can be designed and tested without the app.
 *
 * Commands that exist in apps/capturra-app/src-tauri today:
 *   get_sources, get_permissions, open_privacy_settings, reveal_in_finder,
 *   show_region_guide, is_recording, start_recording, stop_recording   (event: "record")
 * Commands the Studio window needs that DO NOT EXIST YET (see README.md for exact shapes):
 *   list_projects, load_project, save_project, delete_project, pick_recording,
 *   export_project (event: "export")
 */
(function () {
  'use strict';
  const C = window.Capturra;
  const T = window.__TAURI__;
  C.isTauri = !!(T && T.core && T.core.invoke);
  if (C.isTauri) document.body.classList.add('tauri');

  // Tauri v2 ignores CSS -webkit-app-region, so the overlay title bar drags the window by hand:
  // a press on the empty part of a top bar starts a window drag.
  if (C.isTauri && T.window && T.window.getCurrentWindow) {
    const interactive = 'button, input, select, textarea, a, label, [role=button], [data-no-drag]';
    document.addEventListener('mousedown', (e) => {
      if (e.button !== 0 || e.target.closest(interactive) || !e.target.closest('.topbar, .ed-top')) return;
      T.window.getCurrentWindow().startDragging().catch(() => {});
    });
  }

  const local = {}; // mock event bus
  C.listen = async (evt, fn) => {
    if (C.isTauri && T.event) return T.event.listen(evt, (e) => fn(e.payload));
    (local[evt] = local[evt] || []).push(fn);
    return () => { local[evt] = local[evt].filter((f) => f !== fn); };
  };
  const emit = (evt, payload) => (local[evt] || []).forEach((f) => f(payload));

  C.fileUrl = (path) => (C.isTauri && T.core.convertFileSrc ? T.core.convertFileSrc(path) : path);

  C.invoke = (cmd, args) => (C.isTauri ? T.core.invoke(cmd, args) : mock(cmd, args || {}));

  /* --------------------------- mock backend --------------------------- */
  const params = new URLSearchParams(location.search);
  const mockState = {
    recording: null,
    projects: null,
    perms: { screen_recording: true, accessibility: true, input_monitoring: true, responsible_app: 'Terminal' },
  };
  if (params.get('perms') === 'missing') { mockState.perms.accessibility = false; mockState.perms.input_monitoring = false; }
  if (params.get('library') === 'empty') mockState.projects = [];

  const sources = {
    displays: [
      { index: 0, name: 'Built-in Retina Display', x: 0, y: 0, width: 2048, height: 1280, pixel_width: 2048, pixel_height: 1280, main: true, sharpness: 'low_resolution_mode' },
      { index: 1, name: 'LG UltraFine 5K', x: 2048, y: 0, width: 2560, height: 1440, pixel_width: 5120, pixel_height: 2880, main: false },
    ],
    windows: [
      { id: 4121, app: 'Google Chrome', title: 'Lumen — Customers', width: 1480, height: 920 },
      { id: 4388, app: 'Visual Studio Code', title: 'capturra — main.rs', width: 1320, height: 860 },
      { id: 4402, app: 'Figma', title: 'Onboarding flow v3', width: 1600, height: 1000 },
      { id: 4510, app: 'Terminal', title: 'capturra — zsh', width: 760, height: 480 },
      { id: 4577, app: 'Notion', title: 'Q4 product launch plan', width: 1180, height: 780 },
      { id: 4611, app: 'Finder', title: 'Downloads', width: 920, height: 436 },
      { id: 4702, app: 'Firefox', title: 'DOWNLOAD The Dog Stars (2026) | Download Movies and TV Series Free in HD Quality', width: 1500, height: 900 },
    ],
    untitled_hidden: 3,
  };

  function ensureProjects() {
    if (mockState.projects) return mockState.projects;
    const now = Date.now(), h = 3600000, d = 86400000;
    mockState.projects = [
      { id: 'p1', title: 'Customer onboarding walkthrough', created_at: now - 2 * h, thumb_at: 4200, share_url: null },
      { id: 'p2', title: 'Pricing page tour', created_at: now - 1 * d - 3 * h, thumb_at: 9800, share_url: null },
      { id: 'p3', title: 'Release 0.4 — what’s new', created_at: now - 3 * d, thumb_at: 12800, share_url: null },
      { id: 'p4', title: 'Bug repro: export fails on large tables', created_at: now - 6 * d, thumb_at: 6100, share_url: null },
      { id: 'p5', title: 'Team standup demo', created_at: now - 12 * d, thumb_at: 2600, share_url: null },
    ].map((p) => C.demo.projectMeta(p));
    return mockState.projects;
  }

  const wait = (ms) => new Promise((r) => setTimeout(r, ms));

  async function mock(cmd, a) {
    await wait(30);
    switch (cmd) {
      case 'get_sources': return C.clone(sources);
      case 'get_permissions': return { ...mockState.perms };
      case 'open_privacy_settings': C.toast('Would open System Settings → Privacy & Security'); return null;
      case 'reveal_in_finder': C.toast('Would reveal in Finder: ' + a.path); return null;
      case 'show_region_guide': C.toast('Would show the region outline on screen'); return null;
      case 'is_recording': return !!mockState.recording;
      case 'start_recording': {
        if (mockState.recording) throw 'Already recording.';
        const r = (mockState.recording = { cancelled: false, t0: 0 });
        let n = 3;
        const tick = () => {
          if (r.cancelled) return;
          if (n > 0) { emit('record', { state: 'countdown', seconds: n }); n -= 1; setTimeout(tick, 1000); }
          else { r.t0 = Date.now(); emit('record', { state: 'recording' }); }
        };
        tick();
        return null;
      }
      case 'stop_recording': {
        const r = mockState.recording;
        if (!r) return false;
        mockState.recording = null;
        if (!r.t0) { r.cancelled = true; return true; } // cancelled during countdown
        const id = 'p' + C.uid();
        const dir = '~/Movies/Capturra/Recording ' + new Date().toISOString().slice(0, 16).replace('T', ' ');
        ensureProjects().unshift(C.demo.projectMeta({ id, title: 'New recording', created_at: Date.now(), thumb_at: 4000, share_url: null, dir }));
        emit('record', { state: 'saved', dir, video: dir + '/recording.mp4', events: dir + '/events.json', project_id: id });
        return true;
      }
      case 'list_projects': return ensureProjects().map((p) => ({ ...p }));
      case 'load_project': {
        const p = ensureProjects().find((x) => x.id === a.id || (a.dir && x.dir === a.dir));
        if (!p) throw 'Project not found';
        return C.demo.loadProject(p);
      }
      case 'save_project': return true;
      case 'delete_project': { const l = ensureProjects(); const i = l.findIndex((x) => x.id === a.id); if (i >= 0) l.splice(i, 1); return true; }
      case 'pick_recording': throw 'cancelled';
      case 'export_project': {
        let f = 0;
        await new Promise((res) => {
          const step = () => { f = Math.min(1, f + 0.07 + Math.random() * 0.06); emit('export', { id: a.id, fraction: f }); f >= 1 ? res() : setTimeout(step, 180); };
          step();
        });
        return { path: '~/Movies/Capturra/' + (a.options && a.options.name || 'Export') + '.' + (a.options && a.options.format || 'mp4') };
      }
      case 'list_custom_backgrounds': return (mockState.customBgs || []).map((x) => ({ ...x }));
      case 'add_custom_background': C.toast('Would open the file dialog to pick a picture'); return null;
      case 'delete_custom_background': mockState.customBgs = (mockState.customBgs || []).filter((x) => x.id !== a.id); return true;
      default: throw 'Mock backend: unknown command ' + cmd;
    }
  }
})();

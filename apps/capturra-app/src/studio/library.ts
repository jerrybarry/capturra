// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* library.js — the project grid. Data: list_projects -> [meta]; opening goes through C.app.openProject(meta). */
import { promptRename } from "./rename-dialog";

(function () {
  'use strict';
  const C = window.Capturra;
  const { $, esc } = C;
  const L = (C.library = { items: null, q: '', perms: null });

  L.mount = function () {
    const v = $('#view-library');
    v.innerHTML = `
      <div class="topbar"><h1>Library</h1><span class="grow"></span>
        <div class="search">${C.icon('search')}<input class="input" type="search" data-r="q" placeholder="Search recordings" aria-label="Search recordings"></div>
        <button class="btn" data-a="open">${C.icon('folder', 'sm')} Open recording…</button>
        <input type="file" data-r="file" accept="video/*,.json,application/json" multiple hidden>
      </div>
      <div class="lib-scroll" data-r="scroll"><div data-r="notice"></div><div data-r="body"></div></div>`;
    const r = (L.r = {}); C.$$('[data-r]', v).forEach((e) => (r[e.dataset.r] = e));
    r.q.addEventListener('input', () => { L.q = r.q.value.trim().toLowerCase(); L.render(); });
    v.addEventListener('click', (e) => {
      const a = e.target.closest('[data-a]'); if (!a) return;
      if (a.dataset.a === 'record') C.recorder.open();
      else if (a.dataset.a === 'open') openRecording();
    });
    r.file.addEventListener('change', async () => { const f = Array.from(r.file.files); r.file.value = ''; if (f.length) openFiles(f); });
    // drag a recording.mp4 (+ events.json) anywhere onto the library
    v.addEventListener('dragover', (e) => { if (e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files')) e.preventDefault(); });
    v.addEventListener('drop', (e) => { if (e.dataTransfer && e.dataTransfer.files.length) { e.preventDefault(); openFiles(e.dataTransfer.files); } });
  };

  async function openFiles(files) {
    try { const raw = await C.editor.loadFiles(files); C.app.openRaw(raw); }
    catch (err) { C.toast(String(err), { error: true }); }
  }
  async function openRecording() {
    if (C.isTauri) { // native folder picker lives in Rust: returns the folder that holds recording.mp4 + events.json
      try { const dir = await C.invoke('pick_recording'); if (dir) C.app.openProject({ id: null, dir }); }
      catch (err) { if (String(err) !== 'cancelled') C.toast('Could not open: ' + err, { error: true }); }
    } else L.r.file.click();
  }

  L.refresh = async function () {
    try { L.items = await C.invoke('list_projects'); L.perms = await C.invoke('get_permissions').catch(() => null); }
    catch (err) { L.items = []; C.toast('Could not load the library: ' + err, { error: true }); }
    C.app.setCount(L.items.length); L.render();
  };

  L.render = function () {
    const r = L.r, perms = L.perms;
    const miss = perms && !(perms.accessibility && perms.input_monitoring && perms.screen_recording);
    r.notice.innerHTML = miss ? `<div class="notice warn">${C.icon('warn')}<div><strong>Capturra needs permission to record your cursor.</strong><br><span class="muted">Without Accessibility and Input Monitoring access, recordings have no cursor or auto-zoom data.</span><br><button class="btn" style="margin-top:8px" data-a="record">Review permissions</button></div></div>` : '';
    const list = (L.items || []).filter((p) => !L.q || p.title.toLowerCase().includes(L.q));
    if (!L.items) { r.body.innerHTML = ''; return; }
    if (!L.items.length) {
      r.body.innerHTML = `<div class="empty"><div class="art">${C.icon('record', 'lg')}</div><h2>Record your first demo</h2><p>Capture your screen, and Capturra adds a smooth cursor and automatic zooms to the click and typing moments.</p><button class="btn rec lg" data-a="record">${C.icon('record')} New recording</button></div>`;
      return;
    }
    if (!list.length) { r.body.innerHTML = `<div class="empty"><h2>No matches</h2><p>Nothing in your library matches “${esc(L.q)}”.</p></div>`; return; }
    r.body.innerHTML = `<div class="grid">${list.map((p) => `
      <div class="card-wrap" style="position:relative">
        <button class="card" data-id="${p.id}" style="width:100%" aria-label="Open ${esc(p.title)}">
          <div class="thumb">${p.kind === 'demo' ? '<canvas width="480" height="270"></canvas>' : `<video muted preload="metadata" src="${esc(C.fileUrl(p.video))}#t=${(p.thumb_at || 1000) / 1000}"></video>`}
            <span class="dur">${C.fmtDuration(p.duration_ms)}</span><span class="play"><span>${C.icon('play', 'lg')}</span></span></div>
          <div class="meta"><span class="title">${esc(p.title)}</span><span class="sub">${esc(C.fmtDate(p.created_at))}</span></div>
        </button>
        <button class="more" data-more="${p.id}" aria-label="More actions for ${esc(p.title)}" style="z-index:2">${C.icon('more')}</button></div>`).join('')}</div>`;
    C.$$('.card', r.body).forEach((c) => {
      const p = L.items.find((x) => x.id === c.dataset.id), cv = $('canvas', c);
      if (cv) C.demo.thumb(cv, p.thumb_at);
    });
  };

  document.addEventListener('click', (e) => {
    const lib = $('#view-library'); if (!lib || !lib.contains(e.target)) return;
    const more = e.target.closest('[data-more]'), card = e.target.closest('.card');
    if (more) {
      const p = L.items.find((x) => x.id === more.dataset.more), rc = more.getBoundingClientRect();
      C.menu(rc.left, rc.bottom + 4, [
        { label: 'Open', icon: 'play', run: () => C.app.openProject(p) },
        { label: 'Show in Finder', icon: 'folder', run: () => C.invoke('reveal_in_finder', { path: p.dir }).catch((x) => C.toast(String(x), { error: true })) },
        { label: 'Rename…', icon: 'sliders', run: async () => { const t = await promptRename(p.title); if (!t) return; try { await C.invoke('rename_project', { id: p.id, title: t }); C.toast('Renamed'); L.refresh(); } catch (err) { C.toast('Could not rename: ' + err, { error: true }); } } },
        'hr',
        { label: 'Delete…', icon: 'trash', danger: true, run: async () => { if (await C.confirm('Delete this recording?', `“${p.title}” and its files will be moved to the Trash.`, 'Delete', true)) { try { await C.invoke('delete_project', { id: p.id }); C.toast('Moved to the Trash'); L.refresh(); } catch (err) { C.toast('Could not delete: ' + err, { error: true }); } } } },
      ]);
    } else if (card) C.app.openProject(L.items.find((x) => x.id === card.dataset.id));
  });
})();

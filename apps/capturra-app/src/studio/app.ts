// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* app.js — shell: sidebar, view switching, settings, boot. */
import { mountSettingsView, refreshSettingsView } from "./settings-view";

(function () {
  'use strict';
  const C = window.Capturra;
  const { $ } = C;
  const A = (C.app = {});

  function sidebar() {
    $('#sidebar').innerHTML = `
      <div class="brand"><svg class="mark" aria-hidden="true" viewBox="0 0 920 810"><use href="#i-mark"/></svg><span>Capturra</span></div>
      <button class="btn rec block new-rec" data-a="record">${C.icon('record', 'sm')}<span class="lbl-text">New recording</span></button>
      <nav class="nav" aria-label="Sections">
        <button data-nav="library" aria-current="page">${C.icon('library')}<span class="lbl-text">Library</span><span class="count" data-r="count"></span></button>
        <button data-nav="settings">${C.icon('sliders')}<span class="lbl-text">Settings</span></button>
      </nav>
      <div class="spacer"></div>
      <p class="hint">Your recordings stay on this Mac.</p>`;
    $('#sidebar').addEventListener('click', (e) => {
      const b = e.target.closest('button'); if (!b) return;
      if (b.dataset.nav) A.show(b.dataset.nav);
      else if (b.dataset.a === 'record') C.recorder.open();
    });
  }
  A.setCount = (n) => { const c = $('[data-r=count]'); if (c) c.textContent = n || ''; };

  A.show = function (name) {
    for (const v of ['library', 'settings', 'editor']) $('#view-' + v).hidden = name !== v;
    $('#sidebar').hidden = name === 'editor';
    $('#app').style.gridTemplateColumns = name === 'editor' ? '1fr' : '';
    $('#app').dataset.view = name;
    C.$$('[data-nav]', $('#sidebar')).forEach((b) => b.setAttribute('aria-current', b.dataset.nav === name ? 'page' : 'false'));
    if (name === 'library') C.library.refresh();
    if (name === 'settings') refreshSettingsView();
  };

  A.openRaw = function (raw) { A.show('editor'); C.editor.open(raw); };
  A.openProject = async function (meta) {
    try { A.openRaw(await C.invoke('load_project', { id: meta.id, dir: meta.dir || null })); }
    catch (err) { C.toast('Could not open this recording: ' + err, { error: true }); }
  };
  A.onSaved = function (p) {
    C.library.refresh();
    C.toast('Recording saved', { duration: 9000, action: { label: 'Edit', run: () => A.openProject({ id: p.project_id || null, dir: p.dir }) } });
  };

  function boot() {
    C.theme.apply(C.store.get('theme', 'system'));
    sidebar(); C.library.mount(); mountSettingsView($('#view-settings')); C.editor.mount(); C.recorder.mount();
    C.editor.onBack = () => A.show('library');
    const q = new URLSearchParams(location.search);
    A.show(['library', 'settings'].includes(q.get('view')) ? q.get('view') : 'library');
    if (q.get('sheet') === 'record') C.recorder.open().then(() => { const k = q.get('kind'); if (k) { const b = document.querySelector(`[data-kind="${k}"]`); if (b) b.click(); } });
    if (q.get('open')) C.invoke('list_projects').then((l) => { const p = l[Number(q.get('open')) || 0]; if (p) A.openProject(p).then(() => { if (q.get('tab')) C.editor.setTab(q.get('tab')); }); });
    window.addEventListener('beforeunload', () => C.editor.flushSave && C.editor.flushSave());
  }
  document.addEventListener('DOMContentLoaded', boot);
})();

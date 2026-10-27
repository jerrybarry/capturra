// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* editor.js — the editor view: preview stage, inspector (Zoom / Cursor / Frame / Export), timeline, history, save, export.
 *
 * State lives in C.editor.P:
 *   { meta, display, track, cursors, segments, settings, trim:{in,out}, source:{kind:'demo'|'video', url}, pan }
 * Only `segments`, `settings`, `trim` and the title are saved (save_project). Events and video are never modified.
 */
import { trimWholeMs, withWholeMs } from "./segment-times";

(function () {
  'use strict';
  const C = window.Capturra;
  const { $, $$, esc } = C;

  const DEFAULT_SETTINGS = () => ({
    zoom: { auto: true, preset: 'standard' },
    cursor: { show: true, size: 1.2, smoothing: 0.5, ripple: true, squish: true, hand: true },
    frame: { type: 'gradient', wallpaper: 'aurora', color: '#0e1630', image: '', bg: 0, c1: '#6c63ff', c2: '#3a2fb8', padding: 6, radius: 14, shadow: 0.5, aspect: '16:9' },
    audio: { enhance: false, noise: 0.5, volume: 0, normalize: false, fade_in: 0, fade_out: 0, mute: false },
    export: { format: 'mp4', res: '1080', fps: 30, encoder: 'quality', name: '' },
  });
  const merge = (a, b) => { for (const k in b) a[k] = b[k] && typeof b[k] === 'object' && !Array.isArray(b[k]) ? merge(a[k] || {}, b[k]) : b[k]; return a; };

  /** events.json (version 2) -> flat, sorted list the preview understands. Tolerant on purpose: this is the one place to adjust. */
  C.normalizeEvents = function (json) {
    const list = Array.isArray(json) ? json : json && json.events ? json.events : [];
    return list.map((e) => {
      const o = { ...e, type: e.type || e.kind, t_ms: e.t_ms != null ? e.t_ms : e.t != null ? e.t : e.time_ms };
      if (o.type === 'cursor_change') o.id = e.id != null ? e.id : e.cursor != null ? e.cursor : e.cursor_id;
      return o;
    }).filter((e) => e.t_ms != null && e.type).sort((a, b) => a.t_ms - b.t_ms);
  };

  const E = (C.editor = {
    P: null, t: 0, playing: false, loop: false, sel: null, tab: 'zoom',
    hist: [], hi: 0, saveTimer: 0, stage: null, tl: null, view: null, raf: 0, lastNow: 0, onBack: null,
  });

  /* ------------------------------ build + mount ------------------------------ */
  E.mount = function () {
    const v = (E.view = $('#view-editor'));
    v.innerHTML = `
      <div class="ed-top">
        <button class="btn ghost" data-a="back">${C.icon('chev-left')} Library</button>
        <input class="title-input" data-r="title" aria-label="Recording title" spellcheck="false">
        <span class="saved" data-r="saved" role="status"></span>
        <span class="grow"></span>
        <button class="btn ghost icon" data-a="undo" title="Undo (⌘Z)" aria-label="Undo">${C.icon('undo')}</button>
        <button class="btn ghost icon" data-a="redo" title="Redo (⇧⌘Z)" aria-label="Redo">${C.icon('redo')}</button>
        <button class="btn primary" data-a="export">${C.icon('download', 'sm')} Export</button>
      </div>
      <div class="stage" data-r="stage"></div>
      <aside class="inspector">
        <div class="tabs" role="tablist">
          ${[['zoom', 'zoom', 'Zoom'], ['cursor', 'cursor', 'Cursor'], ['frame', 'image', 'Frame'], ['audio', 'speaker', 'Audio'], ['export', 'download', 'Export']]
            .map(([id, ic, l]) => `<button role="tab" data-tab="${id}" title="${l}" aria-selected="${id === 'zoom'}" ${id === 'audio' ? 'hidden' : ''}>${C.icon(ic)}<span>${l}</span></button>`).join('')}
        </div>
        <div class="panel" data-r="panel" role="tabpanel"></div>
      </aside>
      <section class="timeline" data-r="tl" aria-label="Timeline"></section>`;
    E.r = {};
    $$('[data-r]', v).forEach((e) => (E.r[e.dataset.r] = e));
    E.stage = C.Stage(E.r.stage);
    E.tl = C.Timeline(E.r.tl);

    v.addEventListener('click', (e) => {
      const a = e.target.closest('[data-a]');
      if (a && a.closest('.ed-top')) ({ back: E.close, undo: E.undo, redo: E.redo, export: () => E.setTab('export') })[a.dataset.a]();
      const tab = e.target.closest('[data-tab]'); if (tab) E.setTab(tab.dataset.tab);
    });
    E.r.title.addEventListener('change', () => { E.P.meta.title = E.r.title.value.trim() || 'Untitled'; E.r.title.value = E.P.meta.title; E.commit('Rename'); });
    E.r.title.addEventListener('keydown', (e) => { if (e.key === 'Enter') e.target.blur(); });
    bindPanel();
    document.addEventListener('keydown', onKey);
    C.listen('export', (p) => { if (E.P && p.id === E.P.meta.id) setProgress(p.fraction); });
  };

  E.buildProject = function (raw) {
    const display = raw.display || { width: raw.width, height: raw.height, scale: 1 };
    const events = C.normalizeEvents(raw.events);
    const settings = merge(DEFAULT_SETTINGS(), raw.settings ? C.clone(raw.settings) : {});
    // A recording that is not wide (a window, a phone-shaped region) is a small picture on a 16:9
    // canvas, so unless the user chose a shape, use the recording's own. Matches export_project.rs.
    if (!(raw.settings && raw.settings.frame && raw.settings.frame.aspect) && display.width / display.height < 1.25) settings.frame.aspect = 'source';
    const preset = C.ZOOM_PRESETS[settings.zoom.preset] || C.ZOOM_PRESETS.standard;
    const meta = { id: raw.id, title: raw.title, created_at: raw.created_at, duration_ms: raw.duration_ms, width: display.width, height: display.height, dir: raw.dir, video: raw.video, share_url: raw.share_url, kind: raw.kind };
    return {
      meta, display, cursors: raw.cursors || [], events, track: C.makeCursorTrack(events), settings, pan: preset.pan,
      segments: raw.segments ? C.clone(raw.segments) : C.buildSegments(events, raw.duration_ms, preset, display),
      trim: raw.trim ? C.clone(raw.trim) : { in: 0, out: raw.duration_ms },
      source: raw.kind === 'demo' ? { kind: 'demo' } : { kind: 'video', url: raw.blobUrl || C.fileUrl(raw.video) },
    };
  };

  E.open = function (raw) {
    E.pause();
    const P = (E.P = E.buildProject(raw));
    E.t = P.trim.in; E.sel = null; E.tab = 'zoom'; E.loop = false;
    // a new recording starts without the previous one's processed sound; the timeline reports whether this one has any
    clearTimeout(audTimer); audRun++; E.audOn = false; P.hasAudio = undefined;
    if (E.aud) { E.aud.pause(); E.aud.removeAttribute('src'); E.aud.load(); }
    $('[data-tab=audio]', E.view).hidden = true; E.view.classList.remove('has-audio');
    E.hist = [snap()]; E.hi = 0;
    E.r.title.value = P.meta.title;
    E.r.saved.textContent = '';
    $$('[data-tab]', E.view).forEach((b) => b.setAttribute('aria-selected', String(b.dataset.tab === 'zoom')));
    E.stage.setProject(P);
    E.tl.refresh(); E.tl.setPlaying(false);
    renderPanel(); syncHist();
    loadCustom();
    E.seek(E.t);
    if (P.source.kind === 'video' && !P.events.length) C.toast('No input events found — the video plays, but there is no cursor or auto-zoom data.', { duration: 6000 });
  };

  E.close = function () { E.pause(); flushSave(); if (E.onBack) E.onBack(); };

  /* ------------------------------ player ------------------------------ */
  const vid = () => E.stage.video;
  const isVid = () => E.P.source.kind === 'video';

  /* ------------------------------ sound ------------------------------ */
  // With changes (enhance, volume, fades...) the app renders the recording's sound through the same ffmpeg chain the
  // export uses, and that file plays here in step with the picture, whose own sound is switched off. Without
  // changes the picture plays its own sound. Settings are debounced so dragging a slider renders once.
  let audTimer = 0, audRun = 0;
  const userMuted = () => { try { return localStorage.getItem('capturra.muted') === '1'; } catch { return false; } };
  const audioEdited = () => { const a = E.P.settings.audio; return a.enhance || a.volume !== 0 || a.normalize || a.fade_in > 0 || a.fade_out > 0; };
  E.applyAudioRouting = function () {
    if (!E.P || !E.stage) return;
    const off = userMuted() || E.P.settings.audio.mute;
    vid().muted = off || !!E.audOn;
    if (E.aud) E.aud.muted = off;
  };
  E.onAudioKnown = function (has) {
    const tab = $('[data-tab=audio]', E.view);
    tab.hidden = !has; E.view.classList.toggle('has-audio', has);
    if (!has) { if (E.tab === 'audio') E.setTab('zoom'); return; }
    E.applyAudioRouting();
    if (audioEdited()) buildAudioPreview();
  };
  E.audioChanged = function () {
    if (!E.P || !E.P.hasAudio) return;
    E.applyAudioRouting();
    clearTimeout(audTimer); audTimer = setTimeout(buildAudioPreview, 350);
  };
  async function buildAudioPreview() {
    const P = E.P, run = ++audRun;
    if (!P || !P.hasAudio || !C.isTauri || !P.meta.id) return;
    const note = (msg) => { const n = $('[data-r=audiostate]', E.r.panel); if (n) n.textContent = msg; };
    note('Updating the preview…');
    let path = null;
    try { path = await C.invoke('audio_preview', { id: P.meta.id, audio: P.settings.audio, trimInMs: Math.round(P.trim.in), trimOutMs: Math.round(P.trim.out) }); }
    catch (err) { C.toast('Could not preview the audio changes: ' + err, { error: true, duration: 7000 }); }
    if (run !== audRun || E.P !== P) return; // newer settings, or another recording, while this rendered
    note('Changes are heard in the preview and included when you export. Your recording itself is never changed.');
    if (!E.aud) { E.aud = new Audio(); E.aud.preload = 'auto'; }
    if (path) {
      E.aud.src = C.fileUrl(path);
      E.aud.addEventListener('loadedmetadata', () => { E.aud.currentTime = E.t / 1000; if (E.playing) E.aud.play().catch(() => {}); }, { once: true });
      E.aud.load();
      E.audOn = true;
    } else {
      E.audOn = false; E.aud.pause(); E.aud.removeAttribute('src'); E.aud.load();
      if (E.playing) { vid().currentTime = E.t / 1000; vid().play().catch(() => {}); }
    }
    E.applyAudioRouting();
  }

  E.seek = function (t) {
    const P = E.P; if (!P) return;
    E.t = C.clamp(t, 0, P.meta.duration_ms);
    if (isVid() && !E.playing) { try { vid().currentTime = E.t / 1000; } catch (e) { /* not ready */ } }
    if (E.audOn && !E.playing) { try { E.aud.currentTime = E.t / 1000; } catch (e) { /* not ready */ } }
    E.stage.render(E.t); E.tl.setTime(E.t);
    const cam = C.cameraAt(P.segments, E.t, P.display, P.pan);
    E.stage.setBadge([`${P.display.width}×${P.display.height}`, ...(P.settings.zoom.auto && cam.seg ? [`Zoom ${cam.zoom.toFixed(1)}×`] : [])]);
  };
  E.play = function () {
    const P = E.P; if (!P || E.playing) return;
    if (E.t >= P.trim.out - 30 || E.t < P.trim.in) E.t = P.trim.in;
    E.playing = true; E.tl.setPlaying(true); E.lastNow = performance.now();
    if (isVid()) { vid().currentTime = E.t / 1000; vid().play().catch(() => {}); }
    if (E.audOn) { E.aud.currentTime = E.t / 1000; E.aud.play().catch(() => {}); }
    E.raf = requestAnimationFrame(tick);
  };
  E.pause = function () {
    if (!E.P) return;
    E.playing = false; cancelAnimationFrame(E.raf); E.tl && E.tl.setPlaying(false);
    if (isVid()) vid().pause();
    if (E.aud) E.aud.pause();
  };
  E.toggle = () => (E.playing ? E.pause() : E.play());
  function tick(now) {
    if (!E.playing) return;
    const P = E.P, dt = now - E.lastNow; E.lastNow = now;
    let t = isVid() && vid().readyState >= 2 && !vid().paused ? vid().currentTime * 1000 : E.t + dt;
    if (t >= P.trim.out) {
      if (E.loop) { t = P.trim.in; if (isVid()) vid().currentTime = t / 1000; if (E.audOn) E.aud.currentTime = t / 1000; }
      else { E.pause(); E.seek(P.trim.out); return; }
    }
    // the processed sound is a separate player: pull it back if it wandered from the picture
    if (E.audOn && !E.aud.paused && Math.abs(E.aud.currentTime * 1000 - t) > 150) E.aud.currentTime = t / 1000;
    E.t = t; E.stage.render(t); E.tl.setTime(t);
    E.raf = requestAnimationFrame(tick);
  }

  /* ------------------------------ history + save ------------------------------ */
  function snap() { return JSON.stringify({ s: E.P.segments, c: E.P.settings, t: E.P.trim, n: E.P.meta.title }); }
  function apply(js) {
    const o = JSON.parse(js);
    E.P.segments = o.s; E.P.settings = o.c; E.P.trim = o.t; E.P.meta.title = o.n; E.r.title.value = o.n;
    if (E.sel && !E.P.segments.some((s) => s.id === E.sel)) E.sel = null;
    E.stage.layout(); E.tl.refresh(); renderPanel(); E.seek(E.t); E.audioChanged();
  }
  E.commit = function () {
    const s = snap(); if (s === E.hist[E.hi]) return;
    E.hist = E.hist.slice(0, E.hi + 1); E.hist.push(s); if (E.hist.length > 100) E.hist.shift(); E.hi = E.hist.length - 1;
    syncHist(); scheduleSave();
    { const au = E.P.settings.audio; if (E.P.hasAudio && (au.fade_in > 0 || au.fade_out > 0)) E.audioChanged(); }
  };
  E.undo = function () { if (E.hi > 0) { apply(E.hist[--E.hi]); syncHist(); scheduleSave(); } };
  E.redo = function () { if (E.hi < E.hist.length - 1) { apply(E.hist[++E.hi]); syncHist(); scheduleSave(); } };
  function syncHist() {
    $('[data-a=undo]', E.view).disabled = E.hi === 0;
    $('[data-a=redo]', E.view).disabled = E.hi >= E.hist.length - 1;
  }
  function scheduleSave() { E.r.saved.textContent = 'Saving…'; clearTimeout(E.saveTimer); E.saveTimer = setTimeout(flushSave, 700); }
  async function flushSave() {
    clearTimeout(E.saveTimer); E.saveTimer = 0;
    const P = E.P; if (!P || !E.r.saved || E.r.saved.textContent !== 'Saving…') return;
    try {
      await C.invoke('save_project', { id: P.meta.id, project: { title: P.meta.title, segments: withWholeMs(P.segments), settings: P.settings, trim: trimWholeMs(P.trim) } });
      E.r.saved.textContent = 'Saved';
    } catch (err) { E.r.saved.textContent = 'Not saved'; C.toast('Could not save: ' + err, { error: true }); }
  }
  E.flushSave = flushSave;

  /* ------------------------------ segments ------------------------------ */
  E.select = function (id) { E.sel = id; E.tl.refreshZoom(); if (E.tab === 'zoom') renderPanel(); };
  E.changed = function () { E.stage.render(E.t); E.tl.refreshZoom(); };

  E.addZoom = function (t) {
    const P = E.P, d = P.meta.duration_ms, cfg = C.ZOOM_PRESETS[P.settings.zoom.preset];
    t = C.clamp(t, 0, d);
    const sorted = P.segments.slice().sort((a, b) => a.t0 - b.t0);
    const inside = sorted.find((s) => t >= s.t0 && t <= s.t1);
    if (inside) { E.select(inside.id); C.toast('There is already a zoom here — drag its edges to change it.'); return; }
    const prev = sorted.filter((s) => s.t1 <= t).pop(), next = sorted.find((s) => s.t0 > t);
    const lo = prev ? prev.t1 : 0, hi = next ? next.t0 : d;
    const p0 = t, p1 = Math.min(hi - 100, t + cfg.hold);
    if (p1 - p0 < 300) { C.toast('Not enough room for a zoom here.'); return; }
    const pos = P.track.at(t, 0) || { x: P.display.width / 2, y: P.display.height / 2 };
    const seg = { id: C.uid(), level: cfg.level, enabled: true, manual: null, userEdited: true, p0, p1, t0: Math.max(lo, p0 - cfg.rampIn), t1: Math.min(hi, p1 + cfg.rampOut), focus: [{ t: p0, x: pos.x, y: pos.y }] };
    P.segments.push(seg); P.segments.sort((a, b) => a.t0 - b.t0);
    E.sel = seg.id; E.tl.refreshZoom(); renderPanel(); E.seek(Math.min(t + 400, d)); E.commit('Add zoom');
  };
  E.deleteSeg = function (id) {
    const P = E.P, i = P.segments.findIndex((s) => s.id === (id || E.sel)); if (i < 0) return;
    P.segments.splice(i, 1); E.sel = null; E.tl.refreshZoom(); renderPanel(); E.seek(E.t); E.commit('Delete zoom');
  };
  async function rebuild(presetName) {
    const P = E.P, cfg = C.ZOOM_PRESETS[presetName];
    // In the app the Rust engine builds the zooms (the same one the first load and the export use); a plain
    // browser has no engine, so the simpler JS stand-in does.
    let segments = null;
    if (C.isTauri && P.meta.id) {
      try { segments = await C.invoke('build_zoom', { id: P.meta.id, preset: presetName }); }
      catch (err) { C.toast('Could not rebuild the zooms: ' + err, { error: true }); return; }
      if (E.P !== P) return; // another recording was opened meanwhile
    }
    P.settings.zoom.preset = presetName; P.pan = cfg.pan;
    P.segments = segments || C.buildSegments(P.events, P.meta.duration_ms, cfg, P.display); E.sel = null;
    E.tl.refreshZoom(); renderPanel(); E.seek(E.t); E.commit('Zoom preset');
  }

  /* ------------------------------ inspector ------------------------------ */
  E.setTab = function (tab, scrollTo) {
    E.tab = tab;
    $$('[data-tab]', E.view).forEach((b) => b.setAttribute('aria-selected', String(b.dataset.tab === tab)));
    renderPanel();
    if (scrollTo) { const el = $('#' + scrollTo, E.r.panel); if (el) el.scrollIntoView({ block: 'start' }); }
  };
  E.refreshInspector = function () { if (E.tab === 'zoom') { const d = $('[data-r=segwhen]', E.r.panel); const s = E.P.segments.find((x) => x.id === E.sel); if (d && s) d.textContent = whenText(s); renderList(); } };

  const FMT = { db: (v) => (v > 0 ? '+' : '') + Number(v).toFixed(1) + ' dB', s: (v) => Number(v).toFixed(1) + ' s', x: (v) => Number(v).toFixed(1) + '×', px: (v) => Math.round(v) + ' px', pct: (v) => Math.round(v * 100) + '%', dec: (v) => Number(v).toFixed(2), n: (v) => Math.round(v) + '' };
  const whenText = (s) => `${C.fmtClock(s.p0).replace(/^0/, '')} – ${C.fmtClock(s.p1).replace(/^0/, '')}`;
  const fld = (label, path, inner, fmt, hint) => `<div class="field"><div class="lbl"><span>${label}</span>${fmt ? `<output data-out="${path}">${FMT[fmt](C.getPath(E.P.settings, path))}</output>` : ''}</div>${inner}${hint ? `<p class="hint">${hint}</p>` : ''}</div>`;
  const rng = (label, path, min, max, step, fmt, hint) => fld(label, path, `<input type="range" data-set="${path}" data-fmt="${fmt}" min="${min}" max="${max}" step="${step}" value="${C.getPath(E.P.settings, path)}">`, fmt, hint);
  const sw = (label, sub, path) => `<div class="toggle-row"><div class="t"><span>${label}</span>${sub ? `<small>${sub}</small>` : ''}</div><label class="switch"><input type="checkbox" data-set="${path}" ${C.getPath(E.P.settings, path) ? 'checked' : ''} aria-label="${label}"><i></i></label></div>`;
  const seg = (path, opts, label) => fld(label, path, `<div class="seg" role="radiogroup" data-seg="${path}">${opts.map(([v, l]) => `<button role="radio" data-val="${v}" aria-checked="${String(C.getPath(E.P.settings, path)) === String(v)}">${l}</button>`).join('')}</div>`);

  // The user's own backgrounds live outside the project; fetch them so the stage can draw one and the
  // My Brand tab can list them. A failure only means the list stays empty (the stage then shows the gradient).
  function loadCustom() {
    C.invoke('list_custom_backgrounds').then((list) => {
      C.customBgs = list || [];
      if (E.P) { E.stage.layout(); if (E.tab === 'frame') renderPanel(); }
    }).catch(() => {});
  }

  // whether this recording noted when the pointer was over something clickable and carries the hand to show
  const hasHand = () => !!(E.P.track && E.P.track.hasHover && (E.P.cursors || []).some((c) => c.role === 'pointing_hand'));

  function renderPanel() {
    const P = E.P, S = P.settings, el = E.r.panel, keep = el.scrollTop;
    let h = '';
    if (E.tab === 'zoom') {
      h = `<div class="sect"><h3>Auto zoom</h3>${sw('Zoom on activity', 'Follows clicks and typing', 'zoom.auto')}
        ${fld('Style', 'zoom.preset', `<div class="seg" role="radiogroup" data-seg="zoom.preset" data-preset>${['gentle', 'standard', 'punchy'].map((p) => `<button role="radio" data-val="${p}" aria-checked="${S.zoom.preset === p}">${p[0].toUpperCase() + p.slice(1)}</button>`).join('')}</div>`, null, 'Picking a style rebuilds the zooms from your clicks and typing. Undo brings your edits back.')}
        <div class="field"><div class="lbl"><span>Level for all zooms</span><output data-r="lvlall">${FMT.x(P.segments.length ? P.segments[0].level : C.ZOOM_PRESETS[S.zoom.preset].level)}</output></div>
          <input type="range" data-all-level min="1.2" max="4" step="0.1" value="${P.segments.length ? P.segments[0].level : C.ZOOM_PRESETS[S.zoom.preset].level}"></div></div>
        <hr><div class="sect"><h3>Zooms · ${P.segments.length}</h3><div class="seg-list" data-r="list"></div><div data-r="detail"></div>
        <button class="btn block" data-act="addzoom" style="margin-top:10px">${C.icon('plus', 'sm')} Add zoom at playhead</button></div>`;
    } else if (E.tab === 'cursor') {
      h = `<div class="sect"><h3>Cursor</h3>
        <div class="notice">${C.icon('info')}<span>The cursor is drawn from recorded data, so you can restyle it any time without re-recording.</span></div>
        ${sw('Show cursor', '', 'cursor.show')}${rng('Size', 'cursor.size', 0.6, 2.5, 0.05, 'dec')}${rng('Smoothing', 'cursor.smoothing', 0, 1, 0.05, 'pct', 'Higher is calmer; lower follows your hand exactly.')}
        ${sw('Click ripple', 'A soft ring where you click', 'cursor.ripple')}${sw('Click squish', 'Cursor shrinks slightly while pressed', 'cursor.squish')}
        ${hasHand() ? sw('Hand over clickable things', 'The pointing hand over buttons, links, tabs and menu items', 'cursor.hand') : `<div class="notice">${C.icon('info')}<span>This recording did not note what the pointer was over, so it cannot show the hand. New recordings can (it needs the Accessibility permission).</span></div>`}</div>
        <hr><p class="hint faint" style="margin:0">Keystrokes are stored as timing only (no letters), just enough to know when you were typing.</p>`;
    } else if (E.tab === 'frame') {
      const ty = S.frame.type || 'gradient';
      const tile = (attrs, style, label, on) => `<button role="radio" aria-checked="${on}" ${attrs} style="${style}" aria-label="${C.esc(label)}"></button>`;
      let body;
      if (ty === 'wallpaper') {
        body = `<div class="swatches wide" role="radiogroup" aria-label="Wallpapers">${C.WALLPAPERS.map(([id, l]) => tile(`data-wp="${id}"`, `background:center / cover url('/backgrounds/${id}.png')`, l, S.frame.wallpaper === id)).join('')}</div>`;
      } else if (ty === 'color') {
        body = `<div class="swatches" role="radiogroup" aria-label="Colours">${C.FLAT_COLORS.map((c) => tile(`data-flat="${c}"`, `background:${c}`, c, (S.frame.color || '').toLowerCase() === c)).join('')}</div>
          <div class="color-pair" style="margin-top:12px"><label><input type="color" data-color="color" value="${S.frame.color || '#0e1630'}"> Custom colour</label></div>`;
      } else if (ty === 'custom') {
        body = `<div class="swatches wide" role="radiogroup" aria-label="My pictures">
          <button class="add-tile" data-act="addbg" aria-label="Add a picture">${C.icon('plus')}<span>Add</span></button>
          ${C.customBgs.map((c) => `<span class="tile-wrap">${tile(`data-cbg="${C.esc(c.id)}"`, `background:center / cover url('${C.fileUrl(c.path)}')`, c.id, S.frame.image === c.id)}<button class="tile-x" data-act="delbg" data-id="${C.esc(c.id)}" aria-label="Remove ${C.esc(c.id)}">${C.icon('x', 'sm')}</button></span>`).join('')}</div>
          <p class="hint faint" style="margin:10px 0 0">${C.customBgs.length ? 'Pictures are scaled to fill the frame and cropped at the edges.' : 'Add your own picture (PNG, JPEG, HEIC…). It is kept on this Mac and reused in every project.'}</p>`;
      } else {
        body = `<div class="swatches" role="radiogroup" aria-label="Gradients">
          ${C.BACKGROUNDS.map((b, i) => tile(`data-bg="${i}"`, `background:linear-gradient(135deg,${b[0]},${b[1]})`, 'Gradient ' + (i + 1), S.frame.bg === i)).join('')}</div>
          <div class="color-pair" style="margin-top:12px"><label><input type="color" data-color="c1" value="${S.frame.c1}"> Start</label><label><input type="color" data-color="c2" value="${S.frame.c2}"> End</label></div>`;
      }
      h = `<div class="sect"><h3>Background</h3>
        <div class="seg" role="radiogroup" aria-label="Background type" data-bgtypes style="margin-bottom:12px">${[['wallpaper', 'Wallpapers'], ['gradient', 'Gradient'], ['color', 'Color'], ['custom', 'My Brand']].map(([v, l]) => `<button role="radio" data-bgtype="${v}" aria-checked="${ty === v}">${l}</button>`).join('')}</div>
        ${body}</div>
        <hr><div class="sect"><h3>Screen</h3>${rng('Padding', 'frame.padding', 0, 16, 0.5, 'n', '')}${rng('Corner radius', 'frame.radius', 0, 32, 1, 'px')}${rng('Shadow', 'frame.shadow', 0, 1, 0.05, 'pct')}
        ${fld('Canvas shape', 'frame.aspect', `<select class="input" data-set="frame.aspect">${[['16:9', '16:9 · Widescreen'], ['4:3', '4:3 · Classic'], ['1:1', '1:1 · Square'], ['9:16', '9:16 · Vertical'], ['source', 'Match recording']].map(([v, l]) => `<option value="${v}" ${S.frame.aspect === v ? 'selected' : ''}>${l}</option>`).join('')}</select>`)}</div>`;
    } else if (E.tab === 'audio') {
      h = `<div class="sect"><h3>Voice</h3>
        ${sw('Enhance voice', 'Cuts rumble and background noise, evens out the level and adds clarity', 'audio.enhance')}
        ${rng('Noise reduction', 'audio.noise', 0, 1, 0.05, 'pct', 'How much steady background noise (a fan, hiss) to remove. Too much can make a voice sound thin.')}</div>
        <hr><div class="sect"><h3>Level</h3>
        ${rng('Volume', 'audio.volume', -20, 12, 0.5, 'db')}
        ${sw('Even out loudness', 'Brings the whole sound to a standard level (−16 LUFS), good for sharing online', 'audio.normalize')}</div>
        <hr><div class="sect"><h3>Fades</h3>
        ${rng('Fade in', 'audio.fade_in', 0, 5, 0.1, 's')}${rng('Fade out', 'audio.fade_out', 0, 5, 0.1, 's')}</div>
        <hr><div class="sect">${sw('Remove sound', 'Exports without any audio', 'audio.mute')}
        <button class="btn block" data-act="audioreset" style="margin-top:10px">Reset audio</button>
        <p class="hint faint" style="margin:10px 0 0" data-r="audiostate">Changes are heard in the preview and included when you export. Your recording itself is never changed.</p></div>`;
    } else {
      const ex = S.export;
      h = `<div class="sect"><h3>Export</h3>
        <div class="field"><div class="lbl"><span>File name</span></div><input class="input" type="text" data-set="export.name" value="${esc(ex.name || P.meta.title)}" spellcheck="false"></div>
        ${seg('export.format', [['mp4', 'MP4'], ['gif', 'GIF']], 'Format')}
        ${fld('Resolution', 'export.res', `<select class="input" data-set="export.res">${[['720', '720p'], ['1080', '1080p'], ['1440', '1440p'], ['source', 'Recording size']].map(([v, l]) => `<option value="${v}" ${String(ex.res) === v ? 'selected' : ''}>${l}</option>`).join('')}</select>`)}
        ${seg('export.fps', [[30, '30 fps'], [60, '60 fps']], 'Frame rate')}
        ${ex.format === 'gif' ? '' : seg('export.encoder', [['quality', 'Smaller file'], ['fast', 'Faster']], 'Encoding')}
        ${ex.format === 'gif' ? '' : `<p class="hint" style="margin:-6px 0 12px">${ex.encoder === 'fast' ? 'Uses the Mac’s video hardware: several times quicker, with a file about three times bigger at the same quality.' : 'The smallest file for the quality, but slower to make.'}</p>`}
        <button class="btn primary lg block" data-act="export">${C.icon('download')} Export video</button>
        <div class="progress" data-r="prog" style="margin-top:12px" hidden><i></i></div><p class="hint" data-r="progtxt" style="margin-top:6px"></p>
        ${C.isTauri ? '' : '<p class="hint">Browser preview: export is simulated. In the app it renders with the Rust engine.</p>'}</div>
`;
    }
    const HEAD = {
      zoom: ['Auto zoom', 'Zooms follow your clicks and typing. Drag them on the timeline to fine-tune.'],
      cursor: ['Cursor', 'The pointer is drawn from recorded data, so you can restyle it any time.'],
      frame: ['Background', 'Frame your recording with a colour, padding and rounded corners.'],
      audio: ['Audio', 'Clean up your voice, set the level and fade the sound in and out.'],
      export: ['Export', 'Choose a format and size, then render your video.'],
    }[E.tab];
    h = `<header class="panel-head"><h2>${HEAD[0]}</h2><p>${HEAD[1]}</p></header>` + h;
    el.innerHTML = h; el.scrollTop = keep;
    if (E.tab === 'zoom') { renderList(); renderDetail(); }
    $$('input[type=range]', el).forEach(C.paintRange);
  }

  function renderList() {
    const l = $('[data-r=list]', E.r.panel); if (!l) return;
    const P = E.P;
    l.innerHTML = P.segments.length ? P.segments.map((s, i) => `<button class="seg-row ${s.enabled ? '' : 'off'}" data-id="${s.id}" aria-selected="${s.id === E.sel}"><span class="idx">${i + 1}</span><span class="when">${whenText(s)}</span><span class="lvl">${FMT.x(s.level)}</span></button>`).join('')
      : '<p class="hint" style="margin:6px 0 0">No zooms. Click and type in your recording to create them automatically, or add one by hand.</p>';
  }
  function renderDetail() {
    const d = $('[data-r=detail]', E.r.panel); if (!d) return;
    const s = E.P.segments.find((x) => x.id === E.sel);
    if (!s) { d.innerHTML = ''; return; }
    d.innerHTML = `<div class="seg-detail">
      <div class="toggle-row" style="padding-top:0"><div class="t"><span>Enabled</span><small data-r="segwhen">${whenText(s)}</small></div><label class="switch"><input type="checkbox" data-seg-enabled ${s.enabled ? 'checked' : ''} aria-label="Enabled"><i></i></label></div>
      <div class="field"><div class="lbl"><span>Level</span><output data-r="seglvl">${FMT.x(s.level)}</output></div><input type="range" data-seg-level min="1.2" max="4" step="0.1" value="${s.level}"></div>
      <div class="field" style="margin-bottom:10px"><div class="lbl"><span>Focus</span><span class="chip ${s.manual ? 'accent' : ''}">${s.manual ? 'Fixed point' : 'Follows cursor'}</span></div>
        <div class="grid-2"><button class="btn" data-act="pick">${C.icon('target', 'sm')} Pick point</button><button class="btn" data-act="resetfocus" ${s.manual ? '' : 'disabled'}>Follow cursor</button></div></div>
      <button class="btn danger block" data-act="delseg">${C.icon('trash', 'sm')} Delete zoom</button></div>`;
    $$('input[type=range]', d).forEach(C.paintRange);
  }

  function bindPanel() {
    const p = E.r.panel;
    const live = (path) => { if (path.startsWith('audio.')) E.audioChanged(); else if (path.startsWith('frame.')) E.stage.layout(); else E.stage.render(E.t); };
    p.addEventListener('input', (e) => {
      const t = e.target;
      if (t.dataset.set && t.type === 'range') {
        const v = Number(t.value); C.setPath(E.P.settings, t.dataset.set, v); C.paintRange(t);
        const o = $(`[data-out="${t.dataset.set}"]`, p); if (o) o.textContent = FMT[t.dataset.fmt](v); live(t.dataset.set);
      } else if (t.dataset.setAll !== undefined || t.hasAttribute('data-all-level')) {
        const v = Number(t.value); E.P.segments.forEach((s) => (s.level = v)); C.paintRange(t); $('[data-r=lvlall]', p).textContent = FMT.x(v); E.changed(); renderList();
      } else if (t.hasAttribute('data-seg-level')) {
        const s = E.P.segments.find((x) => x.id === E.sel); if (!s) return;
        s.level = Number(t.value); s.userEdited = true; C.paintRange(t); $('[data-r=seglvl]', p).textContent = FMT.x(s.level); E.changed(); renderList();
      } else if (t.dataset.color) {
        E.P.settings.frame[t.dataset.color] = t.value;
        if (t.dataset.color === 'color') $$('[data-flat]', p).forEach((b) => b.setAttribute('aria-checked', String(b.dataset.flat === t.value.toLowerCase())));
        else { E.P.settings.frame.bg = -1; $$('[data-bg]', p).forEach((b) => b.setAttribute('aria-checked', 'false')); }
        E.stage.layout();
      } else if (t.dataset.set === 'export.name') E.P.settings.export.name = t.value;
    });
    p.addEventListener('change', (e) => {
      const t = e.target;
      if (t.dataset.set && t.type === 'checkbox') { C.setPath(E.P.settings, t.dataset.set, t.checked); live(t.dataset.set); E.seek(E.t); E.commit(); }
      else if (t.dataset.set && t.tagName === 'SELECT') { C.setPath(E.P.settings, t.dataset.set, t.value); live(t.dataset.set); E.commit(); }
      else if (t.hasAttribute('data-seg-enabled')) { const s = E.P.segments.find((x) => x.id === E.sel); if (s) { s.enabled = t.checked; E.changed(); renderList(); E.seek(E.t); E.commit(); } }
      else if (t.dataset.set || t.hasAttribute('data-all-level') || t.hasAttribute('data-seg-level') || t.dataset.color) E.commit();
    });
    p.addEventListener('click', (e) => {
      const b = e.target.closest('button'); if (!b) return;
      const row = b.closest('.seg-row');
      if (row) { E.select(row.dataset.id); const s = E.P.segments.find((x) => x.id === row.dataset.id); E.pause(); E.seek((s.p0 + s.p1) / 2); return; }
      const sg = b.closest('[data-seg]');
      if (sg && b.dataset.val != null) {
        if (sg.hasAttribute('data-preset')) { rebuild(b.dataset.val); return; }
        const cur = C.getPath(E.P.settings, sg.dataset.seg);
        C.setPath(E.P.settings, sg.dataset.seg, typeof cur === 'number' ? Number(b.dataset.val) : b.dataset.val);
        $$('button', sg).forEach((x) => x.setAttribute('aria-checked', String(x === b))); E.commit();
        if (sg.dataset.seg === 'export.format' || sg.dataset.seg === 'export.encoder') renderPanel(); // the hint and the encoding choice depend on them
        return;
      }
      const fr = E.P.settings.frame;
      if (b.dataset.bgtype) { fr.type = b.dataset.bgtype; E.stage.layout(); renderPanel(); if (fr.type === 'custom') loadCustom(); E.commit(); return; }
      if (b.dataset.wp) { fr.type = 'wallpaper'; fr.wallpaper = b.dataset.wp; $$('[data-wp]', p).forEach((x) => x.setAttribute('aria-checked', String(x === b))); E.stage.layout(); E.commit(); return; }
      if (b.dataset.flat) { fr.type = 'color'; fr.color = b.dataset.flat; $$('[data-flat]', p).forEach((x) => x.setAttribute('aria-checked', String(x === b))); const ci = $('[data-color=color]', p); if (ci) ci.value = fr.color; E.stage.layout(); E.commit(); return; }
      if (b.dataset.cbg) { fr.type = 'custom'; fr.image = b.dataset.cbg; $$('[data-cbg]', p).forEach((x) => x.setAttribute('aria-checked', String(x === b))); E.stage.layout(); E.commit(); return; }
      if (b.dataset.act === 'audioreset') { E.P.settings.audio = DEFAULT_SETTINGS().audio; renderPanel(); E.audioChanged(); E.commit(); return; }
      if (b.dataset.act === 'addbg') {
        C.invoke('add_custom_background').then((added) => {
          if (!added) return;
          C.customBgs = [added, ...C.customBgs.filter((x) => x.id !== added.id)];
          fr.type = 'custom'; fr.image = added.id; renderPanel(); E.stage.layout(); E.commit();
        }).catch((err) => C.toast('Could not add the picture: ' + (err && err.message || err)));
        return;
      }
      if (b.dataset.act === 'delbg') {
        const id = b.dataset.id;
        C.invoke('delete_custom_background', { id }).then(() => {
          C.customBgs = C.customBgs.filter((x) => x.id !== id);
          if (fr.type === 'custom' && fr.image === id) { fr.image = ''; fr.type = 'gradient'; E.stage.layout(); E.commit(); }
          renderPanel();
        }).catch((err) => C.toast('Could not remove the picture: ' + (err && err.message || err)));
        return;
      }
      if (b.dataset.bg != null) { fr.type = 'gradient'; E.P.settings.frame.bg = Number(b.dataset.bg); $$('[data-bg]', p).forEach((x) => x.setAttribute('aria-checked', String(x === b))); E.stage.layout(); E.commit(); return; }
      const a = b.dataset.act; if (!a) return;
      if (a === 'addzoom') E.addZoom(E.t);
      else if (a === 'delseg') E.deleteSeg();
      else if (a === 'resetfocus') { const s = E.P.segments.find((x) => x.id === E.sel); s.manual = null; renderDetail(); E.seek(E.t); E.commit(); }
      else if (a === 'pick') {
        const s = E.P.segments.find((x) => x.id === E.sel); E.pause();
        E.stage.pick((pt) => { if (pt) { s.manual = pt; s.userEdited = true; renderDetail(); E.commit(); } E.seek(E.t); });
      } else if (a === 'export') doExport();
    });
  }

  /* ------------------------------ export ------------------------------ */
  function setProgress(f) {
    const pr = $('[data-r=prog]', E.r.panel), tx = $('[data-r=progtxt]', E.r.panel); if (!pr) return;
    pr.hidden = false; $('i', pr).style.width = Math.round(f * 100) + '%'; if (tx) tx.textContent = f < 1 ? `Rendering… ${Math.round(f * 100)}%` : 'Finishing…';
  }
  let exporting = false;
  async function doExport() {
    if (exporting) return; exporting = true;
    const P = E.P, btn = $('[data-act=export]', E.r.panel); btn.disabled = true; setProgress(0); E.pause();
    try {
      E.r.saved.textContent = 'Saving…'; await flushSave();
      const o = P.settings.export, res = await C.invoke('export_project', { id: P.meta.id, options: { ...o, name: o.name || P.meta.title } });
      const tx = $('[data-r=progtxt]', E.r.panel); if (tx) tx.textContent = 'Done · ' + res.path;
      C.toast('Export finished', { action: { label: 'Show in Finder', run: () => C.invoke('reveal_in_finder', { path: res.path }) } });
    } catch (err) { C.toast('Export failed: ' + err, { error: true }); const tx = $('[data-r=progtxt]', E.r.panel); if (tx) tx.textContent = ''; }
    finally { exporting = false; const b = $('[data-act=export]', E.r.panel); if (b) b.disabled = false; }
  }
  /* ------------------------------ keyboard ------------------------------ */
  function onKey(e) {
    if (E.view.hidden || !E.P || document.querySelector('.scrim')) return;
    const tag = (e.target.tagName || '').toLowerCase();
    if (tag === 'input' && e.target.type !== 'range' && e.target.type !== 'checkbox') return;
    if (tag === 'textarea' || tag === 'select') return;
    const mod = e.metaKey || e.ctrlKey, P = E.P;
    if (mod && e.key.toLowerCase() === 'z') { e.preventDefault(); e.shiftKey ? E.redo() : E.undo(); }
    else if (mod && e.key.toLowerCase() === 'e') { e.preventDefault(); E.setTab('export'); }
    else if (mod) return;
    else if (e.key === ' ' && tag !== 'button') { e.preventDefault(); E.toggle(); }
    else if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') { e.preventDefault(); E.pause(); E.seek(E.t + (e.key === 'ArrowLeft' ? -1 : 1) * (e.shiftKey ? 33 : 500)); }
    else if (e.key === 'Home') E.seek(P.trim.in);
    else if (e.key === 'End') E.seek(P.trim.out);
    else if (e.key.toLowerCase() === 'z') E.addZoom(E.t);
    else if (e.key === '[') { P.trim.in = Math.min(E.t, P.trim.out - 500); E.tl.refreshTrim(); E.commit(); }
    else if (e.key === ']') { P.trim.out = Math.max(E.t, P.trim.in + 500); E.tl.refreshTrim(); E.commit(); }
    else if ((e.key === 'Delete' || e.key === 'Backspace') && E.sel) { e.preventDefault(); E.deleteSeg(); }
  }

  /* ------------------------------ open from files (browser / drag-in) ------------------------------ */
  E.loadFiles = async function (files) {
    files = Array.from(files);
    const video = files.find((f) => /^video\//.test(f.type) || /\.(mp4|mov|m4v|webm)$/i.test(f.name));
    const json = files.find((f) => /\.json$/i.test(f.name));
    if (!video) throw 'Choose the recording video (recording.mp4), and optionally its events.json.';
    const url = URL.createObjectURL(video);
    const probe = document.createElement('video'); probe.preload = 'metadata'; probe.src = url;
    await new Promise((res, rej) => { probe.onloadedmetadata = res; probe.onerror = () => rej('This video could not be read.'); });
    let events = [], display = null, cursors = [];
    if (json) { try { const j = JSON.parse(await json.text()); events = j; display = j.display; cursors = j.cursors || []; } catch (e) { throw 'events.json is not valid JSON.'; } }
    const w = probe.videoWidth, h = probe.videoHeight;
    return {
      id: 'local-' + C.uid(), title: video.name.replace(/\.[^.]+$/, ''), created_at: Date.now(), duration_ms: Math.round(probe.duration * 1000), width: w, height: h,
      display: display && display.width ? { width: display.width, height: display.height, scale: display.scale || 1 } : { width: w, height: h, scale: 1 },
      kind: 'video', blobUrl: url, dir: '', events, cursors, share_url: null,
    };
  };
})();

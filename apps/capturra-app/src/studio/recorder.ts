// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* recorder.js — "New recording" sheet, countdown overlay and the floating recording bar.
 * Talks to the existing Rust commands: get_sources, get_permissions, open_privacy_settings({pane}),
 * show_region_guide({req}), start_recording({req}), stop_recording, and the "record" event.
 * req = { kind:'display'|'window'|'region', display?, window?, region?:[x,y,w,h], fps?, countdown? }
 */
(function () {
  'use strict';
  const C = window.Capturra;
  const { $, esc } = C;
  const R = (C.recorder = { sheet: null, hud: null, timer: 0, t0: 0 });
  // `?kind=window` (or region) opens the sheet on that tab, to check a layout in a plain browser
  const S = { kind: new URLSearchParams(location.search).get('kind') || 'display', display: 0, window: null, region: [200, 150, 1280, 720], fps: 30, countdown: 3, audio: false, mic: false };

  R.mount = function () {
    C.listen('record', (p) => {
      if (p.state === 'countdown') showCountdown(p.seconds);
      else if (p.state === 'recording') showHud();
      else if (p.state === 'saved') { clearHud(); C.app.onSaved(p); }
      else if (p.state === 'failed') { clearHud(); C.toast('Recording failed: ' + p.message, { error: true, duration: 8000 }); }
    });
    C.invoke('is_recording').then((on) => { if (on) showHud(); }).catch(() => {});
  };

  R.open = async function () {
    if (R.sheet) return;
    let src, perms;
    try { [src, perms] = await Promise.all([C.invoke('get_sources'), C.invoke('get_permissions')]); }
    catch (err) { C.toast('Could not read your screens: ' + err, { error: true }); return; }
    if (S.window == null && src.windows[0]) S.window = src.windows[0].id;
    R.src = src; R.perms = perms;
    // The saved choice (the shortcut uses it too). A plain browser has no settings: it stays off.
    try { const st = (await C.invoke('get_settings')).settings; S.audio = !!st.system_audio; S.mic = !!st.microphone; } catch { /* keep the last values */ }
    R.sheet = C.sheet({
      title: 'New recording', className: 'wide', onClose: () => { R.sheet = null; },
      body: '<div data-r="b"></div>',
      footer: `<span class="grow" data-r="msg"></span><button class="btn" data-x>Cancel</button><button class="btn rec" data-go>${C.icon('record')} Start recording</button>`,
    });
    R.sheet.el('[data-x]').onclick = () => R.sheet.close();
    R.sheet.el('[data-go]').onclick = start;
    R.sheet.root.addEventListener('click', onClick);
    R.sheet.root.addEventListener('input', onInput);
    paint();
  };

  function ok(v) { return v ? 'ok' : ''; }
  function paint() {
    const { src, perms } = R, b = R.sheet.el('[data-r=b]');
    const pend = [['screen', 'Screen recording', perms.screen_recording], ['accessibility', 'Accessibility', perms.accessibility], ['input', 'Input monitoring', perms.input_monitoring]];
    const all = pend.every((x) => x[2]);
    const sources = S.kind === 'display'
      ? src.displays.map((d) => srcCard('display', d.index, d.name + (d.main ? ' · Main' : ''), `${d.width}×${d.height} pt`, S.display === d.index, 'monitor')).join('')
      : S.kind === 'window'
        ? src.windows.map((w) => srcCard('window', w.id, w.app, w.title || 'Untitled', S.window === w.id, 'window')).join('') : '';
    const softNotice = lowResolutionNotice(src);
    b.innerHTML = `
      ${all ? '' : `<div class="notice warn">${C.icon('warn')}<div style="flex:1"><strong>Allow access to ${esc(perms.responsible_app || 'Capturra')}</strong>
        ${pend.map(([pane, l, g]) => `<div class="perm ${ok(g)}"><span class="dot"></span><span class="grow">${l}</span>${g ? '<span class="chip ok">Allowed</span>' : `<button class="btn" data-pane="${pane}">Open Settings</button>`}</div>`).join('')}
        <span class="muted">After allowing, quit and reopen the app, then come back here.</span></div></div>`}
      <div class="seg" role="radiogroup" aria-label="What to record">
        ${[['display', 'monitor', 'Display'], ['window', 'window', 'Window'], ['region', 'region', 'Region']].map(([k, ic, l]) => `<button role="radio" aria-checked="${S.kind === k}" data-kind="${k}">${C.icon(ic, 'sm')} ${l}</button>`).join('')}</div>
      ${S.kind === 'region' ? regionHtml() : `<div class="src-grid" role="radiogroup">${sources || '<p class="muted">Nothing to record here.</p>'}</div>${S.kind === 'window' && src.untitled_hidden ? `<p class="hint faint" style="margin:-4px 0 12px">${src.untitled_hidden} untitled window${src.untitled_hidden > 1 ? 's are' : ' is'} hidden — macOS can only capture windows that have a title.</p>` : ''}`}
      ${softNotice}
      <div class="grid-2" style="margin-top:6px">
        <div class="field"><div class="lbl"><span>Frame rate</span></div><div class="seg" data-fps>${[30, 60].map((f) => `<button role="radio" aria-checked="${S.fps === f}" data-fps="${f}">${f} fps</button>`).join('')}</div></div>
        <div class="field"><div class="lbl"><span>Countdown</span></div><div class="seg">${[0, 3, 5].map((c) => `<button role="radio" aria-checked="${S.countdown === c}" data-cd="${c}">${c ? c + ' s' : 'None'}</button>`).join('')}</div></div></div>
      <div class="toggle-row"><div class="t"><span>Microphone</span><small>Your voice, from the default input in System Settings &gt; Sound. Mixed with the system audio.</small></div><label class="switch"><input type="checkbox" data-mic ${S.mic ? 'checked' : ''} aria-label="Microphone"><i></i></label></div>
      <div class="toggle-row"><div class="t"><span>System audio</span><small>Everything your Mac plays, except Capturra’s own sound. Needs macOS 13 or newer.</small></div><label class="switch"><input type="checkbox" data-audio ${S.audio ? 'checked' : ''} aria-label="System audio"><i></i></label></div>
      <p class="hint faint" style="margin:8px 0 0">The cursor and keystroke timing are recorded as data, not pixels, so you can restyle them in the editor.</p>`;
    R.sheet.el('[data-go]').disabled = !all || (S.kind === 'window' && S.window == null);
    if (all) loadPreviews();
  }

  // Real pictures for the source cards. One at a time (each is a screenshot), remembered for the
  // life of the sheet, and a failure just leaves the placeholder icon.
  const previews = {}; // 'display:0' -> data URL, or null when it could not be taken
  let previewRun = 0;
  async function loadPreviews() {
    const run = ++previewRun;
    for (const card of C.$$('.src[data-src]', R.sheet.root)) {
      const key = card.dataset.src + ':' + card.dataset.id;
      if (!(key in previews)) {
        try { previews[key] = await C.invoke('source_preview', { kind: card.dataset.src, id: Number(card.dataset.id) }); }
        catch (err) { previews[key] = null; console.warn('preview failed for ' + key + ': ' + err); }
      }
      if (run !== previewRun || !R.sheet) return; // the sheet was redrawn or closed meanwhile
      const pv = previews[key] && card.isConnected ? card.querySelector('.pv') : null;
      if (pv) { pv.style.backgroundImage = `url("${previews[key]}")`; pv.classList.add('has-img'); }
    }
  }
  // A Retina panel running a 1x mode records only as many pixels as the screen shows, so video (and zooms
  // especially) look soft. Say so, with a way to fix it.
  function lowResolutionNotice(src) {
    const low = (d) => d && d.sharpness === 'low_resolution_mode';
    const soft = S.kind === 'window' ? src.displays.some(low) : low(src.displays.find((d) => d.index === S.display));
    if (!soft) return '';
    return `<div class="notice warn">${C.icon('warn')}<div style="flex:1"><strong>This display is in a low-resolution mode, so recordings will look soft.</strong><br>
      <span class="muted">macOS is drawing your screen at 1 pixel per point (the picture is then stretched to the panel). A Retina size records at twice the pixels, which keeps text sharp when the video zooms in. Pick “Default” or “More Space” in Displays. Sharper video is heavier on your Mac: if frames drop, record at 30 fps.</span></div>
      <button class="btn" data-open-displays>Open Displays</button></div>`;
  }
  function srcCard(kind, id, t, s, on, icon) {
    return `<button class="src" role="radio" aria-checked="${on}" data-src="${kind}" data-id="${id}"><div class="pv">${C.icon(icon, 'lg')}</div><div><div class="t">${esc(t)}</div><div class="s">${esc(s)}</div></div></button>`;
  }
  function regionHtml() {
    const d = R.src.displays.find((x) => x.index === S.display) || R.src.displays[0], [x, y, w, h] = S.region;
    const f = (i, l) => `<div class="field" style="margin:0"><div class="lbl"><span>${l}</span></div><input class="input" type="number" min="0" data-reg="${i}" value="${Math.round(S.region[i])}"></div>`;
    return `<div class="region-box">
      <div><div class="region-map" style="aspect-ratio:${d.width}/${d.height}"><span class="lab">${esc(d.name)} · ${d.width}×${d.height}</span>
        <div class="r" data-r="box" style="left:${(x / d.width) * 100}%;top:${(y / d.height) * 100}%;width:${(w / d.width) * 100}%;height:${(h / d.height) * 100}%"></div></div>
        <div class="presets">${[['1280×720', 1280, 720], ['1920×1080', 1920, 1080], ['Square 900', 900, 900], ['Vertical 720×1280', 720, 1280]].map(([l, pw, ph]) => `<button data-preset="${pw},${ph}">${l}</button>`).join('')}</div></div>
      <div><div class="grid-2" style="gap:8px">${f(0, 'X')}${f(1, 'Y')}${f(2, 'Width')}${f(3, 'Height')}</div>
        ${R.src.displays.length > 1 ? `<div class="field" style="margin-top:10px"><div class="lbl"><span>On display</span></div><select class="input" data-disp>${R.src.displays.map((q) => `<option value="${q.index}" ${q.index === S.display ? 'selected' : ''}>${esc(q.name)}</option>`).join('')}</select></div>` : ''}
        <button class="btn block" data-pick style="margin-top:10px">${C.icon('region', 'sm')} Select on screen…</button>
        <button class="btn block" data-guide style="margin-top:6px">${C.icon('eye', 'sm')} Show outline on screen</button>
        <p class="hint" style="margin-top:6px">Points from the display’s top-left corner. Only this region is recorded.</p></div></div>`;
  }

  function req() {
    const q = { kind: S.kind, display: S.display, fps: S.fps, countdown: S.countdown, system_audio: S.audio, microphone: S.mic };
    if (S.kind === 'window') q.window = S.window;
    if (S.kind === 'region') q.region = S.region.map(Number);
    return q;
  }
  function onClick(e) {
    const t = e.target.closest('button'); if (!t) return;
    if (t.dataset.kind) { S.kind = t.dataset.kind; paint(); }
    else if (t.dataset.src) { if (t.dataset.src === 'display') S.display = Number(t.dataset.id); else S.window = Number(t.dataset.id); paint(); }
    else if (t.dataset.fps) { S.fps = Number(t.dataset.fps); paint(); }
    else if (t.dataset.cd != null) { S.countdown = Number(t.dataset.cd); paint(); }
    else if (t.dataset.pane) C.invoke('open_privacy_settings', { pane: t.dataset.pane }).catch((x) => C.toast(String(x), { error: true }));
    else if (t.dataset.preset) { const [w, h] = t.dataset.preset.split(',').map(Number), d = R.src.displays.find((q) => q.index === S.display) || R.src.displays[0]; S.region = [Math.max(0, Math.round((d.width - w) / 2)), Math.max(0, Math.round((d.height - h) / 2)), Math.min(w, d.width), Math.min(h, d.height)]; paint(); }
    else if (t.hasAttribute('data-pick')) C.invoke('pick_region', { display: S.display }).then((r) => { if (r) S.region = r; paint(); }).catch((x) => { C.toast(String(x), { error: true }); paint(); });
    else if (t.hasAttribute('data-open-displays')) C.invoke('open_display_settings').catch((x) => C.toast(String(x), { error: true }));
    else if (t.hasAttribute('data-guide')) C.invoke('show_region_guide', { req: req() }).catch((x) => C.toast(String(x), { error: true }));
  }
  // Remember a choice: the shortcut and the next recording use the saved value.
  function remember(patch) {
    C.invoke('get_settings').then((v) => C.invoke('save_settings', { settings: { ...v.settings, ...patch } }))
      .catch((x) => C.toast('Could not save the choice: ' + x, { error: true }));
  }
  // Turning the microphone on asks macOS for permission first, so a refusal shows here and not as a
  // recording that quietly has no voice.
  async function setMic(box) {
    if (!box.checked) { S.mic = false; remember({ microphone: false }); return; }
    box.disabled = true;
    try {
      const a = await C.invoke('microphone_access', { request: true });
      if (a.status === 'authorized') { S.mic = true; remember({ microphone: true }); }
      else {
        S.mic = false; box.checked = false;
        C.toast(a.status === 'restricted' ? 'Microphone access is restricted on this Mac.' : `Microphone access is off for ${a.app}. Turn it on in System Settings, then quit and reopen the app.`,
          { error: true, duration: 9000, action: a.status === 'denied' ? { label: 'Open Settings', run: () => C.invoke('open_privacy_settings', { pane: 'microphone' }) } : undefined });
      }
    } catch (x) { S.mic = false; box.checked = false; C.toast('Could not check the microphone: ' + x, { error: true }); }
    box.disabled = false;
  }
  function onInput(e) {
    const t = e.target;
    if (t.dataset.audio != null) { S.audio = t.checked; remember({ system_audio: S.audio }); return; }
    if (t.dataset.mic != null) { setMic(t); return; }
    if (t.dataset.reg != null) { S.region[Number(t.dataset.reg)] = Number(t.value) || 0; const box = $('[data-r=box]', R.sheet.root), d = R.src.displays.find((q) => q.index === S.display) || R.src.displays[0]; const [x, y, w, h] = S.region; Object.assign(box.style, { left: (x / d.width) * 100 + '%', top: (y / d.height) * 100 + '%', width: (w / d.width) * 100 + '%', height: (h / d.height) * 100 + '%' }); }
    else if (t.hasAttribute('data-disp')) { S.display = Number(t.value); paint(); }
  }

  async function start() {
    const q = req(), msg = R.sheet.el('[data-r=msg]');
    if (q.kind === 'region' && (q.region[2] < 64 || q.region[3] < 64)) { msg.textContent = 'The region is too small.'; return; }
    R.sheet.el('[data-go]').disabled = true; msg.textContent = 'Starting…';
    try { await C.invoke('start_recording', { req: q }); R.sheet.close(); showHud(true); }
    catch (err) { msg.textContent = ''; R.sheet.el('[data-go]').disabled = false; C.toast(String(err), { error: true, duration: 7000 }); }
  }

  /* ------------------------------ countdown + recording bar ------------------------------ */
  function clearHud() { clearInterval(R.timer); $('#hud-root').innerHTML = ''; R.hud = null; }
  function showCountdown(n) {
    const root = $('#hud-root');
    // Build the overlay once. Redrawing it every second re-created the blurred backdrop and flashed.
    if (!$('.countdown', root)) {
      clearHud();
      root.innerHTML = `<div class="countdown" role="alert"><div class="box"><div class="n"></div><div class="under"><p>Recording starts soon</p><button class="btn" data-cancel>Cancel</button></div></div></div>`;
      $('[data-cancel]', root).onclick = () => { C.invoke('stop_recording'); clearHud(); };
    }
    // Swap only the number: a fresh element plays its one-shot entrance again for each new number.
    const old = $('.n', root), fresh = old.cloneNode(false);
    fresh.textContent = n;
    old.replaceWith(fresh);
  }
  function showHud(waiting) {
    if (waiting) return; // the "recording" event draws the bar
    clearHud(); R.t0 = Date.now();
    $('#hud-root').innerHTML = `<div class="hud" role="status"><span class="dot"></span><span class="time" data-r="time">00:00</span><button class="btn primary" data-stop>${C.icon('stop', 'sm')} Stop</button></div>`;
    const tm = $('[data-r=time]', $('#hud-root'));
    R.timer = setInterval(() => { const s = Math.floor((Date.now() - R.t0) / 1000); tm.textContent = String(Math.floor(s / 60)).padStart(2, '0') + ':' + String(s % 60).padStart(2, '0'); }, 250);
    $('[data-stop]', $('#hud-root')).onclick = async (e) => { e.target.disabled = true; try { await C.invoke('stop_recording'); } catch (err) { C.toast(String(err), { error: true }); } };
  }
})();

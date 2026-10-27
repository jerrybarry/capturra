// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* ui.js — tiny helpers shared by every view. Everything hangs off window.Capturra (C). */
(function () {
  'use strict';
  const C = (window.Capturra = window.Capturra || {});

  C.$ = (sel, root = document) => root.querySelector(sel);
  C.$$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  C.esc = (s) => String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  C.clamp = (v, a, b) => Math.min(b, Math.max(a, v));
  C.lerp = (a, b, t) => a + (b - a) * t;
  C.uid = () => Math.random().toString(36).slice(2, 9);
  C.clone = (o) => JSON.parse(JSON.stringify(o));
  C.icon = (name, cls) => `<svg class="ic ${cls || ''}" aria-hidden="true"><use href="#i-${name}"/></svg>`;

  // 00:04.2
  C.fmtClock = (ms) => {
    const s = Math.max(0, ms) / 1000;
    const m = Math.floor(s / 60);
    return String(m).padStart(2, '0') + ':' + (s % 60).toFixed(1).padStart(4, '0');
  };
  // 0:16 / 1:05:09
  C.fmtDuration = (ms) => {
    const s = Math.round(Math.max(0, ms) / 1000);
    const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = s % 60;
    return h ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}` : `${m}:${String(sec).padStart(2, '0')}`;
  };
  C.fmtDate = (ms) => {
    const d = new Date(ms), now = new Date();
    const days = Math.floor((now - d) / 86400000);
    const time = d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
    if (d.toDateString() === now.toDateString()) return 'Today, ' + time;
    if (days === 1 || (days === 0)) return 'Yesterday, ' + time;
    return d.toLocaleDateString([], { month: 'short', day: 'numeric', year: d.getFullYear() === now.getFullYear() ? undefined : 'numeric' });
  };

  // localStorage can throw or come back empty (private windows, blocked storage): never rely on it.
  C.store = {
    get(key, fallback) { try { const v = localStorage.getItem('capturra.' + key); return v == null ? fallback : JSON.parse(v); } catch (e) { return fallback; } },
    set(key, value) { try { localStorage.setItem('capturra.' + key, JSON.stringify(value)); } catch (e) { /* ignore */ } },
  };

  C.getPath = (obj, path) => path.split('.').reduce((o, k) => (o == null ? o : o[k]), obj);
  C.setPath = (obj, path, value) => {
    const keys = path.split('.');
    const last = keys.pop();
    const target = keys.reduce((o, k) => (o[k] = o[k] || {}), obj);
    target[last] = value;
  };

  // Pointer-drag helper: calls onMove(ev) until the pointer is released.
  C.trackDrag = (startEvent, onMove, onEnd) => {
    startEvent.preventDefault();
    const move = (ev) => onMove(ev);
    const up = (ev) => {
      document.removeEventListener('pointermove', move);
      document.removeEventListener('pointercancel', up);
      document.body.style.cursor = '';
      if (onEnd) onEnd(ev);
    };
    document.addEventListener('pointermove', move);
    document.addEventListener('pointerup', up, { once: true });
    document.addEventListener('pointercancel', up, { once: true });
  };

  // ---- toasts ----
  C.toast = (message, opts = {}) => {
    const root = C.$('#toast-root');
    const el = document.createElement('div');
    el.className = 'toast' + (opts.error ? ' err' : '');
    el.setAttribute('role', opts.error ? 'alert' : 'status');
    el.innerHTML = `<span>${C.esc(message)}</span>${opts.action ? `<button type="button">${C.esc(opts.action.label)}</button>` : ''}`;
    if (opts.action) el.querySelector('button').onclick = () => { opts.action.run(); el.remove(); };
    root.appendChild(el);
    setTimeout(() => el.remove(), opts.duration || (opts.action ? 7000 : 3600));
    return el;
  };

  // ---- sheets (modal dialogs) ----
  // C.sheet({ title, body, footer, className }) -> { root, close(), el(sel) }
  C.sheet = (opts) => {
    const root = C.$('#sheet-root');
    const scrim = document.createElement('div');
    scrim.className = 'scrim';
    scrim.innerHTML = `<div class="sheet ${opts.className || ''}" role="dialog" aria-modal="true" aria-label="${C.esc(opts.title || '')}">
      <header><h2>${C.esc(opts.title || '')}</h2><button class="btn ghost icon" data-close aria-label="Close">${C.icon('x')}</button></header>
      <div class="body">${opts.body || ''}</div>
      ${opts.footer ? `<footer>${opts.footer}</footer>` : ''}
    </div>`;
    const prevFocus = document.activeElement;
    const api = {
      root: scrim,
      el: (sel) => scrim.querySelector(sel),
      all: (sel) => Array.from(scrim.querySelectorAll(sel)),
      close() {
        document.removeEventListener('keydown', onKey, true);
        scrim.remove();
        if (prevFocus && prevFocus.focus) prevFocus.focus();
        if (opts.onClose) opts.onClose();
      },
      setBody(html) { scrim.querySelector('.body').innerHTML = html; },
    };
    const onKey = (e) => {
      if (e.key === 'Escape' && !opts.persistent) { e.stopPropagation(); api.close(); }
      if (e.key === 'Tab') { // keep focus inside the sheet
        const f = scrim.querySelectorAll('button:not(:disabled), input:not(:disabled), select, [tabindex="0"]');
        if (!f.length) return;
        const first = f[0], last = f[f.length - 1];
        if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
        else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
      }
    };
    document.addEventListener('keydown', onKey, true);
    scrim.addEventListener('pointerdown', (e) => { if (e.target === scrim && !opts.persistent) api.close(); });
    scrim.querySelector('[data-close]').onclick = () => api.close();
    root.appendChild(scrim);
    const focusTarget = scrim.querySelector('[autofocus]') || scrim.querySelector('.body button, .body input, .body select, footer button');
    if (focusTarget) focusTarget.focus();
    return api;
  };

  C.confirm = (title, body, okLabel, danger) => new Promise((resolve) => {
    let answered = false;
    const s = C.sheet({
      title, className: 'narrow',
      body: `<p class="muted" style="margin:0">${C.esc(body)}</p>`,
      footer: `<button class="btn" data-no>Cancel</button><button class="btn ${danger ? 'rec' : 'primary'}" data-yes autofocus>${C.esc(okLabel || 'OK')}</button>`,
      onClose: () => { if (!answered) resolve(false); },
    });
    s.el('[data-no]').onclick = () => { answered = true; s.close(); resolve(false); };
    s.el('[data-yes]').onclick = () => { answered = true; s.close(); resolve(true); };
  });

  // Small context menu at (x, y). items: [{label, icon, run, danger}] or 'hr'.
  C.menu = (x, y, items) => {
    C.$$('.menu').forEach((m) => m.remove());
    const m = document.createElement('div');
    m.className = 'menu';
    m.setAttribute('role', 'menu');
    items.forEach((it) => {
      if (it === 'hr') { m.appendChild(document.createElement('hr')); return; }
      const b = document.createElement('button');
      b.type = 'button'; b.setAttribute('role', 'menuitem');
      if (it.danger) b.className = 'danger';
      b.innerHTML = (it.icon ? C.icon(it.icon, 'sm') : '') + `<span>${C.esc(it.label)}</span>`;
      b.onclick = () => { close(); it.run(); };
      m.appendChild(b);
    });
    document.body.appendChild(m);
    const r = m.getBoundingClientRect();
    m.style.left = Math.min(x, innerWidth - r.width - 8) + 'px';
    m.style.top = Math.min(y, innerHeight - r.height - 8) + 'px';
    const close = () => { m.remove(); document.removeEventListener('pointerdown', away, true); document.removeEventListener('keydown', esc, true); };
    const away = (e) => { if (!m.contains(e.target)) close(); };
    const esc = (e) => { if (e.key === 'Escape') close(); };
    setTimeout(() => { document.addEventListener('pointerdown', away, true); document.addEventListener('keydown', esc, true); });
    const first = m.querySelector('button'); if (first) first.focus();
    return close;
  };

  // Paint the filled part of a range input (used by the CSS gradient track).
  C.paintRange = (input) => {
    const min = Number(input.min || 0), max = Number(input.max || 100);
    input.style.setProperty('--fill', ((Number(input.value) - min) / (max - min)) * 100 + '%');
  };

  C.theme = {
    apply(t) { if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t; else delete document.documentElement.dataset.theme; },
  };
})();

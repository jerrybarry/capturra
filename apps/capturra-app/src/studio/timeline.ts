// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* timeline.js — transport bar, ruler, clip track (with trim), zoom track (drag/resize segments), input track.
 * Reads and writes project state through C.editor (see editor.js): P, t, playing, sel, seek(), toggle(),
 * select(), commit(), changed().
 */
import { frameTimes, loadFrames, tileCount } from "./filmstrip";
import { loadWaveform, resample } from "./waveform";

(function () {
  'use strict';
  const C = window.Capturra;
  const MIN_PLATEAU = 250, MIN_GAP = 0;

  C.Timeline = function (root) {
    root.innerHTML = `
      <div class="tl-bar">
        <div class="tl-left">
        <button class="btn" data-a="addzoom" title="Add a zoom at the playhead (Z)">${C.icon('zoom', 'sm')} Add zoom</button>
        <button class="btn ghost" data-a="trimin" title="Set trim start at the playhead ([)">${C.icon('trim', 'sm')} In</button>
        <button class="btn ghost" data-a="trimout" title="Set trim end at the playhead (])">${C.icon('trim', 'sm')} Out</button>
        </div>
        <div class="tl-mid">
        <button class="btn ghost icon" data-a="start" title="Jump to start (Home)" aria-label="Jump to start">${C.icon('skip-back')}</button>
        <button class="btn icon" data-a="play" title="Play / pause (Space)" aria-label="Play">${C.icon('play')}</button>
        <button class="btn ghost icon" data-a="end" title="Jump to end (End)" aria-label="Jump to end">${C.icon('skip-fwd')}</button>
        <div class="time num" aria-live="off"><span data-r="now">00:00.0</span> <small>/ <span data-r="dur">00:00.0</span></small></div>
        <button class="btn ghost icon" data-a="loop" title="Loop" aria-label="Loop" aria-pressed="false">${C.icon('loop')}</button>
        </div>
        <div class="tl-right">
        <button class="btn ghost icon" data-a="mute" hidden title="Mute the recording's sound" aria-label="Mute" aria-pressed="false">${C.icon('speaker')}</button>
        <div class="zoomctl" title="Timeline zoom">${C.icon('search', 'sm')}<input type="range" min="1" max="8" step="0.1" value="1" data-r="zoom" aria-label="Timeline zoom"></div>
        </div>
      </div>
      <div class="tl-body">
        <div class="tl-labels"><div style="height:54px">${C.icon('image', 'sm')} Clip</div><div style="height:44px">${C.icon('zoom', 'sm')} Zoom</div><div style="height:40px">${C.icon('cursor', 'sm')} Input</div><div style="height:40px" data-r="audiolabel" hidden>${C.icon('speaker', 'sm')} Audio</div></div>
        <div class="tl-scroll" data-r="scroll"><div class="tl-inner" data-r="inner">
          <div class="ruler" data-r="ruler"></div>
          <div class="track clip" data-r="clip"></div>
          <div class="track zoomt" data-r="zoomt"></div>
          <div class="track input" data-r="input"></div>
          <div class="track audio" data-r="audio" hidden></div>
          <div class="playhead" data-r="head"></div>
        </div></div>
      </div>`;
    const r = {};
    C.$$('[data-r]', root).forEach((e) => (r[e.dataset.r] = e));
    let mult = 1, pps = 0.05, W = 0;
    const ED = () => C.editor;
    const dur = () => ED().P.meta.duration_ms;
    const x2t = (x) => Math.round((x / W) * dur()); // whole milliseconds, so dragged edges never hold a fraction
    const t2x = (t) => (t / dur()) * W;

    const api = {
      refresh() {
        const P = ED().P;
        if (!P) return;
        const avail = r.scroll.clientWidth - 2;
        W = Math.max(avail, avail * mult);
        r.inner.style.width = W + 'px';
        r.dur.textContent = C.fmtClock(dur());
        drawRuler(); drawClip(); drawAudio(); drawZoom(); drawInput(); api.setTime(ED().t, true);
        const playIcon = r.play ? null : null; void playIcon;
        root.querySelector('[data-a=loop]').setAttribute('aria-pressed', String(!!ED().loop));
      },
      setTime(t, noScroll) {
        const P = ED().P; if (!P) return;
        r.now.textContent = C.fmtClock(t);
        r.head.style.left = t2x(t) + 'px';
        if (!noScroll && ED().playing) {
          const x = t2x(t), sl = r.scroll.scrollLeft, w = r.scroll.clientWidth;
          if (x > sl + w - 40 || x < sl) r.scroll.scrollLeft = Math.max(0, x - 80);
        }
      },
      setPlaying(on) {
        const b = root.querySelector('[data-a=play]');
        b.innerHTML = C.icon(on ? 'pause' : 'play'); b.setAttribute('aria-label', on ? 'Pause' : 'Play');
      },
      refreshZoom() { drawZoom(); },
      refreshTrim() { drawClip(); },
    };

    /* ---------- drawing ---------- */
    function drawRuler() {
      const d = dur() / 1000, pxs = W / d;
      const steps = [0.1, 0.2, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300];
      const step = steps.find((s) => s * pxs >= 70) || 600;
      let h = '';
      for (let s = 0; s <= d + 1e-6; s += step / 2) {
        const major = Math.abs(s / step - Math.round(s / step)) < 1e-6;
        h += `<div class="tick ${major ? 'major' : ''}" style="left:${s * pxs}px">${major ? `<span>${step >= 1 ? C.fmtDuration(s * 1000) : s.toFixed(1) + 's'}</span>` : ''}</div>`;
      }
      r.ruler.innerHTML = h;
    }

    function drawClip() {
      const P = ED().P, tr = P.trim;
      const n = tileCount(W);
      r.clip.innerHTML = `<div class="clip-bar" style="width:${W}px"><div class="strip"></div><span class="cap">${(dur() / 1000).toFixed(1)}s</span></div>
        <div class="trim-shade" style="left:0;width:${t2x(tr.in)}px"></div><div class="trim-shade" style="left:${t2x(tr.out)}px;width:${W - t2x(tr.out)}px"></div>
        <div class="trim-handle l" data-h="in" style="left:${t2x(tr.in)}px" title="Trim start"></div><div class="trim-handle r" data-h="out" style="left:${t2x(tr.out)}px" title="Trim end"></div>`;
      const strip = C.$('.strip', r.clip);
      if (P.source.kind === 'demo') {
        for (let i = 0; i < n; i++) {
          const c = document.createElement('canvas'); c.width = 160; c.height = 90; strip.appendChild(c);
          C.demo.thumb(c, ((i + 0.5) / n) * dur());
        }
      } else if (P.source.kind === 'video') fillFilmstrip(strip, P, n);
    }

    // Real recordings: frames taken by the app (ffmpeg) at even spacing. Fetched once per tile count
    // and remembered, because the clip is redrawn on every zoom or trim change.
    let stripRun = 0;
    function paintTile(canvas, img) {
      canvas.height = Math.max(1, Math.round(canvas.width * img.naturalHeight / img.naturalWidth));
      canvas.getContext('2d').drawImage(img, 0, 0, canvas.width, canvas.height);
    }
    async function fillFilmstrip(strip, P, n) {
      if (!C.isTauri || !P.meta.id) return; // only the app can read frames; elsewhere the bar stays plain
      const run = ++stripRun, key = P.meta.id + '|' + n;
      const canvases = Array.from({ length: n }, () => { const c = document.createElement('canvas'); c.width = 160; c.height = 100; strip.appendChild(c); return c; });
      if (!P._strip || P._strip.key !== key) {
        const frames = await loadFrames(P.meta.id, frameTimes(dur(), n));
        if (run !== stripRun) return; // the clip was redrawn while this loaded
        P._strip = { key, frames };
      }
      P._strip.frames.forEach((img, i) => img && paintTile(canvases[i], img));
    }

    // The sound of a real recording, as bars. The outline is fetched once per project (the app decodes the
    // audio with ffmpeg) and redrawn from memory when the timeline is zoomed. A recording without sound
    // shows no track and no mute button.
    let audioRun = 0;
    const muted = () => { try { return localStorage.getItem('capturra.muted') === '1'; } catch { return false; } };
    function applyMute() {
      const on = muted(), btn = root.querySelector('[data-a=mute]');
      ED().applyAudioRouting();
      btn.setAttribute('aria-pressed', String(on));
      btn.setAttribute('title', on ? 'Unmute the recording\u2019s sound' : 'Mute the recording\u2019s sound');
      btn.setAttribute('aria-label', on ? 'Unmute' : 'Mute');
    }
    async function drawAudio() {
      const P = ED().P, run = ++audioRun, label = root.querySelector('[data-r=audiolabel]'), btn = root.querySelector('[data-a=mute]');
      const hide = () => { r.audio.hidden = true; label.hidden = true; btn.hidden = true; if (P.hasAudio !== false) { P.hasAudio = false; ED().onAudioKnown(false); } };
      // `?audio=1` in a plain browser draws a made-up sound, to design and check the Audio track and tab
      const mock = !C.isTauri && new URLSearchParams(location.search).has('audio');
      if (!mock && (!C.isTauri || !P.meta.id || P.source.kind !== 'video')) return hide();
      if (!P._waveP) P._waveP = mock ? Promise.resolve(Array.from({ length: 900 }, (_, i) => Math.min(1, 0.15 + Math.abs(Math.sin(i / 7)) * Math.abs(Math.sin(i / 53)) * 0.9))) : loadWaveform(P.meta.id);
      const peaks = await P._waveP;
      if (run !== audioRun || ED().P !== P) return; // redrawn, or another project, while this loaded
      if (!peaks) return hide();
      r.audio.hidden = false; label.hidden = false; btn.hidden = false; applyMute();
      if (P.hasAudio !== true) { P.hasAudio = true; ED().onAudioKnown(true); }
      const dpr = Math.min(2, window.devicePixelRatio || 1), H = 40;
      const c = document.createElement('canvas');
      c.className = 'input-canvas'; c.style.width = W + 'px'; c.width = Math.round(W * dpr); c.height = Math.round(H * dpr);
      const g = c.getContext('2d'); g.scale(dpr, dpr);
      g.fillStyle = getComputedStyle(document.documentElement).getPropertyValue('--accent').trim() || '#5a5af2';
      g.globalAlpha = 0.75;
      resample(peaks, Math.max(1, Math.floor(W / 3))).forEach((v, b) => { const h = Math.max(2, v * (H - 6)); g.fillRect(b * 3, (H - h) / 2, 2, h); });
      r.audio.innerHTML = ''; r.audio.appendChild(c);
    }

    function drawZoom() {
      const P = ED().P, sel = ED().sel;
      r.zoomt.innerHTML = P.segments.map((s, i) => {
        const l = t2x(s.t0), w = Math.max(6, t2x(s.t1) - l), pl = t2x(s.p0) - l, pw = Math.max(4, t2x(s.p1) - t2x(s.p0));
        return `<div class="zseg ${s.id === sel ? 'sel' : ''} ${s.enabled ? '' : 'off'}" data-id="${s.id}" style="left:${l}px;width:${w}px" title="Zoom ${i + 1} · ${s.level.toFixed(1)}×">
          <div class="plateau" style="left:${pl}px;width:${pw}px">${pw > 38 ? s.level.toFixed(1) + '×' : ''}</div><div class="h l" data-e="l"></div><div class="h r" data-e="r"></div></div>`;
      }).join('') + (P.segments.length ? '' : '<div class="tl-hint">No zooms yet — press Z, or use “Add zoom”.</div>');
    }

    function drawInput() {
      const P = ED().P, c = document.createElement('canvas'), dpr = Math.min(2, window.devicePixelRatio || 1);
      c.className = 'input-canvas'; c.style.width = W + 'px'; c.width = Math.round(W * dpr); c.height = Math.round(40 * dpr);
      const g = c.getContext('2d'); g.scale(dpr, dpr);
      const cs = getComputedStyle(document.documentElement);
      const accent = cs.getPropertyValue('--accent').trim() || '#5a5af2', mute = cs.getPropertyValue('--text-3').trim() || '#888';
      // pointer activity (speed) as faint bars
      const bins = Math.max(1, Math.floor(W / 3)), act = new Float32Array(bins), mv = P.track.moves;
      for (let i = 1; i < mv.length; i++) { const b = Math.min(bins - 1, Math.floor((mv[i].t_ms / dur()) * bins)); act[b] += Math.hypot(mv[i].x - mv[i - 1].x, mv[i].y - mv[i - 1].y); }
      const mx = Math.max(1, ...act);
      g.fillStyle = mute; g.globalAlpha = 0.28;
      for (let b = 0; b < bins; b++) { const h = Math.sqrt(act[b] / mx) * 24; g.fillRect(b * 3, 36 - h, 2, h); }
      g.globalAlpha = 0.9;
      for (const k of P.track.keys) g.fillRect(t2x(k) - 1, 30, 2, 6);   // key presses: timing only
      g.fillStyle = accent; g.globalAlpha = 1;
      for (const d of P.track.downs) { g.beginPath(); g.arc(t2x(d.t_ms), 12, 4.5, 0, 7); g.fill(); }
      r.input.innerHTML = ''; r.input.appendChild(c);
    }

    /* ---------- interaction ---------- */
    function seekFromEvent(e) {
      const rect = r.inner.getBoundingClientRect();
      ED().seek(C.clamp(x2t(e.clientX - rect.left), 0, dur()));
    }
    function scrub(e) {
      if (e.button !== 0) return;
      const wasPlaying = ED().playing; if (wasPlaying) ED().pause();
      seekFromEvent(e);
      C.trackDrag(e, seekFromEvent, () => { if (wasPlaying) ED().play(); });
    }
    r.ruler.addEventListener('pointerdown', scrub);
    r.input.addEventListener('pointerdown', scrub);
    r.clip.addEventListener('pointerdown', (e) => {
      const h = e.target.dataset.h;
      if (!h) { if (e.target.closest('.clip-bar')) scrub(e); return; }
      const P = ED().P, rect = r.inner.getBoundingClientRect();
      C.trackDrag(e, (ev) => {
        const t = C.clamp(x2t(ev.clientX - rect.left), 0, dur());
        if (h === 'in') P.trim.in = Math.min(t, P.trim.out - 500); else P.trim.out = Math.max(t, P.trim.in + 500);
        drawClip(); ED().seek(h === 'in' ? P.trim.in : P.trim.out);
      }, () => ED().commit('Trim'));
    });

    r.zoomt.addEventListener('dblclick', (e) => {
      if (e.target.closest('.zseg')) return;
      const rect = r.inner.getBoundingClientRect();
      ED().addZoom(x2t(e.clientX - rect.left));
    });
    r.zoomt.addEventListener('pointerdown', (e) => {
      const el = e.target.closest('.zseg');
      if (!el) { ED().select(null); scrub(e); return; }
      if (e.button !== 0) return;
      const P = ED().P, segs = P.segments, i = segs.findIndex((s) => s.id === el.dataset.id), s = segs[i];
      ED().select(s.id);
      const edge = e.target.dataset.e || 'm';
      const o = { t0: s.t0, p0: s.p0, p1: s.p1, t1: s.t1 }, x0 = e.clientX, dtOf = (ev) => x2t(ev.clientX - x0) - 0;
      const prev = segs[i - 1], next = segs[i + 1];
      const lo = prev ? prev.t1 + MIN_GAP : 0, hi = next ? next.t0 - MIN_GAP : dur();
      let moved = false;
      C.trackDrag(e, (ev) => {
        let d = x2t(ev.clientX - x0); if (Math.abs(ev.clientX - x0) > 2) moved = true;
        if (edge === 'm') {
          d = C.clamp(d, lo - o.t0, hi - o.t1);
          s.t0 = o.t0 + d; s.p0 = o.p0 + d; s.p1 = o.p1 + d; s.t1 = o.t1 + d;
        } else if (edge === 'l') {
          const p0 = C.clamp(o.p0 + d, lo + 0, o.p1 - MIN_PLATEAU), rin = Math.min(o.p0 - o.t0, p0 - lo);
          s.p0 = p0; s.t0 = p0 - rin;
        } else {
          const p1 = C.clamp(o.p1 + d, o.p0 + MIN_PLATEAU, hi), rout = Math.min(o.t1 - o.p1, hi - p1);
          s.p1 = p1; s.t1 = p1 + rout;
        }
        s.userEdited = true;
        drawZoom(); ED().seek(ED().t, true); ED().refreshInspector();
      }, () => { if (moved) ED().commit('Edit zoom'); });
    });

    root.addEventListener('click', (e) => {
      const b = e.target.closest('[data-a]'); if (!b) return;
      const a = b.dataset.a, E = ED();
      if (a === 'play') E.toggle();
      else if (a === 'start') E.seek(E.P.trim.in);
      else if (a === 'end') E.seek(E.P.trim.out);
      else if (a === 'mute') { try { localStorage.setItem('capturra.muted', muted() ? '0' : '1'); } catch { /* not remembered */ } applyMute(); }
      else if (a === 'loop') { E.loop = !E.loop; b.setAttribute('aria-pressed', String(E.loop)); }
      else if (a === 'addzoom') E.addZoom(E.t);
      else if (a === 'trimin') { E.P.trim.in = Math.min(E.t, E.P.trim.out - 500); drawClip(); E.commit('Trim'); }
      else if (a === 'trimout') { E.P.trim.out = Math.max(E.t, E.P.trim.in + 500); drawClip(); E.commit('Trim'); }
    });
    r.zoom.addEventListener('input', () => {
      const centre = x2t(r.scroll.scrollLeft + r.scroll.clientWidth / 2);
      mult = Number(r.zoom.value); C.paintRange(r.zoom); api.refresh();
      r.scroll.scrollLeft = t2x(centre) - r.scroll.clientWidth / 2;
    });
    C.paintRange(r.zoom);
    new ResizeObserver(() => { if (ED() && ED().P) api.refresh(); }).observe(r.scroll);
    return api;
  };
})();

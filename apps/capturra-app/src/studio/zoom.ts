// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* zoom.js — a JS port of the zoom/cursor logic, used for the live preview inside the editor.
 *
 * The authoritative engine is Rust (crates/capturra-core: zoom.rs, cursor.rs) and exports are rendered
 * from it. In the app, load_project should return the Rust-computed `segments`; this file only
 * (a) builds a starting set in the browser / demo mode and (b) evaluates the camera for any time t.
 * In the app the zooms (first load AND the style picker, `build_zoom`) come from the Rust engine; this builder
 * is only the stand-in for a plain browser and the demo, a simpler version without click clustering, follow
 * or level fitting. Keep PRESETS identical to `ZoomConfig::preset` in zoom.rs.
 *
 * Times are milliseconds on the video clock. Positions are display points (events.json space).
 */
(function () {
  'use strict';
  const C = window.Capturra;
  const { clamp, lerp, uid } = C;

  C.ease = (t) => { t = clamp(t, 0, 1); return t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2; };

  C.ZOOM_PRESETS = {
    gentle:   { level: 1.6, lead: 250, hold: 1600, rampIn: 700, rampOut: 800, merge: 2400, pan: 600 },
    standard: { level: 2.0, lead: 150, hold: 1200, rampIn: 450, rampOut: 600, merge: 2000, pan: 450 }, // = ZoomConfig::default()
    punchy:   { level: 2.6, lead: 100, hold: 1000, rampIn: 300, rampOut: 420, merge: 1500, pan: 320 },
  };
  const KEY_BURST_GAP = 800;
  const MIN_PAN = 0.04; // fraction of the display; smaller focus moves are ignored

  /** Build zoom segments from events. Returns [{id,t0,p0,p1,t1,level,enabled,focus:[{t,x,y}],manual:null|{x,y}}] */
  C.buildSegments = function (events, durationMs, cfg, display) {
    const trig = [];
    let px = display.width / 2, py = display.height / 2;
    let burst = null;
    const flushBurst = () => { if (burst) { trig.push({ t: burst.start, x: burst.x, y: burst.y }); burst = null; } };
    for (const e of events) {
      if (e.type === 'move') { px = e.x; py = e.y; }
      else if (e.type === 'down') { flushBurst(); trig.push({ t: e.t_ms, x: e.x, y: e.y }); }
      else if (e.type === 'key') {
        if (burst && e.t_ms - burst.last <= KEY_BURST_GAP) burst.last = e.t_ms;
        else { flushBurst(); burst = { start: e.t_ms, last: e.t_ms, x: px, y: py }; }
      }
    }
    flushBurst();
    trig.sort((a, b) => a.t - b.t);

    const raw = [];
    for (const tr of trig) {
      const hs = Math.max(0, tr.t - cfg.lead), he = tr.t + cfg.hold;
      const last = raw[raw.length - 1];
      if (last && hs - last.p1 <= cfg.merge) { last.p1 = Math.max(last.p1, he); last.focus.push({ t: hs, x: tr.x, y: tr.y }); }
      else raw.push({ p0: hs, p1: he, focus: [{ t: hs, x: tr.x, y: tr.y }] });
    }
    const segs = raw.map((s) => ({
      id: uid(), level: cfg.level, enabled: true, manual: null,
      p0: s.p0, p1: s.p1, t0: Math.max(0, s.p0 - cfg.rampIn), t1: Math.min(durationMs, s.p1 + cfg.rampOut),
      focus: dropTinyPans(s.focus, display),
    }));
    // never let two segments' ramps overlap: split the shared gap in the middle
    for (let i = 1; i < segs.length; i++) {
      const a = segs[i - 1], b = segs[i];
      if (b.t0 < a.t1) { const mid = (a.p1 + b.p0) / 2; a.t1 = Math.max(a.p1, mid); b.t0 = Math.min(b.p0, mid); }
    }
    for (const s of segs) { s.p1 = Math.min(s.p1, s.t1); s.p0 = Math.min(s.p0, s.p1); }
    return segs;
  };

  function dropTinyPans(focus, display) {
    const out = [focus[0]];
    for (let i = 1; i < focus.length; i++) {
      const p = out[out.length - 1], f = focus[i];
      if (Math.hypot((f.x - p.x) / display.width, (f.y - p.y) / display.height) >= MIN_PAN) out.push(f);
    }
    return out;
  }

  function focusAt(seg, t, pan, display) {
    if (seg.manual) return seg.manual;
    const kf = seg.focus;
    let cur = kf[0];
    for (let i = 1; i < kf.length; i++) {
      const k = kf[i], start = k.t - pan, prev = kf[i - 1];
      if (t >= k.t) cur = k;
      else if (t > start) { const u = C.ease((t - start) / pan); return { x: lerp(prev.x, k.x, u), y: lerp(prev.y, k.y, u) }; }
      else break;
    }
    return cur;
  }

  /** Camera for time t -> {zoom, cx, cy (0..1 view centre), seg}. */
  C.cameraAt = function (segs, t, display, pan) {
    for (const s of segs) {
      if (!s.enabled || t < s.t0 || t > s.t1) continue;
      let e = 1;
      if (t < s.p0) e = C.ease((t - s.t0) / Math.max(1, s.p0 - s.t0));
      else if (t > s.p1) e = C.ease((s.t1 - t) / Math.max(1, s.t1 - s.p1));
      const z = 1 + (s.level - 1) * e;
      const f = focusAt(s, t, pan || 450, display);
      const half = 0.5 / z;
      return { zoom: z, cx: clamp(f.x / display.width, half, 1 - half), cy: clamp(f.y / display.height, half, 1 - half), seg: s };
    }
    return { zoom: 1, cx: 0.5, cy: 0.5, seg: null };
  };

  /* ------------------------------ cursor track ------------------------------ */
  /** Sorted pointer samples + click list + cursor-shape changes. */
  C.makeCursorTrack = function (events) {
    const mv = [], downs = [], ups = [], keys = [], shapes = [], hovers = [];
    for (const e of events) {
      if (e.type === 'move') mv.push(e);
      else if (e.type === 'down') { downs.push(e); mv.push({ t_ms: e.t_ms, x: e.x, y: e.y }); }
      else if (e.type === 'up') { ups.push(e); mv.push({ t_ms: e.t_ms, x: e.x, y: e.y }); }
      else if (e.type === 'key') keys.push(e.t_ms);
      else if (e.type === 'cursor_change') shapes.push(e);
      else if (e.type === 'hover') hovers.push(e);
    }
    hovers.sort((a, b) => a.t_ms - b.t_ms);
    mv.sort((a, b) => a.t_ms - b.t_ms);
    shapes.sort((a, b) => a.t_ms - b.t_ms);
    downs.sort((a, b) => a.t_ms - b.t_ms);
    const t = mv.map((m) => m.t_ms);
    const st = shapes.map((s) => s.t_ms);
    const bs = (arr, v) => { let lo = 0, hi = arr.length; while (lo < hi) { const mid = (lo + hi) >> 1; if (arr[mid] <= v) lo = mid + 1; else hi = mid; } return lo; };

    function rawAt(ms) {
      if (!mv.length) return null;
      const i = bs(t, ms);
      if (i === 0) return { x: mv[0].x, y: mv[0].y, before: true };
      if (i >= mv.length) return { x: mv[mv.length - 1].x, y: mv[mv.length - 1].y };
      const a = mv[i - 1], b = mv[i], u = (ms - a.t_ms) / Math.max(1, b.t_ms - a.t_ms);
      // long gaps are "pointer was still", so hold position instead of drifting
      return u > 0 && b.t_ms - a.t_ms > 120 ? { x: a.x, y: a.y } : { x: lerp(a.x, b.x, u), y: lerp(a.y, b.y, u) };
    }
    return {
      moves: mv, downs, ups, keys, shapes,
      /** smoothed pointer position; smoothing 0..1 averages the last up-to-170ms */
      at(ms, smoothing) {
        const w = clamp(smoothing == null ? 0.5 : smoothing, 0, 1) * 170;
        if (w < 1) return rawAt(ms);
        let sx = 0, sy = 0, n = 0;
        for (let k = 0; k < 6; k++) { const p = rawAt(ms - (w * k) / 5); if (p) { sx += p.x; sy += p.y; n++; } }
        return n ? { x: sx / n, y: sy / n } : null;
      },
      /** whether the pointer is over something clickable (recordings that noted it) */
      clickableAt(ms) { let on = false; for (const h of hovers) { if (h.t_ms > ms) break; on = !!h.clickable; } return on; },
      hasHover: hovers.length > 0,
      shapeAt(ms) { const i = bs(st, ms); return i ? shapes[i - 1].id : (shapes[0] ? shapes[0].id : 0); },
      lastMoveBefore(ms) { const i = bs(t, ms); return i ? mv[i - 1].t_ms : -1e9; },
    };
  };
})();

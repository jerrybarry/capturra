// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* stage.js — the preview canvas: background, rounded frame, zoomed screen content, cursor, click ripples.
 *
 * Preview only. The exported video is rendered by Rust from the same settings; this file mirrors
 * its camera math (zoom.js) so what you see is what you get within small rounding differences.
 *
 *   const stage = C.Stage(containerEl);
 *   stage.setProject(P);     // P = {display, track, segments, settings, source, cursors, ...}
 *   stage.render(tMs);       // draw one frame
 *   stage.pick(cb)           // next click returns {x,y} in display points (renders unzoomed meanwhile)
 */
(function () {
  'use strict';
  const C = window.Capturra;

  const BACKGROUNDS = [
    ['#6c63ff', '#3a2fb8'], ['#ff7a59', '#c2306b'], ['#19b4a1', '#0a5d8f'], ['#2c3e66', '#0e1630'],
    ['#f6d365', '#fda085'], ['#e9e9ef', '#cfd2e2'],
  ];
  C.BACKGROUNDS = BACKGROUNDS;
  // Bundled wallpapers (files in public/backgrounds, embedded in the exporter under the same ids).
  C.WALLPAPERS = [['aurora', 'Aurora'], ['sunset', 'Sunset'], ['lagoon', 'Lagoon'], ['orchid', 'Orchid'], ['ember', 'Ember'], ['meadow', 'Meadow'], ['midnight', 'Midnight'], ['dune', 'Dune']];
  C.FLAT_COLORS = ['#13151a', '#0e1630', '#1f6feb', '#19b4a1', '#e8e9ec', '#f6d365', '#ff7a59', '#c2306b'];
  C.customBgs = []; // [{ id, path }] from list_custom_backgrounds, newest first
  C.backgroundCss = (s) => {
    const url = (u) => `center / cover no-repeat url("${u}")`;
    if (s.type === 'color') return s.color || '#0e1630';
    if (s.type === 'wallpaper' && C.WALLPAPERS.some((w) => w[0] === s.wallpaper)) return url('/backgrounds/' + s.wallpaper + '.png');
    if (s.type === 'custom') {
      const c = C.customBgs.find((x) => x.id === s.image);
      if (c) return url(C.fileUrl(c.path));
    }
    const bg = s.bg >= 0 && BACKGROUNDS[s.bg] ? BACKGROUNDS[s.bg] : [s.c1, s.c2];
    return `linear-gradient(135deg, ${bg[0]}, ${bg[1]})`;
  };
  const ASPECTS = { '16:9': 16 / 9, '4:3': 4 / 3, '1:1': 1, '9:16': 9 / 16, source: 0 };
  C.ASPECTS = ASPECTS;

  C.Stage = function (host) {
    host.innerHTML = `<div class="artboard"><div class="frame"><canvas class="content"></canvas><canvas class="overlay"></canvas></div></div>
      <div class="badge"></div><div class="pick-hint" hidden>Click where the zoom should centre · Esc to cancel</div>
      <video class="vid-hidden" playsinline preload="auto"></video>`;
    const art = C.$('.artboard', host), frame = C.$('.frame', host);
    const cc = C.$('canvas.content', host), oc = C.$('canvas.overlay', host);
    const cx = cc.getContext('2d'), ox = oc.getContext('2d');
    const video = C.$('video', host), badge = C.$('.badge', host), hint = C.$('.pick-hint', host);
    let P = null, last = 0, picking = null, fw = 1, fh = 1, dpr = 1;
    const sprites = {}; // cursor id -> {img, ok}

    const api = {
      video, el: host,
      setProject(p) {
        P = p; last = 0;
        for (const k in sprites) delete sprites[k];
        if (p && p.source && p.source.url) { video.src = p.source.url; video.load(); } else { video.removeAttribute('src'); video.load(); }
        api.layout();
      },
      layout() {
        if (!P) return;
        const s = P.settings.frame;
        const hr = host.getBoundingClientRect();
        const availW = Math.max(100, hr.width - 56), availH = Math.max(80, hr.height - 56);
        const d = P.display;
        const ar = s.aspect === 'source' || !ASPECTS[s.aspect] ? d.width / d.height : ASPECTS[s.aspect];
        let aw = availW, ah = aw / ar;
        if (ah > availH) { ah = availH; aw = ah * ar; }
        art.style.width = aw + 'px'; art.style.height = ah + 'px';
        art.style.background = C.backgroundCss(s);
        const pad = (s.padding / 100) * Math.min(aw, ah);
        let bw = aw - pad * 2, bh = bw * (d.height / d.width);
        if (bh > ah - pad * 2) { bh = ah - pad * 2; bw = bh * (d.width / d.height); }
        fw = Math.max(40, bw); fh = Math.max(30, bh);
        Object.assign(frame.style, {
          left: (aw - fw) / 2 + 'px', top: (ah - fh) / 2 + 'px', width: fw + 'px', height: fh + 'px',
          // the corner radius and the shadow are sized for a 900 px wide picture and scale with it, exactly as the export
          // does (`PREVIEW_WIDTH` in capturra-render's frame.rs), so the preview shows the same proportions at any window size
          borderRadius: s.radius * (fw / 900) + 'px', boxShadow: `0 ${(6 + s.shadow * 22) * (fw / 900)}px ${(14 + s.shadow * 50) * (fw / 900)}px rgba(0,0,0,${0.12 + s.shadow * 0.5})`,
        });
        dpr = Math.min(2, window.devicePixelRatio || 1);
        for (const c of [cc, oc]) { c.width = Math.round(fw * dpr); c.height = Math.round(fh * dpr); }
        api.render(last);
      },
      pick(cb) {
        picking = cb; frame.classList.add('pick'); hint.hidden = false; api.render(last);
        const done = (pt) => { picking = null; frame.classList.remove('pick'); hint.hidden = true; frame.removeEventListener('pointerdown', onDown); document.removeEventListener('keydown', onKey, true); api.render(last); if (pt) cb(pt); else cb(null); };
        const onDown = (e) => { const r = frame.getBoundingClientRect(); done({ x: ((e.clientX - r.left) / r.width) * P.display.width, y: ((e.clientY - r.top) / r.height) * P.display.height }); };
        const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); done(null); } };
        frame.addEventListener('pointerdown', onDown); document.addEventListener('keydown', onKey, true);
      },
      get picking() { return !!picking; },
      setBadge(chips) { badge.innerHTML = chips.map((c) => `<span class="chip">${C.esc(c)}</span>`).join(''); },
      render(t) {
        last = t;
        if (!P) return;
        const d = P.display, s = P.settings;
        const cam = picking || s.zoom.auto === false ? { zoom: 1, cx: 0.5, cy: 0.5 } : C.cameraAt(P.segments, t, d, P.pan);
        const z = cam.zoom, vw = d.width / z, vh = d.height / z;
        const vx = cam.cx * d.width - vw / 2, vy = cam.cy * d.height - vh / 2;
        const k = fw / vw; // frame px per display point at this zoom

        // screen content
        cx.setTransform(1, 0, 0, 1, 0, 0);
        cx.clearRect(0, 0, cc.width, cc.height);
        if (P.source && P.source.kind === 'video' && video.readyState >= 2) {
          const sx = (video.videoWidth / d.width);
          cx.imageSmoothingQuality = 'high';
          cx.drawImage(video, vx * sx, vy * sx, vw * sx, vh * sx, 0, 0, cc.width, cc.height);
        } else if (P.source && P.source.kind === 'demo') {
          const m = k * dpr;
          cx.setTransform(m, 0, 0, m, -vx * m, -vy * m);
          C.demo.draw(cx, t);
          cx.setTransform(1, 0, 0, 1, 0, 0);
        } else {
          cx.fillStyle = '#111'; cx.fillRect(0, 0, cc.width, cc.height);
          cx.fillStyle = '#888'; cx.font = `${14 * dpr}px sans-serif`; cx.textAlign = 'center';
          cx.fillText('Loading video…', cc.width / 2, cc.height / 2);
        }

        // cursor + ripples, drawn unzoomed in frame space so they stay crisp
        ox.setTransform(dpr, 0, 0, dpr, 0, 0);
        ox.clearRect(0, 0, fw, fh);
        const cs = s.cursor;
        const toF = (x, y) => [(x - vx) * k, (y - vy) * k];
        if (cs.ripple) {
          for (const dn of P.track.downs) {
            const age = t - dn.t_ms;
            if (age < 0 || age > 520) continue;
            const u = age / 520, [rx, ry] = toF(dn.x, dn.y);
            ox.beginPath(); ox.arc(rx, ry, (10 + 34 * u) * (fw / 900 + 0.6), 0, 7);
            ox.lineWidth = 3 * (1 - u) + 0.5; ox.strokeStyle = `rgba(90,110,255,${0.75 * (1 - u)})`; ox.stroke();
          }
        }
        if (cs.show) {
          const p = P.track.at(t, cs.smoothing);
          if (p) {
            const [px, py] = toF(p.x, p.y);
            let down = false;
            for (const dn of P.track.downs) { if (dn.t_ms <= t && t < dn.t_ms + 140) { down = true; break; } }
            const scale = cs.size * (fw / 1100 + 0.55) * (1 + (z - 1) * 0.3) * (down && cs.squish ? 0.86 : 1);
            // the pointing hand over buttons and links, when the recording noted them and the user wants it
            const hand = cs.hand !== false && P.track.hasHover && P.cursors ? P.cursors.find((c) => c.role === 'pointing_hand') : null;
            drawCursor(ox, px, py, scale, hand && P.track.clickableAt(t) ? hand.id : P.track.shapeAt(t));
          }
        }
      },
    };

    function drawCursor(g, x, y, s, id) {
      const spr = P.cursors && P.cursors.find((c) => c.id === id && (c.file || c.sprite));
      if (spr) {
        let e = sprites[id];
        if (!e) {
          e = sprites[id] = { img: new Image(), ok: false };
          e.img.onload = () => { e.ok = true; api.render(last); };
          e.img.src = C.fileUrl((P.meta.dir || '') + '/' + (spr.file || spr.sprite));
        }
        if (e.ok) {
          const hot = spr.hotspot || [spr.hotspot_x || 0, spr.hotspot_y || 0];
          const w = (spr.width || spr.size && spr.size[0] || e.img.naturalWidth / (P.display.scale || 1)), h = (spr.height || spr.size && spr.size[1] || e.img.naturalHeight / (P.display.scale || 1));
          g.save(); g.shadowColor = 'rgba(0,0,0,.35)'; g.shadowBlur = 5 * s; g.shadowOffsetY = 2 * s;
          g.drawImage(e.img, x - hot[0] * s, y - hot[1] * s, w * s, h * s); g.restore();
          return;
        }
      }
      g.save(); g.translate(x, y); g.scale(s, s);
      g.shadowColor = 'rgba(0,0,0,.35)'; g.shadowBlur = 5; g.shadowOffsetY = 2;
      g.lineJoin = 'round'; g.lineWidth = 1.6; g.strokeStyle = '#fff'; g.fillStyle = '#111';
      if (id === 1) { // I-beam
        g.beginPath(); g.moveTo(-4, -10); g.lineTo(4, -10); g.moveTo(0, -10); g.lineTo(0, 10); g.moveTo(-4, 10); g.lineTo(4, 10);
        g.lineWidth = 4.2; g.strokeStyle = '#fff'; g.stroke(); g.lineWidth = 1.6; g.strokeStyle = '#111'; g.stroke();
      } else if (id === 2) { // pointing hand
        g.beginPath(); g.moveTo(-1, 14); g.lineTo(-7, 5); g.lineTo(-6, 3); g.lineTo(-3, 5); g.lineTo(-3, -9); g.lineTo(0, -9); g.lineTo(0, -1); g.lineTo(9, 1); g.lineTo(9, 14); g.closePath();
        g.fillStyle = '#fff'; g.fill(); g.strokeStyle = '#111'; g.lineWidth = 1.5; g.stroke();
      } else { // arrow
        g.beginPath(); g.moveTo(0, 0); g.lineTo(0, 17); g.lineTo(4.2, 13.2); g.lineTo(7.2, 20); g.lineTo(10, 18.7); g.lineTo(7.2, 12.3); g.lineTo(12.6, 12.3); g.closePath();
        g.stroke(); g.fill();
      }
      g.restore();
    }

    new ResizeObserver(() => api.layout()).observe(host);
    video.addEventListener('seeked', () => api.render(last));
    video.addEventListener('loadeddata', () => api.render(last));
    return api;
  };
})();

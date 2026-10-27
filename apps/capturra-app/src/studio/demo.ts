// @ts-nocheck — ported unchanged from the original UI guide; the typed modules in this folder are newer.
/* demo.js — a built-in "recording": a fake SaaS dashboard drawn on a canvas, driven by scripted
 * mouse and keyboard events in the same shape as events.json (version 2). It lets the library and
 * editor run, and be designed, with no real recording. Everything here is deterministic in t, so
 * scrubbing the timeline is exact. Nothing in this file is used for real recordings.
 */
(function () {
  'use strict';
  const C = window.Capturra;

  const W = 1920, H = 1080, DURATION = 18000;
  const display = { width: W, height: H, scale: 1 };

  /* ------------------------------ scripted events ------------------------------ */
  const NAV = [['Overview', 170], ['Customers', 234], ['Pricing', 298], ['Reports', 362], ['Settings', 426]];
  const SEARCH = { x: 560, y: 30, w: 480, h: 52 };
  const NEWBTN = { x: 1580, y: 28, w: 240, h: 56 };
  const TOGGLE = { x: 300, y: 156, w: 56, h: 32 };
  const MODAL = { x: 620, y: 300, w: 680, h: 490 };
  const MINPUT = { x: 660, y: 440, w: 600, h: 60 };
  const MSAVE = { x: 1120, y: 700, w: 140, h: 52 };
  const MCANCEL = { x: 980, y: 700, w: 120, h: 52 };
  const inRect = (r, x, y) => x >= r.x && x <= r.x + r.w && y >= r.y && y <= r.y + r.h;

  const modalOpen = (t) => t >= 11100 && t < 15200;

  function kindAt(x, y, t) {
    if (modalOpen(t)) return inRect(MINPUT, x, y) ? 1 : inRect(MSAVE, x, y) || inRect(MCANCEL, x, y) ? 2 : 0;
    if (inRect(SEARCH, x, y)) return 1;
    if (inRect(NEWBTN, x, y) || inRect(TOGGLE, x, y)) return 2;
    if (x < 260 && NAV.some(([, cy]) => Math.abs(y - cy) < 26)) return 2;
    return 0;
  }

  function buildEvents() {
    const ev = [];
    const ease = C.ease;
    let cur = { x: 960, y: 540 };
    const at = (t, x, y) => { cur = { x, y }; ev.push({ type: 'move', t_ms: Math.round(t), x, y }); };
    const glide = (t0, t1, x, y) => {
      const a = cur, dx = x - a.x, dy = y - a.y, dist = Math.hypot(dx, dy) || 1;
      for (let t = t0; t <= t1; t += 8) {
        const u = ease((t - t0) / (t1 - t0));
        const arc = Math.sin(Math.PI * u) * 0.05 * dist;
        ev.push({ type: 'move', t_ms: Math.round(t), x: +(a.x + dx * u - (dy / dist) * arc).toFixed(2), y: +(a.y + dy * u + (dx / dist) * arc).toFixed(2) });
      }
      cur = { x, y };
      ev.push({ type: 'move', t_ms: Math.round(t1), x, y });
    };
    const click = (t) => { ev.push({ type: 'down', t_ms: t, x: cur.x, y: cur.y, button: 'left' }); ev.push({ type: 'up', t_ms: t + 110, x: cur.x, y: cur.y, button: 'left' }); };
    const keys = (t0, n, step) => { for (let i = 0; i < n; i++) ev.push({ type: 'key', t_ms: t0 + i * step }); };

    at(300, 960, 540);
    glide(600, 1800, 140, 234); click(2000);
    glide(2600, 4000, 328, 172); click(4200);
    glide(4800, 6400, 800, 56); click(6600);
    keys(7100, 4, 200);
    glide(9600, 10900, 1700, 56); click(11000);
    glide(11400, 12100, 960, 470); click(12200);
    keys(12500, 9, 100);
    glide(13700, 14500, 1190, 726); click(14600);
    glide(15400, 16600, 1400, 520);
    at(17950, 1400, 520);

    // cursor shape changes follow what is under the pointer
    let last = -1;
    const tr = C.makeCursorTrack(ev);
    for (let t = 0; t <= DURATION; t += 30) {
      const p = tr.at(t, 0);
      const k = p ? kindAt(p.x, p.y, t) : 0;
      if (k !== last) { ev.push({ type: 'cursor_change', t_ms: t, id: k }); last = k; }
    }
    return ev;
  }
  const events = buildEvents();
  const track = C.makeCursorTrack(events);

  /* ------------------------------ drawing ------------------------------ */
  const FONT = '-apple-system, "SF Pro Text", "Segoe UI", system-ui, sans-serif';
  const ROWS = [
    ['Acme Inc', 'Business', '$2,400', 'Active'], ['Brightwave', 'Starter', '$480', 'Active'], ['Cobalt Labs', 'Business', '$3,150', 'Active'],
    ['Dunmore & Co', 'Team', '$1,260', 'Trial'], ['Evergreen Foods', 'Business', '$2,900', 'Active'], ['Fathom Studio', 'Starter', '$320', 'Past due'],
    ['Granite Works', 'Team', '$1,740', 'Active'], ['Halcyon Health', 'Business', '$4,020', 'Active'],
  ];
  const ARCHIVED = ['Zenith Corp', 'Team', '$0', 'Archived'];
  const AVATAR = ['#5a5af2', '#e5622d', '#1a9e6d', '#c43d8f', '#2a7de1', '#c98a1b', '#7a56d6', '#d1405a'];

  function rr(ctx, x, y, w, h, r) {
    ctx.beginPath();
    if (ctx.roundRect) { ctx.roundRect(x, y, w, h, r); return; }
    r = Math.min(r, w / 2, h / 2);
    ctx.moveTo(x + r, y); ctx.arcTo(x + w, y, x + w, y + h, r); ctx.arcTo(x + w, y + h, x, y + h, r);
    ctx.arcTo(x, y + h, x, y, r); ctx.arcTo(x, y, x + w, y, r); ctx.closePath();
  }
  function text(ctx, s, x, y, size, weight, color, align) {
    ctx.font = `${weight || 400} ${size}px ${FONT}`;
    ctx.fillStyle = color || '#1b1d2e';
    ctx.textAlign = align || 'left';
    ctx.textBaseline = 'middle';
    ctx.fillText(s, x, y);
  }
  const countKeys = (a, b, t) => events.filter((e) => e.type === 'key' && e.t_ms >= a && e.t_ms < b && e.t_ms <= t).length;

  /** Draw the screen at time t into ctx, in 1920x1080 coordinates (apply any camera transform first). */
  function draw(ctx, t) {
    const cp = track.at(t, 0) || { x: -999, y: -999 };
    ctx.save();
    ctx.fillStyle = '#f4f5f9'; ctx.fillRect(0, 0, W, H);

    // sidebar
    ctx.fillStyle = '#12162b'; ctx.fillRect(0, 0, 260, H);
    rr(ctx, 32, 40, 40, 40, 12); ctx.fillStyle = '#7676ff'; ctx.fill();
    ctx.beginPath(); ctx.arc(52, 60, 9, 0, 7); ctx.fillStyle = '#fff'; ctx.fill();
    text(ctx, 'Lumen', 90, 61, 28, 700, '#fff');
    const section = t >= 2080 ? 'Customers' : 'Overview';
    NAV.forEach(([name, cy]) => {
      const sel = name === section, hov = cp.x < 260 && Math.abs(cp.y - cy) < 26;
      if (sel || hov) { rr(ctx, 20, cy - 26, 220, 52, 12); ctx.fillStyle = sel ? 'rgba(118,118,255,.28)' : 'rgba(255,255,255,.07)'; ctx.fill(); }
      ctx.beginPath(); ctx.arc(50, cy, 8, 0, 7); ctx.fillStyle = sel ? '#a9a9ff' : '#5b6086'; ctx.fill();
      text(ctx, name, 80, cy + 1, 22, sel ? 600 : 500, sel ? '#fff' : '#a3a8c8');
    });
    rr(ctx, 20, H - 100, 220, 64, 14); ctx.fillStyle = 'rgba(255,255,255,.06)'; ctx.fill();
    ctx.beginPath(); ctx.arc(54, H - 68, 18, 0, 7); ctx.fillStyle = '#e5622d'; ctx.fill();
    text(ctx, 'Maya Reyes', 84, H - 76, 19, 600, '#fff'); text(ctx, 'Admin', 84, H - 54, 16, 400, '#8a90b4');

    // header
    ctx.fillStyle = '#fff'; ctx.fillRect(260, 0, W - 260, 112);
    ctx.fillStyle = '#e6e8f0'; ctx.fillRect(260, 111, W - 260, 1);
    text(ctx, section, 300, 56, 34, 700, '#1b1d2e');
    const sfocus = t >= 6700 && t < 9200;
    rr(ctx, SEARCH.x, SEARCH.y, SEARCH.w, SEARCH.h, 12);
    ctx.fillStyle = '#f4f5f9'; ctx.fill(); ctx.lineWidth = sfocus ? 3 : 1.5; ctx.strokeStyle = sfocus ? '#5a5af2' : '#dfe1ea'; ctx.stroke();
    const typed = 'acme'.slice(0, countKeys(7100, 7900, t));
    if (typed) text(ctx, typed, SEARCH.x + 20, SEARCH.y + 27, 24, 400, '#1b1d2e');
    else text(ctx, 'Search customers…', SEARCH.x + 20, SEARCH.y + 27, 24, 400, '#9aa0b8');
    if (sfocus && Math.floor(t / 500) % 2 === 0) { ctx.fillStyle = '#5a5af2'; ctx.fillRect(SEARCH.x + 22 + (typed ? typed.length * 13.2 : 0), SEARCH.y + 13, 2, 28); }
    const nh = inRect(NEWBTN, cp.x, cp.y) && !modalOpen(t);
    rr(ctx, NEWBTN.x, NEWBTN.y, NEWBTN.w, NEWBTN.h, 14); ctx.fillStyle = nh ? '#4a4ae0' : '#5a5af2'; ctx.fill();
    text(ctx, '+  New customer', NEWBTN.x + NEWBTN.w / 2, NEWBTN.y + 29, 22, 600, '#fff', 'center');

    if (section === 'Overview') {
      [['Monthly revenue', '$48,210', '+12.4%'], ['Active customers', '1,284', '+3.1%'], ['Churn', '1.8%', '−0.3%']].forEach(([l, v, d], i) => {
        const x = 300 + i * 520;
        rr(ctx, x, 150, 490, 190, 20); ctx.fillStyle = '#fff'; ctx.fill(); ctx.strokeStyle = '#e6e8f0'; ctx.lineWidth = 1.5; ctx.stroke();
        text(ctx, l, x + 32, 196, 22, 500, '#6b7192'); text(ctx, v, x + 32, 258, 52, 700, '#1b1d2e'); text(ctx, d, x + 32, 308, 22, 600, '#1a9e6d');
      });
      rr(ctx, 300, 370, 1560, 640, 20); ctx.fillStyle = '#fff'; ctx.fill(); ctx.strokeStyle = '#e6e8f0'; ctx.stroke();
      text(ctx, 'Revenue, last 12 months', 336, 422, 26, 600, '#1b1d2e');
      for (let i = 0; i < 12; i++) {
        const h = 120 + ((i * 53) % 7) * 34 + i * 14, x = 360 + i * 124;
        rr(ctx, x, 940 - h, 68, h, 10); ctx.fillStyle = i === 11 ? '#5a5af2' : '#d9dcf8'; ctx.fill();
        text(ctx, ['J', 'F', 'M', 'A', 'M', 'J', 'J', 'A', 'S', 'O', 'N', 'D'][i], x + 34, 975, 18, 500, '#8a90b4', 'center');
      }
    } else {
      // filter row
      const on = t >= 4250;
      rr(ctx, TOGGLE.x, TOGGLE.y, TOGGLE.w, TOGGLE.h, 16); ctx.fillStyle = on ? '#5a5af2' : '#cfd2e2'; ctx.fill();
      ctx.beginPath(); ctx.arc(TOGGLE.x + (on ? 40 : 16), TOGGLE.y + 16, 12, 0, 7); ctx.fillStyle = '#fff'; ctx.fill();
      text(ctx, 'Show archived', 376, 173, 22, 500, '#3d4260');
      text(ctx, 'Sorted by revenue', 1860, 173, 20, 500, '#8a90b4', 'right');

      // table
      rr(ctx, 300, 220, 1560, 790, 20); ctx.fillStyle = '#fff'; ctx.fill(); ctx.strokeStyle = '#e6e8f0'; ctx.lineWidth = 1.5; ctx.stroke();
      ['Customer', 'Plan', 'MRR', 'Status'].forEach((h, i) => text(ctx, h.toUpperCase(), [350, 900, 1240, 1500][i], 262, 16, 700, '#8a90b4'));
      ctx.fillStyle = '#eceef5'; ctx.fillRect(300, 290, 1560, 1.5);
      let rows = ROWS.slice();
      if (t >= 15200) rows.unshift(['Acme West', 'Team', '$0', 'Trial']);
      if (on) rows.push(ARCHIVED);
      if (t >= 7900) rows = rows.filter((r) => r[0].toLowerCase().includes('acme'));
      rows.slice(0, 8).forEach((r, i) => {
        const y = 290 + i * 82, fresh = r[0] === 'Acme West';
        if (fresh) { ctx.fillStyle = 'rgba(90,90,242,.07)'; ctx.fillRect(301, y, 1558, 82); }
        else if (!modalOpen(t) && cp.x > 300 && cp.x < 1860 && cp.y > y && cp.y < y + 82) { ctx.fillStyle = '#f7f8fc'; ctx.fillRect(301, y, 1558, 82); }
        ctx.beginPath(); ctx.arc(376, y + 41, 22, 0, 7); ctx.fillStyle = AVATAR[(r[0].charCodeAt(0) + r[0].length) % AVATAR.length]; ctx.fill();
        text(ctx, r[0][0], 376, y + 42, 22, 700, '#fff', 'center');
        text(ctx, r[0], 416, y + 41, 24, 600, r[3] === 'Archived' ? '#9aa0b8' : '#1b1d2e');
        text(ctx, r[1], 900, y + 41, 22, 500, '#3d4260'); text(ctx, r[2], 1240, y + 41, 22, 600, '#1b1d2e');
        const col = { Active: ['#e1f5ec', '#17855a'], Trial: ['#e6e8ff', '#4a4ae0'], 'Past due': ['#fdeaea', '#c0392b'], Archived: ['#eceef5', '#6b7192'] }[r[3]];
        rr(ctx, 1500, y + 22, r[3].length * 12 + 36, 38, 19); ctx.fillStyle = col[0]; ctx.fill();
        text(ctx, r[3], 1518, y + 42, 18, 600, col[1]);
        ctx.fillStyle = '#eceef5'; ctx.fillRect(300, y + 81, 1560, 1.5);
      });
    }

    // modal
    if (modalOpen(t)) {
      const k = Math.min(1, (t - 11100) / 220);
      ctx.fillStyle = `rgba(14,16,36,${0.46 * k})`; ctx.fillRect(0, 0, W, H);
      ctx.save(); ctx.globalAlpha = k; ctx.translate(0, (1 - k) * 18);
      ctx.shadowColor = 'rgba(0,0,0,.3)'; ctx.shadowBlur = 60; ctx.shadowOffsetY = 20;
      rr(ctx, MODAL.x, MODAL.y, MODAL.w, MODAL.h, 24); ctx.fillStyle = '#fff'; ctx.fill(); ctx.shadowColor = 'transparent';
      text(ctx, 'New customer', 660, 355, 32, 700, '#1b1d2e');
      text(ctx, 'Company name', 660, 412, 20, 500, '#6b7192');
      const foc = t >= 12300;
      rr(ctx, MINPUT.x, MINPUT.y, MINPUT.w, MINPUT.h, 14); ctx.fillStyle = '#f4f5f9'; ctx.fill(); ctx.lineWidth = foc ? 3 : 1.5; ctx.strokeStyle = foc ? '#5a5af2' : '#dfe1ea'; ctx.stroke();
      const nm = 'Acme West'.slice(0, countKeys(12500, 13600, t));
      text(ctx, nm || 'e.g. Northwind', 684, 471, 26, 400, nm ? '#1b1d2e' : '#9aa0b8');
      if (foc && Math.floor(t / 500) % 2 === 0) { ctx.fillStyle = '#5a5af2'; ctx.fillRect(686 + nm.length * 14.2, 454, 2, 34); }
      text(ctx, 'Plan', 660, 556, 20, 500, '#6b7192');
      rr(ctx, 660, 580, 600, 60, 14); ctx.fillStyle = '#f4f5f9'; ctx.fill(); ctx.lineWidth = 1.5; ctx.strokeStyle = '#dfe1ea'; ctx.stroke();
      text(ctx, 'Team — $29 / seat', 684, 611, 24, 500, '#1b1d2e');
      const ch = inRect(MCANCEL, cp.x, cp.y), sh = inRect(MSAVE, cp.x, cp.y);
      rr(ctx, MCANCEL.x, MCANCEL.y, MCANCEL.w, MCANCEL.h, 14); ctx.fillStyle = ch ? '#e6e8f0' : '#eef0f6'; ctx.fill();
      text(ctx, 'Cancel', MCANCEL.x + 60, MCANCEL.y + 27, 21, 600, '#3d4260', 'center');
      rr(ctx, MSAVE.x, MSAVE.y, MSAVE.w, MSAVE.h, 14); ctx.fillStyle = sh ? '#4a4ae0' : '#5a5af2'; ctx.fill();
      text(ctx, 'Save', MSAVE.x + 70, MSAVE.y + 27, 21, 600, '#fff', 'center');
      ctx.restore();
    }

    // toast
    if (t >= 15200 && t < 17000) {
      const k = Math.min(1, (t - 15200) / 260), out = Math.min(1, Math.max(0, (17000 - t) / 260));
      ctx.save(); ctx.globalAlpha = Math.min(k, out); ctx.translate(0, (1 - k) * 24);
      ctx.shadowColor = 'rgba(0,0,0,.25)'; ctx.shadowBlur = 30; ctx.shadowOffsetY = 10;
      rr(ctx, 1520, 960, 340, 70, 18); ctx.fillStyle = '#1b1d2e'; ctx.fill(); ctx.shadowColor = 'transparent';
      ctx.beginPath(); ctx.arc(1560, 995, 14, 0, 7); ctx.fillStyle = '#3fcf7e'; ctx.fill();
      text(ctx, 'Customer added', 1590, 996, 22, 600, '#fff');
      ctx.restore();
    }
    ctx.restore();
  }

  /* ------------------------------ projects ------------------------------ */
  function projectMeta(p) {
    const dir = p.dir || '~/Movies/Capturra/' + p.title;
    return {
      id: p.id, title: p.title, created_at: p.created_at, duration_ms: DURATION, width: W, height: H,
      dir, video: dir + '/recording.mp4', events: dir + '/events.json', share_url: p.share_url || null,
      thumb_at: p.thumb_at || 4000, kind: 'demo',
    };
  }
  function loadProject(meta) {
    return { ...meta, display: { ...display }, events: C.clone(events), cursors: [], segments: null, settings: null };
  }
  function thumb(canvas, t) {
    const ctx = canvas.getContext('2d');
    ctx.save(); ctx.scale(canvas.width / W, canvas.height / H);
    draw(ctx, t); ctx.restore();
  }

  C.demo = { display, duration: DURATION, events, track, draw, projectMeta, loadProject, thumb, kindAt };
})();

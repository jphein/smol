/* smol — project-site behaviour
   · WYSIWYG inline editing persisted to disk via the Python server
   · live Mission Control (tasks + agents) polling
   · an interactive 72×40 OLED block-digger mockup                       */
(() => {
  'use strict';
  const $  = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => [...r.querySelectorAll(s)];
  const esc = s => String(s).replace(/[&<>"]/g, c => ({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;' }[c]));

  /* ============================ content load / save ============================ */
  const editables = () => $$('[data-edit]');
  const saveBtn = $('#saveBtn');
  let dirty = false;
  const setDirty = v => { dirty = v; saveBtn.disabled = !v; };

  async function loadContent() {
    try {
      const r = await fetch('content.json', { cache: 'no-store' });
      if (!r.ok) return;
      const f = ((await r.json()) || {}).fields || {};
      editables().forEach(el => {
        const k = el.getAttribute('data-edit');
        if (typeof f[k] === 'string') el.innerHTML = f[k];
      });
    } catch { /* opened as a file / server down: keep the HTML defaults */ }
  }

  const collect = () => {
    const f = {};
    editables().forEach(el => (f[el.getAttribute('data-edit')] = el.innerHTML.trim()));
    return f;
  };

  async function save() {
    if (!dirty) return;
    try {
      const r = await fetch('/save', {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ fields: collect() }),
      });
      const j = await r.json();
      if (j.ok) { setDirty(false); toast(`saved ${j.saved} blocks ✓`); }
      else toast('save failed: ' + (j.error || '?'), true);
    } catch { toast('save failed — is the server running?', true); }
  }

  /* ============================ edit mode + toolbar ============================ */
  const body = document.body, toggle = $('#editToggle');
  const isEditing = () => body.classList.contains('editing');
  function setEdit(on) {
    body.classList.toggle('editing', on);
    toggle.classList.toggle('on', on);
    $('#editLbl').textContent = on ? 'Editing' : 'Edit';
    editables().forEach(el => (el.contentEditable = on ? 'true' : 'false'));
    if (!on && dirty) save();
  }
  toggle.addEventListener('click', () => setEdit(!isEditing()));

  $$('.tb').forEach(btn => {
    btn.addEventListener('mousedown', e => e.preventDefault()); // keep the text selection
    btn.addEventListener('click', () => {
      const [cmd, arg] = btn.getAttribute('data-cmd').split(':');
      document.execCommand(cmd, false, arg || null);
      if (isEditing()) setDirty(true);
    });
  });

  document.addEventListener('input', e => { if (isEditing() && e.target.closest('[data-edit]')) setDirty(true); });
  document.addEventListener('focusout', e => {
    if (isEditing() && e.target.closest('[data-edit]') && dirty) setTimeout(() => dirty && save(), 150);
  });
  saveBtn.addEventListener('click', save);
  document.addEventListener('keydown', e => {
    const meta = e.metaKey || e.ctrlKey;
    if (meta && e.key.toLowerCase() === 's') { e.preventDefault(); if (isEditing()) save(); }
    if (meta && e.key.toLowerCase() === 'e') { e.preventDefault(); setEdit(!isEditing()); }
  });

  let toastT;
  function toast(msg, err) {
    const t = $('#toast');
    t.textContent = msg; t.classList.toggle('err', !!err); t.classList.add('show');
    clearTimeout(toastT); toastT = setTimeout(() => t.classList.remove('show'), 2600);
  }

  /* ============================ mission control ============================ */
  const live = $('#live'), liveTxt = $('#liveTxt');
  const ledOf = s => (['done', 'active', 'failed', 'pending'].includes(s) ? s : 'pending');

  async function pollTasks() {
    try {
      const d = await (await fetch('tasks.json', { cache: 'no-store' })).json();
      // NB: this reports the FEED being reachable, not the data being fresh — tasks.json is an
      // archived snapshot. Saying 'live' here made a 3-week-old ledger look current.
      live.classList.add('on'); liveTxt.textContent = 'feed ok';
      $('#taskList').innerHTML = d.tasks.map(t => `
        <div class="trow ${esc(t.status)}">
          <span class="tled ${ledOf(t.status)}"></span>
          <span class="ttl">${esc(t.title)}</span>
          <span class="town">${esc(t.owner || '')}</span>
        </div>`).join('');
      const done = d.tasks.filter(t => t.status === 'done').length;
      $('#taskCnt').textContent = `${done}/${d.tasks.length}`;
      $('#taskUpd').textContent = 'updated ' + (d.updated || '');
    } catch { live.classList.remove('on'); liveTxt.textContent = 'feed offline'; }
  }
  async function pollAgents() {
    try {
      const d = await (await fetch('agents.json', { cache: 'no-store' })).json();
      $('#agentList').innerHTML = d.agents.map(a => `
        <div class="agent">
          <span class="aglyph">${esc(a.glyph || '▚')}</span>
          <div style="flex:1">
            <div class="aname">${a.link ? `<a href="${esc(a.link)}" target="_blank" style="color:var(--glow)">${esc(a.name)}</a>` : esc(a.name)}</div>
            <div class="arole">${esc(a.role)}</div>
          </div>
          <span class="astat ${ledOf(a.status)}">${esc(a.status)}</span>
        </div>`).join('');
      const active = d.agents.filter(a => a.status === 'active').length;
      $('#agentCnt').textContent = active ? `${active} active` : `${d.agents.length}`;
      $('#agentUpd').textContent = 'updated ' + (d.updated || '');
    } catch { /* ignore */ }
  }
  const poll = () => { pollTasks(); pollAgents(); };

  /* ============================ scroll reveal + clock ============================ */
  const io = new IntersectionObserver(es => es.forEach(e => {
    if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
  }), { threshold: 0.14 });
  $$('.reveal').forEach(el => io.observe(el));

  const clock = () => { const c = $('#clock'); if (c) c.textContent = new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); };

  /* ================================ OLED block-digger ================================
     Rendered at the panel's REAL resolution: 72x40 device pixels, 1-bit, then scaled
     up by CSS with image-rendering:pixelated. It used to draw into a 576x320 canvas
     with an 8x coordinate space, which meant tile insets of 1-3 canvas px were EIGHTHS
     of a device pixel and the HUD used rgba(...,.5) -- a grey. Neither is producible by
     this part. At TP = 4 device px per tile the 18x10 world lands on 72x40 exactly.   */
  const cv = $('#oled');
  const panel = window.OLED.Panel(cv);
  const TP = 4, COLS = 18, ROWS = 10;                 // 18 x 4 = 72, 10 x 4 = 40
  const AIR = 0, DIRT = 1, STONE = 2, GRASS = 3;

  let world = [], P = { x: 0, y: 0, f: 1 }, inv = 0, lastInput = -9999;

  function genWorld() {
    const surf = Math.floor(ROWS / 2);
    world = [];
    for (let r = 0; r < ROWS; r++) {
      const row = [];
      for (let c = 0; c < COLS; c++)
        row.push(r < surf ? AIR : r === surf ? GRASS : r < surf + 2 ? DIRT : STONE);
      world.push(row);
    }
    P = { x: (COLS >> 1), y: surf - 1, f: 1 }; inv = 0;
  }
  const solid = (c, r) => (c < 0 || c >= COLS || r < 0 || r >= ROWS) ? true : world[r][c] !== AIR;

  function move(dc) {
    P.f = dc; const nx = P.x + dc;
    if (!solid(nx, P.y)) P.x = nx;
    else if (!solid(nx, P.y - 1) && !solid(P.x, P.y - 1)) { P.x = nx; P.y--; }
  }
  const jump = () => { if (!solid(P.x, P.y - 1)) P.y--; };
  const gravity = () => { if (!solid(P.x, P.y + 1)) P.y++; };
  function dig() {
    let tc = P.x + P.f, tr = P.y;
    if (!solid(tc, tr)) { tc = P.x; tr = P.y + 1; }
    if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] !== AIR) { world[tr][tc] = AIR; inv++; }
  }
  function place() {
    if (inv <= 0) return;
    let tc = P.x + P.f, tr = P.y;
    if (solid(tc, tr)) { tc = P.x; tr = P.y + 1; }
    if (tc >= 0 && tc < COLS && tr >= 0 && tr < ROWS && world[tr][tc] === AIR && !(tc === P.x && tr === P.y)) { world[tr][tc] = DIRT; inv--; }
  }

  const act = { up: jump, down: dig, left: () => move(-1), right: () => move(1), dig, place };
  function doAct(k, ts) { (act[k] || (() => {}))(); lastInput = ts ?? performance.now(); }

  // keyboard — only while hovering the device and not editing
  let hoverDevice = false;
  const dev = $('.device');
  if (dev) { dev.addEventListener('pointerenter', () => hoverDevice = true); dev.addEventListener('pointerleave', () => hoverDevice = false); }
  const KEYMAP = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down', a: 'dig', b: 'place', ' ': 'dig' };
  document.addEventListener('keydown', e => {
    if (isEditing() || !hoverDevice) return;
    const k = KEYMAP[e.key] || KEYMAP[e.key.toLowerCase()];
    if (k) { e.preventDefault(); doAct(k); }
  });
  // on-screen buttons
  $$('[data-k]').forEach(b => b.addEventListener('pointerdown', e => {
    e.preventDefault(); doAct(b.getAttribute('data-k'));
    b.classList.add('hit'); setTimeout(() => b.classList.remove('hit'), 130);
  }));

  // autopilot when idle
  function autopilot() {
    const r = Math.random();
    const blocked = solid(P.x + P.f, P.y) && solid(P.x + P.f, P.y - 1);
    if (blocked) { r < 0.6 ? dig() : (P.f *= -1); }
    else if (r < 0.12) P.f *= -1;
    else if (r < 0.26) dig();
    else if (r < 0.33 && !solid(P.x, P.y - 1) && Math.random() < 0.5) jump();
    else if (r < 0.40 && inv > 1) place();
    else move(P.f);
  }

  // render — every coordinate below is a DEVICE PIXEL on a 72x40 1-bit panel
  function draw(demo) {
    panel.clear();
    // With only 4x4 px per tile and no greys, SOLID fills merge into one bright slab
    // (the first pixel-true attempt did exactly that). So terrain is drawn as sparse
    // TEXTURE and only the player is solid — the classic 1-bit trick, and it makes the
    // player the brightest thing on the glass without needing a second colour.
    for (let r = 0; r < ROWS; r++) for (let c = 0; c < COLS; c++) {
      const t = world[r][c]; if (t === AIR) continue;
      const x = c * TP, y = r * TP;
      if (t === GRASS) {                     // surface: solid crust + tufts above it
        panel.hline(x, y + 1, TP);
        panel.px(x + 1, y); panel.px(x + 3, y);
      } else if (t === DIRT) {               // loose soil: two sparse grains
        panel.px(x + 1, y + 1); panel.px(x + 3, y + 2);
      } else {                               // stone: a 2x2 core, denser than soil
        panel.rect(x + 1, y + 1, 2, 2);
      }
    }
    panel.rect(P.x * TP, P.y * TP, TP, TP);                          // player: solid
    panel.px(P.x * TP + (P.f > 0 ? TP - 1 : 0), P.y * TP + 1, 0);    // facing "eye"

    // HUD — FONT_5X8, on a cleared band, exactly as firmware would have to do it
    const hud = String(inv);
    panel.rect(0, 0, 5 + hud.length * window.OLED.GLYPH_W, window.OLED.ROW_H, 0);
    panel.rect(1, 2, 2, 2);                                          // "a block" glyph
    panel.text(hud, 5, 0);
    if (demo) {
      const w = 4 * window.OLED.GLYPH_W;
      panel.rect(window.OLED.W - w, 0, w, window.OLED.ROW_H, 0);
      panel.text('auto', window.OLED.W - w, 0);
    }
    panel.flush();
  }

  let lastFall = 0, lastStep = 0;
  function loop(ts) {
    if (ts - lastFall > 220) { gravity(); lastFall = ts; }
    const demo = ts - lastInput > 3000;
    if (demo && ts - lastStep > 340) { autopilot(); lastStep = ts; }
    draw(demo);
    requestAnimationFrame(loop);
  }

  /* ============================== the bard (02) ==============================
     Types a REAL story onto a pixel-true 72×40 1-bit panel (see oled.js): the
     firmware's own FONT_5X8 glyphs, its 14-column wrap, its 5-row scroll.

     STORY is verbatim the deterministic (temp 0) output of the model blob
     committed at rust/clock/model/stories260K-q8.bin. Do NOT hand-edit it into
     something nicer — that would make the page a mock-up, which is the one thing
     this section claims it isn't. To change it, re-run:
       python3 tools/bard_reference.py rust/clock/model/stories260K-q8.bin \
         --temp 0 --steps 52 -i "Once upon a time, there was a little owl"
     and paste the output.                                                     */
  const BARD = {
    // Verbatim deterministic (temp 0) output of the committed blob. Regenerate with
    //   python3 tools/bard_reference.py rust/clock/model/stories260K-q8.bin \
    //     --temp 0 --steps 90 -i "Once upon a time, there was a little owl"
    // Greedy decoding is the only reproducible mode (the reference refuses temp > 0 —
    // sampling lives in the firmware's RNG), and a 260K model starts repeating itself
    // shortly after this, which is why the excerpt ends where it does.
    STORY: 'Once upon a time, there was a little owl named Jack. Jack loved to ' +
           'play with his toys. One day, Jack saw a big box in the ground. He wanted ' +
           'to play with it, but he was too small.',
    // The reveal rate is ms per CHARACTER, not per token: it is the firmware's own
    // CFG-`V` delivery setting, which DEFAULTS TO 160 (#302). An earlier version of
    // this file typed at 58 ms/char, derived from the 202 ms/token generation figure —
    // wrong axis. Generation and reveal are separate clocks; the panel is showing the
    // reveal one.
    MS_PER_CHAR: 160,
    HOLD_MS: 3400,
  };

  function bardTypewriter() {
    const cv = $('#bardOled');
    if (!cv || !window.OLED) return;
    const O = window.OLED, panel = O.Panel(cv);

    // Wrap ONCE with the firmware's own geometry (14 cols) — see oled.js. Wrapping
    // forward and keeping the tail is what textflow.rs does, so a line never
    // re-flows once it is on the glass; the text scrolls instead.
    const all = O.wrapTail(BARD.STORY, O.COLS, 1e9).lines;
    const total = all.reduce((n, l) => n + l.length + 1, 0);
    let at = 0, holding = 0;

    const render = () => {
      const shown = [];
      let seen = 0;
      for (const l of all) {
        if (seen >= at) break;
        shown.push(l.slice(0, Math.min(l.length, at - seen)));
        seen += l.length + 1;
      }
      const tail = shown.slice(Math.max(0, shown.length - O.ROWS));
      panel.clear().rows(tail);
      // the quill caret, one glyph cell, at the end of the last line
      if (tail.length) panel.caret(Math.min(tail[tail.length - 1].length, O.COLS - 1), tail.length - 1);
      panel.flush();
    };

    let last = 0;
    const tick = ts => {
      if (!last) last = ts;
      if (holding) {
        if (ts - holding > BARD.HOLD_MS) { at = 0; holding = 0; last = ts; render(); }
      } else if (ts - last >= BARD.MS_PER_CHAR) {
        last = ts; at += 1; render();
        if (at >= total) holding = ts;
      }
      requestAnimationFrame(tick);
    };
    render();
    requestAnimationFrame(tick);
  }

  /* ======================= world snake, on the actual glass =======================
     The section's big graphic is a DIAGRAM of the 256x256 world. This is the same game
     at the resolution the board really has: 18x10 cells of 4 px. It drifts and wraps,
     because "no walls, the world wraps" is a claim the page makes and a 72x40 panel can
     actually demonstrate.                                                              */
  function wsGlass() {
    const cv = $('#wsOled');
    if (!cv || !window.OLED) return;
    const O = window.OLED, panel = O.Panel(cv), C = 4;         // 4 px cells -> 18 x 10
    const GW = 18, GH = 10;
    let head = 6, len = 5, food = 13, t = 0;

    const cell = (cx, cy, on = 1) => panel.rect((cx % GW) * C, cy * C, C - 1, C - 1, on);

    // Laid out against BOTH grids at once, which is the fiddly part of a 72x40 panel:
    // 4 px cells give 18 x 10 cells, FONT_5X8 gives 14 x 5 text rows, and text row r
    // covers cell rows 2r and 2r+1. Everything below is placed so nothing collides and
    // nothing runs past column 14 -- the first attempt put a 6-char name at column 11,
    // which the panel clipped mid-glyph on top of the snake.
    function frame() {
      panel.clear();
      panel.text('you ' + (183 + len), 0, 0);            // text row 0  (cells 0-1)
      panel.text('Herald', 0, O.ROW_H);                   // text row 1  (cells 2-3)
      cell(1, 4); cell(2, 4);                             // its snake, cell row 4
      for (let i = 0; i < len; i++) cell((head - i + GW * 2) % GW, 6);   // you, row 6
      const fx = food * C, fy = 8 * C;                    // treasure, cell row 8
      panel.px(fx + 1, fy).px(fx, fy + 1).px(fx + 2, fy + 1).px(fx + 1, fy + 2);
      panel.flush();
    }

    frame();
    setInterval(() => {
      head = (head + 1) % GW;
      if (head === food) { len = Math.min(len + 1, 9); food = (food + 7) % GW; }
      t++;
      frame();
    }, 620);
  }

  /* ================================ share ================================
     One button, always visible: navigator.share where it exists, clipboard-copy
     as the desktop fallback. (Hiding it unless navigator.share exists means every
     desktop visitor sees nothing — the fallback is strictly better.) SHARE_URL is
     the deployed Pages URL and must stay in step with the canonical + og:url meta. */
  const SHARE_URL = 'https://jphein.github.io/smol/';
  const SHARE_TEXT = "A $3 ESP32-C3 writing children's stories on a 72×40 OLED — a real " +
    '260K-param transformer, no WiFi, no cloud. Plus a self-updating ESP-NOW mesh and a ' +
    'creature that hops between boards.';

  function initShare() {
    const btn = $('#native-share'), lbl = $('#native-share-label');
    if (!btn || !lbl) return;
    const restore = lbl.textContent;
    btn.addEventListener('click', () => {
      if (navigator.share) {
        navigator.share({ title: 'smol', text: SHARE_TEXT, url: SHARE_URL }).catch(() => {});
        return;
      }
      const done = ok => {
        lbl.textContent = ok ? 'Link copied ✓' : 'Copy failed — the URL is in the address bar';
        setTimeout(() => { lbl.textContent = restore; }, 2200);
      };
      // clipboard needs a secure context; degrade to a message rather than silence
      if (navigator.clipboard?.writeText) {
        navigator.clipboard.writeText(SHARE_URL).then(() => done(true), () => done(false));
      } else done(false);
    });
  }

  /* ================================ version ================================
     Reads the realm-sigil stamp from site/version.json, which is written at PUBLISH time
     by tools/stamp_site_version.py and gitignored — so there is no version in this repo to
     go stale, and nothing here to hand-update. Three honest outcomes and no fourth:

       · fetched        → the sigil word + short hash, linked to the commit
       · not there      → "version unknown" (a local run or file:// open; nothing was stamped)
       · fetch failed   → "version unavailable" — an unreachable capability ANNOUNCES itself
                          rather than silently vanishing, the same rule the share button and
                          the feed LED follow.

     `cache: 'no-store'` is load-bearing: a cached version.json is precisely the
     confidently-stale number this whole surface exists to avoid.

     Everything from the JSON goes in via textContent, and the href is accepted only if it is
     really a github.com URL — the file is generated from CI environment values, and a version
     stamp is not a good reason to hand the DOM an unchecked string. */
  async function initVersion() {
    const el = $('#site-version');
    if (!el) return;
    const say = (txt, title) => {
      el.textContent = txt;                     // textContent, never innerHTML
      if (title) el.title = title;
    };
    let v;
    try {
      const r = await fetch('version.json', { cache: 'no-store' });
      if (!r.ok) { say(r.status === 404 ? 'version unknown' : 'version unavailable'); return; }
      v = await r.json();
    } catch { say('version unavailable'); return; }

    const label = (v && (v.version || v.hash)) ? String(v.version || v.hash) : '';
    if (!label) { say('version unknown'); return; }

    const built = v.built ? ` · built ${String(v.built).replace('T', ' ').replace('Z', ' UTC')}` : '';
    const title = `branch ${v.branch || 'unknown'}${built}${v.dirty ? ' · working tree was dirty' : ''}`;
    const url = typeof v.commit_url === 'string' ? v.commit_url : '';
    el.className = 'ver known';
    if (/^https:\/\/github\.com\//.test(url)) {
      el.textContent = '';
      const a = document.createElement('a');
      a.href = url; a.target = '_blank'; a.rel = 'noopener';
      a.textContent = label;                    // textContent, never innerHTML
      el.appendChild(a);
      el.title = title;
    } else {
      say(label, title);
    }
  }

  /* ================================ boot ================================ */
  // On the published (GitHub Pages) copy or a file:// open there is no server
  // to POST edits to, so hide the editor dock and run read-only. Editing works
  // when served locally by server.py.
  const READ_ONLY = location.hostname.endsWith('github.io') || location.protocol === 'file:';
  if (READ_ONLY) {
    const dock = $('.dock'); if (dock) dock.style.display = 'none';
  }
  loadContent();
  bardTypewriter();
  initShare();
  initVersion();
  wsGlass();
  poll(); setInterval(poll, 4000);
  clock(); setInterval(clock, 15000);
  genWorld(); requestAnimationFrame(loop);
})();

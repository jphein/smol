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
      live.classList.add('on'); liveTxt.textContent = 'live';
      $('#taskList').innerHTML = d.tasks.map(t => `
        <div class="trow ${esc(t.status)}">
          <span class="tled ${ledOf(t.status)}"></span>
          <span class="ttl">${esc(t.title)}</span>
          <span class="town">${esc(t.owner || '')}</span>
        </div>`).join('');
      const done = d.tasks.filter(t => t.status === 'done').length;
      $('#taskCnt').textContent = `${done}/${d.tasks.length}`;
      $('#taskUpd').textContent = 'updated ' + (d.updated || '');
    } catch { live.classList.remove('on'); liveTxt.textContent = 'offline'; }
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

  /* ================================ OLED block-digger ================================ */
  const cv = $('#oled'), ctx = cv.getContext('2d');
  ctx.imageSmoothingEnabled = false;
  const TP = 32, COLS = cv.width / TP, ROWS = cv.height / TP;   // 18 × 10 tiles
  const BG = '#02100f', FG = '#7df9ff';
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

  // render
  function draw(demo) {
    ctx.fillStyle = BG; ctx.fillRect(0, 0, cv.width, cv.height);
    ctx.save(); ctx.shadowColor = FG; ctx.shadowBlur = 6; ctx.fillStyle = FG; ctx.strokeStyle = FG; ctx.lineWidth = 2;
    for (let r = 0; r < ROWS; r++) for (let c = 0; c < COLS; c++) {
      const t = world[r][c]; if (t === AIR) continue;
      const x = c * TP, y = r * TP;
      if (t === DIRT) ctx.strokeRect(x + 3, y + 3, TP - 6, TP - 6);
      else ctx.fillRect(x + 1, y + 1, TP - 2, TP - 2);
    }
    ctx.fillRect(P.x * TP + 1, P.y * TP + 1, TP - 2, TP - 2);        // player
    ctx.restore();
    // detail punches (no glow)
    ctx.fillStyle = BG;
    for (let r = 0; r < ROWS; r++) for (let c = 0; c < COLS; c++) {
      if (world[r][c] === GRASS) { ctx.fillRect(c * TP + 6, r * TP + 6, 4, 4); ctx.fillRect(c * TP + 18, r * TP + 12, 4, 4); }
      if (world[r][c] === STONE) { ctx.fillRect(c * TP + 10, r * TP + 8, 5, 5); }
    }
    ctx.fillRect(P.x * TP + (P.f > 0 ? TP - 12 : 6), P.y * TP + 8, 5, 5);  // player "eye"
    // HUD
    ctx.fillStyle = FG; ctx.shadowColor = FG; ctx.shadowBlur = 8;
    ctx.font = 'bold 20px "JetBrains Mono", monospace'; ctx.textBaseline = 'top';
    ctx.fillText('◆ ' + inv, 8, 6);
    ctx.shadowBlur = 0;
    if (demo) { ctx.font = '12px "JetBrains Mono", monospace'; ctx.fillStyle = 'rgba(125,249,255,.5)'; ctx.fillText('▸ auto', cv.width - 52, 8); }
  }

  let lastFall = 0, lastStep = 0;
  function loop(ts) {
    if (ts - lastFall > 220) { gravity(); lastFall = ts; }
    const demo = ts - lastInput > 3000;
    if (demo && ts - lastStep > 340) { autopilot(); lastStep = ts; }
    draw(demo);
    requestAnimationFrame(loop);
  }

  /* ================================ boot ================================ */
  loadContent();
  poll(); setInterval(poll, 4000);
  clock(); setInterval(clock, 15000);
  genWorld(); requestAnimationFrame(loop);
})();

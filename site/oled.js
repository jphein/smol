/* smol — a pixel-true 0.42" SSD1306 mock: 72x40, 1-bit, FONT_5X8.
   ==================================================================
   Every display on this page is the SAME physical part, so it gets ONE renderer.
   The panel's constraints are physical, not stylistic:

     * 72 x 40 PHYSICAL PIXELS, 1 bit each. Two states. No greys, no antialiasing,
       no sub-pixel positioning. A CSS text-shadow inside the glass is a lie.
     * FONT_5X8 -> COLS = 14 (14 x 5 = 70 of the 72 px; the 2 px of slack on the
       right is why text never touches the edge on real hardware) and
       ROWS = 5 (5 x 8 = 40 exactly). Constants mirrored from
       rust/clock/src/bard/mod.rs:341-345.

   So: draw into a 72x40 framebuffer, blit it 1:1 into a 72x40 canvas, and let CSS
   scale it up with `image-rendering: pixelated`. Glow belongs OUTSIDE the glass, as
   a filter on the scaled element -- never as soft pixels inside it.

   FONT: the glyph bitmaps below are the EXACT font the firmware renders with --
   embedded-graphics 0.8.2 `FONT_5X8`, extracted from its
   fonts/raw/ascii/font_5x8.raw (80x48, 1-bit, 16 glyphs per row, chars 0x20..0x7F).
   That file is Markus Kuhn's `ucs-fonts` 5x8.bdf (X11 "misc-fixed"), whose BDF
   COPYRIGHT property reads: "Public domain font.  Share and enjoy."
   Encoding here: 96 glyphs x 5 columns, one byte per column, bit r = row r (top
   down). Regenerate with tools-free Python from that .raw if it ever changes.     */
window.OLED = (() => {
  'use strict';

  const W = 72, H = 40, COLS = 14, ROWS = 5, GLYPH_W = 5, ROW_H = 8;

  const FONT_B64 =
    'AAAAAAAAAF4AAAAOAA4AFH8UfxQEKn8qEAAWCDQANkk2QAAAAA4AAAA8QgAAAEI8AABUODhUABAQ' +
    'fBAQAIBgIAAQEBAQAABA4EAAYBAIBgAAPEI8AABEfkAAZFJSTAAiSk4yABgUfhAALkpKMgA8Skow' +
    'AAJiGgYANEpKNAAMUlI8AABsbAAAAIBsLAAAGCRCACgoKCgAAEIkGAAABFIMADxCmaUefBISfAB+' +
    'Sko0ADxCQiQAfkJCPAB+SkpCAH4KCgIAPEJSNAB+CAh+AABCfkIAIEI+AgB+CDRCAH5AQEAAfgwM' +
    'fgB+DDh+ADxCQjwAfhISDAA8UmK8AH4SEmwAJEpSJAAAAn4CAD5AQD4AHmBgHgB+MDB+AGYYGGYA' +
    'BghwCAZiUkpGAAB+QkIABggQYAAAQkJ+AAAEAgQAgICAgAAAAgQAADBISHgAfkhIMAAAMEhIADBI' +
    'SH4AMGhYEAAQfBIEABCoqHAAfggIcAAASHpAAABAgHoAfhAQaAAAQn5AAHgIcAhweAgIcAAwSEgw' +
    'APgoKBAAECgo+AB4EAgQAABQWCgACD5IIAA4QEB4AAA4QDgAOEAwQDhIMDBIAFigoHgASGhYSAAI' +
    'KlVBAAAAfgAAQVUqCAAEAgQCAAAEUgwA';

  // decode once: Uint8Array(480), glyph g occupies [g*5, g*5+5)
  const FONT = (() => {
    const bin = atob(FONT_B64), a = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) a[i] = bin.charCodeAt(i);
    return a;
  })();

  /* ---- word-wrap, mirroring rust/clock/src/bard/textflow.rs -------------------
     Forward wrap, keep the LAST `rows` lines. Forward (not backward) is deliberate
     in the firmware: it keeps every line stable once it appears, so the text
     scrolls instead of re-flowing as the story grows.                            */
  function wrapTail(text, cols = COLS, rows = ROWS) {
    const lines = [];
    let cur = '';
    for (const word of String(text).split(/\s+/)) {
      if (!word) continue;
      if (!cur.length) cur = word;
      else if (cur.length + 1 + word.length <= cols) cur += ' ' + word;
      else { lines.push(cur); cur = word; }
      // a single word longer than the panel is hard-broken, as the panel would
      while (cur.length > cols) { lines.push(cur.slice(0, cols)); cur = cur.slice(cols); }
    }
    if (cur.length) lines.push(cur);
    return { lines, tail: lines.slice(Math.max(0, lines.length - rows)) };
  }

  /* ---- the panel ------------------------------------------------------------ */
  function Panel(canvas, opts = {}) {
    const fg = opts.fg || [125, 249, 255];      // phosphor cyan
    const bg = opts.bg || [2, 16, 15];          // near-black glass
    canvas.width = W; canvas.height = H;
    const ctx = canvas.getContext('2d', { alpha: false });
    const img = ctx.createImageData(W, H);
    const fb = new Uint8Array(W * H);           // 1 byte per pixel, 0 or 1

    const api = {
      W, H, COLS, ROWS,
      clear() { fb.fill(0); return api; },
      px(x, y, on = 1) {
        x |= 0; y |= 0;
        if (x >= 0 && x < W && y >= 0 && y < H) fb[y * W + x] = on ? 1 : 0;
        return api;
      },
      rect(x, y, w, h, on = 1) {
        for (let j = 0; j < h; j++) for (let i = 0; i < w; i++) api.px(x + i, y + j, on);
        return api;
      },
      frame(x, y, w, h, on = 1) {          // 1px outline
        for (let i = 0; i < w; i++) { api.px(x + i, y, on); api.px(x + i, y + h - 1, on); }
        for (let j = 0; j < h; j++) { api.px(x, y + j, on); api.px(x + w - 1, y + j, on); }
        return api;
      },
      hline(x, y, w, on = 1) { for (let i = 0; i < w; i++) api.px(x + i, y, on); return api; },
      vline(x, y, h, on = 1) { for (let j = 0; j < h; j++) api.px(x, y + j, on); return api; },
      /** One FONT_5X8 glyph run at pixel (x, y). y is the glyph TOP, not a baseline. */
      text(str, x, y, on = 1) {
        let cx = x;
        for (const ch of String(str)) {
          const code = ch.codePointAt(0);
          const g = (code >= 0x20 && code <= 0x7f) ? code - 0x20 : 0x1f;  // fallback glyph
          const base = g * GLYPH_W;
          for (let c = 0; c < GLYPH_W; c++) {
            const col = FONT[base + c];
            for (let r = 0; r < ROW_H; r++) if ((col >> r) & 1) api.px(cx + c, y + r, on);
          }
          cx += GLYPH_W;
        }
        return api;
      },
      /** Text rows from the top-left, 5x8 grid — the firmware's layout. */
      rows(lines, on = 1) {
        lines.slice(0, ROWS).forEach((l, i) => api.text(l, 0, i * ROW_H, on));
        return api;
      },
      /** Solid caret block, one glyph cell wide, at grid (col, row). */
      caret(col, row, on = 1) { return api.rect(col * GLYPH_W, row * ROW_H + 1, GLYPH_W - 1, ROW_H - 2, on); },
      flush() {
        const d = img.data;
        for (let i = 0, p = 0; i < fb.length; i++, p += 4) {
          const c = fb[i] ? fg : bg;
          d[p] = c[0]; d[p + 1] = c[1]; d[p + 2] = c[2]; d[p + 3] = 255;
        }
        ctx.putImageData(img, 0, 0);
        return api;
      },
    };
    return api;
  }

  return { W, H, COLS, ROWS, GLYPH_W, ROW_H, Panel, wrapTail };
})();

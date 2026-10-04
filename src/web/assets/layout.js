// Splitters between the workspace panels: File Overview | middle (Plugins or Results) | right
// column, and Runs / Triage Hints inside the right column. Drag, or focus one and use the arrow
// keys; double-click resets. Sizes are CSS variables on .wb, remembered in this browser.

import { prefs } from './core.js';

// minimum sizes (px); the CSS has the same minimums, these keep the drag from overshooting
const MIN = { c1: 220, mid: 360, c3: 260, r1: 90, r2: 90 };
const STEP = 16;

export function initSplitters() {
  const wb = document.querySelector('.wb');
  const size = prefs.get('layout', {});
  const apply = () => {
    for (const k of ['c1', 'c3', 'r1']) {
      if (size[k]) wb.style.setProperty('--' + k, size[k] + 'px'); else wb.style.removeProperty('--' + k);
    }
  };
  const save = () => { prefs.set('layout', size); apply(); };
  apply();

  const shown = sel => { const e = document.querySelector(sel); return e && getComputedStyle(e).display !== 'none'; };

  // the value each splitter sets, from a pointer position
  const calc = {
    s1: (x, y, r) => clamp(x - r.left, MIN.c1, r.width - 10 - MIN.mid - current('s2')),
    s2: (x, y, r) => clamp(r.right - x, MIN.c3, r.width - 10 - MIN.mid - (shown('.wb-overview') ? leftWidth() : 0)),
    s3: (x, y, r) => clamp(y - r.top, MIN.r1, r.height - 5 - MIN.r2),
  };
  const key = { s1: 'c1', s2: 'c3', s3: 'r1' };
  const leftWidth = () => document.querySelector('.wb-overview').getBoundingClientRect().width;
  const current = id => {
    if (id === 's1') return leftWidth();
    if (id === 's2') return document.querySelector('.wb-runs').getBoundingClientRect().width;
    return document.querySelector('.wb-runs').getBoundingClientRect().height;
  };

  for (const id of ['s1', 's2', 's3']) {
    const sp = document.getElementById(id);
    const vertical = id !== 's3';
    sp.addEventListener('pointerdown', e => {
      if (e.button !== 0) return;
      e.preventDefault();
      sp.setPointerCapture(e.pointerId);
      sp.classList.add('drag');
      document.body.classList.add('resizing', vertical ? 'v' : 'h');
      const move = ev => { size[key[id]] = Math.round(calc[id](ev.clientX, ev.clientY, wb.getBoundingClientRect())); apply(); };
      const up = () => {
        sp.classList.remove('drag');
        document.body.classList.remove('resizing', 'v', 'h');
        sp.removeEventListener('pointermove', move);
        sp.removeEventListener('pointerup', up);
        sp.removeEventListener('pointercancel', up);
        save();
      };
      sp.addEventListener('pointermove', move);
      sp.addEventListener('pointerup', up);
      sp.addEventListener('pointercancel', up);
    });
    sp.addEventListener('dblclick', () => { delete size[key[id]]; save(); });
    sp.addEventListener('keydown', e => {
      const grow = { s1: ['ArrowRight', 'ArrowLeft'], s2: ['ArrowLeft', 'ArrowRight'], s3: ['ArrowDown', 'ArrowUp'] }[id];
      const d = e.key === grow[0] ? STEP : e.key === grow[1] ? -STEP : 0;
      if (!d) return;
      e.preventDefault();
      const r = wb.getBoundingClientRect();
      const now = size[key[id]] || current(id);
      // express the new size as the pointer position that would produce it, then clamp as a drag would
      const pos = id === 's1' ? r.left + now + d : id === 's2' ? r.right - (now + d) : r.top + now + d;
      size[key[id]] = Math.round(calc[id](pos, pos, r));
      save();
    });
  }

  // keep remembered sizes inside a smaller window
  addEventListener('resize', () => {
    const r = wb.getBoundingClientRect();
    let changed = false;
    if (size.c1 && size.c1 > r.width - 10 - MIN.mid - (size.c3 || MIN.c3)) { size.c1 = Math.max(MIN.c1, Math.round(r.width - 10 - MIN.mid - (size.c3 || MIN.c3))); changed = true; }
    if (size.c3 && size.c3 > r.width - 10 - MIN.mid) { size.c3 = Math.max(MIN.c3, Math.round(r.width - 10 - MIN.mid)); changed = true; }
    if (size.r1 && size.r1 > r.height - 5 - MIN.r2) { size.r1 = Math.max(MIN.r1, Math.round(r.height - 5 - MIN.r2)); changed = true; }
    if (changed) apply();
  });
}

const clamp = (v, lo, hi) => Math.max(lo, Math.min(Math.max(lo, hi), v));

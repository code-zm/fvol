// Shared plumbing: API calls, the live event stream, the client store, DOM helpers.

/** The access token: from the startup URL's fragment (#token=...) into this origin's
 * localStorage (origin-scoped, so other ports on 127.0.0.1 can't read it), then off the URL.
 * Never a cookie: cookies are not port-isolated. */
function initToken() {
  const m = /(?:^#|&)token=([^&]+)/.exec(location.hash);
  if (m) {
    const t = decodeURIComponent(m[1]);
    try { localStorage.setItem('fastvol.token', t); } catch (e) { /* storage blocked: keep it for this page only */ }
    const rest = location.hash.replace(/(^#|&)token=[^&]+/, '').replace(/^#&/, '#');
    history.replaceState(null, '', location.pathname + location.search + (rest.length > 1 ? rest : ''));
    return t;
  }
  try { return localStorage.getItem('fastvol.token') || ''; } catch (e) { return ''; }
}
export let TOKEN = initToken();
export function setToken(t) {
  TOKEN = t;
  try { localStorage.setItem('fastvol.token', t); } catch (e) { /* ignore */ }
}

export class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status; }
}

/** JSON API call. `body` objects are sent as JSON. */
export async function api(path, { method = 'GET', body, signal } = {}) {
  const headers = { 'x-vol-token': TOKEN };
  let payload;
  if (body !== undefined) { headers['content-type'] = 'application/json'; payload = JSON.stringify(body); }
  const r = await fetch('/api/' + path, { method, headers, body: payload, signal, credentials: 'same-origin', cache: 'no-store' });
  const text = await r.text();
  let data = null;
  try { data = text ? JSON.parse(text) : null; } catch (e) { /* not JSON */ }
  if (!r.ok) throw new ApiError(r.status, (data && data.error) || text || r.statusText);
  return data;
}

// ------------------------------------------------------------------ tiny pub/sub
const listeners = new Map();
export function on(evt, fn) {
  if (!listeners.has(evt)) listeners.set(evt, new Set());
  listeners.get(evt).add(fn);
  return () => listeners.get(evt).delete(fn);
}
export function emit(evt, data) {
  const s = listeners.get(evt);
  if (s) for (const fn of [...s]) { try { fn(data); } catch (e) { console.error(e); } }
}

// ------------------------------------------------------------------ store
export const store = {
  session: null,
  plugins: [],
  pluginMap: new Map(),
  runs: new Map(),          // id -> summary of one plugin execution
  batches: [],              // runs as the user sees them: several plugins started together
  procRun: null,            // run id of the process list
};

/** The open image's runs (batches), newest first. */
export function sessionBatches() {
  const sid = store.session && store.session.id;
  return store.batches.filter(b => b.session === sid).sort((a, b) => b.id - a.id);
}

export function pluginOs(name) {
  const p = name.split('.')[0];
  return p === 'windows' || p === 'linux' || p === 'mac' ? p : 'generic';
}

export function shortName(name) {
  // windows.malware.malfind.Malfind -> malware.malfind
  const parts = name.split('.');
  const os = pluginOs(name);
  const mid = parts.slice(os === 'generic' ? 0 : 1, -1);
  return mid.join('.') || parts[parts.length - 1];
}

// ------------------------------------------------------------------ event stream
let connFails = 0;
export async function startEvents() {
  const conn = document.getElementById('conn');
  for (;;) {
    try {
      const r = await fetch('/api/events', { headers: { 'x-vol-token': TOKEN }, cache: 'no-store' });
      if (!r.ok) throw new Error('events ' + r.status);
      const reader = r.body.getReader();
      const dec = new TextDecoder();
      let buf = '';
      conn.hidden = true;
      connFails = 0;
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let nl;
        while ((nl = buf.indexOf('\n')) >= 0) {
          const line = buf.slice(0, nl);
          buf = buf.slice(nl + 1);
          if (line) handleEvent(JSON.parse(line));
        }
      }
    } catch (e) { /* fall through to reconnect */ }
    connFails++;
    if (connFails > 1) conn.hidden = false;
    await sleep(Math.min(5000, 300 * connFails));
  }
}

function handleEvent(ev) {
  if (ev.t === 'batches') {
    store.batches = ev.batches;
    emit('batches', ev.batches);
  } else if (ev.t === 'session') {
    const prev = store.session;
    store.session = ev.session;
    emit('session', { prev, cur: ev.session });
  } else if (ev.t === 'runs') {
    const changed = [];
    if (ev.full) {
      for (const id of [...store.runs.keys()]) if (!ev.runs.some(r => r.id === id)) store.runs.delete(id);
    }
    for (const r of ev.runs) {
      const prev = store.runs.get(r.id);
      if (prev && prev.seq > r.seq) continue;
      store.runs.set(r.id, r);
      if (!prev || prev.seq !== r.seq || prev.status !== r.status || prev.elapsed !== r.elapsed) changed.push(r);
    }
    for (const id of ev.removed || []) store.runs.delete(id);
    if (changed.length || (ev.removed && ev.removed.length) || ev.full) emit('runs', changed);
    for (const r of changed) emit('run:' + r.id, r);
  }
}

// ------------------------------------------------------------------ runs
/** Start (or reuse) a run; resolves to the run summary. */
export async function runPlugin(plugin, args = {}, { reuse = false, origin = 'user' } = {}) {
  const res = await api('runs', { method: 'POST', body: { plugin, args, reuse, origin } });
  // the event stream may already have delivered a newer state of this run
  const prev = store.runs.get(res.run.id);
  if (!prev || res.run.seq >= prev.seq) store.runs.set(res.run.id, res.run);
  const cur = store.runs.get(res.run.id);
  emit('runs', [cur]);
  emit('run:' + cur.id, cur);
  return cur;
}

/** Resolve when the run is finished. */
export function whenDone(id) {
  return new Promise(resolve => {
    const r = store.runs.get(id);
    if (r && finished(r)) return resolve(r);
    const off = on('run:' + id, s => { if (finished(s)) { off(); resolve(s); } });
  });
}

export const finished = r => r.status === 'done' || r.status === 'failed' || r.status === 'cancelled';

/** Fetch rows [from, from+count) of a run's view. */
export function fetchRows(id, view, from, count, signal) {
  return api(`runs/${id}/rows?view=${view}&from=${from}&count=${count}`, { signal });
}

/** All rows of a (small) finished run as arrays of display strings. */
export async function allRows(id, max = 200000) {
  const out = [];
  for (let from = 0; ; from += 5000) {
    const r = await fetchRows(id, 0, from, 5000);
    for (const row of r.rows) out.push(row);
    if (r.rows.length < 5000 || out.length >= max) break;
  }
  return out;
}

export const cellText = c => (c === null ? '-' : c === 0 ? 'N/A' : c);

/** Download an API URL: plain links can't send the token header, so ask for a single-use,
 * 60-second ticket bound to exactly this URL first. */
export async function download(path) {
  try {
    const { url } = await api('ticket', { method: 'POST', body: { path } });
    const a = el('a', { href: url, download: '' });
    document.body.append(a);
    a.click();
    a.remove();
  } catch (e) { toast('Download failed: ' + e.message, 'bad'); }
}

// ------------------------------------------------------------------ DOM helpers
/** el('div.cls#id', {attrs, on: {click}}, ...children). Text children are text nodes. */
export function el(spec, attrs, ...kids) {
  const m = /^([a-z0-9]+)?((?:[.#][\w-]+)*)$/i.exec(spec) || [];
  const node = document.createElement(m[1] || 'div');
  if (m[2]) for (const part of m[2].match(/[.#][\w-]+/g) || []) {
    if (part[0] === '.') node.classList.add(part.slice(1)); else node.id = part.slice(1);
  }
  if (attrs && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) { kids.unshift(attrs); attrs = null; }
  if (attrs) {
    for (const [k, v] of Object.entries(attrs)) {
      if (v === undefined || v === null || v === false) continue;
      if (k === 'on') for (const [e, fn] of Object.entries(v)) node.addEventListener(e, fn);
      else if (k === 'text') node.textContent = v;
      else if (k === 'class') node.className = v;
      else if (k === 'style') Object.assign(node.style, v);
      else if (k === 'dataset') Object.assign(node.dataset, v);
      else if (k in node && typeof v !== 'string') node[k] = v;
      else node.setAttribute(k, v === true ? '' : v);
    }
  }
  for (const k of kids.flat(Infinity)) {
    if (k === null || k === undefined || k === false) continue;
    node.append(k instanceof Node ? k : document.createTextNode(String(k)));
  }
  return node;
}

export function clear(node) { while (node.firstChild) node.removeChild(node.firstChild); return node; }

export const sleep = ms => new Promise(r => setTimeout(r, ms));

export function debounce(fn, ms) {
  let t;
  return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); };
}

// ------------------------------------------------------------------ formatting
export function fmtBytes(n) {
  if (n == null) return '';
  const u = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let i = 0, v = n;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${n} B` : `${v.toFixed(v < 10 ? 2 : 1)} ${u[i]}`;
}
export function fmtCount(n) { return (n ?? 0).toLocaleString('en-US'); }
export function fmtMs(ms) {
  if (ms == null) return '';
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(ms < 10000 ? 2 : 1)} s`;
  const m = Math.floor(ms / 60000), s = Math.round((ms % 60000) / 1000);
  return `${m}m ${String(s).padStart(2, '0')}s`;
}
export function fmtAgo(epochMs) {
  const d = (Date.now() - epochMs) / 1000;
  if (d < 45) return 'just now';
  if (d < 3600) return `${Math.round(d / 60)} min ago`;
  if (d < 86400) return `${Math.round(d / 3600)} h ago`;
  return new Date(epochMs).toISOString().slice(0, 16).replace('T', ' ');
}
/** CLI datetime text -> epoch seconds (null if not a time). */
export function parseTime(s) {
  if (typeof s !== 'string') return null;
  const m = /^(\d{4})-(\d\d)-(\d\d)[ T](\d\d):(\d\d):(\d\d)(?:\.(\d+))?/.exec(s);
  if (!m) return null;
  const t = Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]) / 1000;
  return t + (m[7] ? +('0.' + m[7]) : 0);
}
export function fmtTime(secs, withDate = true) {
  if (secs == null) return '';
  const iso = new Date(secs * 1000).toISOString();
  return withDate ? iso.slice(0, 19).replace('T', ' ') : iso.slice(11, 19);
}

// ------------------------------------------------------------------ python int(x, 0)
/** Parse like python int(x, 0); returns a BigInt or null. */
export function int0(s) {
  if (typeof s === 'number') return Number.isInteger(s) ? BigInt(s) : null;
  let t = String(s).trim();
  let neg = false;
  if (t[0] === '-' || t[0] === '+') { neg = t[0] === '-'; t = t.slice(1); }
  if (!t) return null;
  let radix = 10, digits = t, prefixed = false;
  if (/^0[xob]/i.test(t)) { radix = { x: 16, o: 8, b: 2 }[t[1].toLowerCase()]; digits = t.slice(2); prefixed = true; }
  if (!digits) return null;
  // underscores: single, between digits (one allowed right after a base prefix)
  if (/__/.test(digits) || digits.endsWith('_') || (!prefixed && digits.startsWith('_'))) return null;
  const clean = digits.replace(/_/g, '');
  if (!clean) return null;
  const re = { 2: /^[01]+$/, 8: /^[0-7]+$/, 10: /^[0-9]+$/, 16: /^[0-9a-f]+$/i }[radix];
  if (!re.test(clean)) return null;
  // decimal: no leading zeros except zero itself
  if (radix === 10 && clean.length > 1 && clean[0] === '0' && /[1-9]/.test(clean)) return null;
  let v = 0n;
  const R = BigInt(radix);
  for (const ch of clean.toLowerCase()) v = v * R + BigInt(parseInt(ch, 16));
  return neg ? -v : v;
}

// ------------------------------------------------------------------ clipboard & toasts
export function toast(msg, kind = '') {
  const t = el('div.toast' + (kind ? '.' + kind : ''), { text: msg });
  document.getElementById('toasts').append(t);
  setTimeout(() => t.remove(), kind === 'bad' ? 6000 : 2200);
}
export async function copy(text, what = 'Copied') {
  try {
    await navigator.clipboard.writeText(text);
  } catch (e) {
    const ta = el('textarea', { style: { position: 'fixed', left: '-9999px' } });
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand('copy');
    ta.remove();
  }
  toast(`${what}${text.length < 60 ? ': ' + text.replace(/\s+/g, ' ') : ''}`);
}

// ------------------------------------------------------------------ persistence (per image)
export const prefs = {
  get(k, d) { try { const v = localStorage.getItem('fastvol.' + k); return v === null ? d : JSON.parse(v); } catch (e) { return d; } },
  set(k, v) { try { localStorage.setItem('fastvol.' + k, JSON.stringify(v)); } catch (e) { /* quota / blocked */ } },
};

// ------------------------------------------------------------------ menus
let openMenu = null;
export function closeMenu() { if (openMenu) { openMenu.remove(); openMenu = null; } }
/** Pop a menu below `anchor`. items: [{label, hint, act}] | 'sep' | {header}. */
export function menu(anchor, items) {
  closeMenu();
  const m = el('div.menu', { role: 'menu' });
  for (const it of items) {
    if (it === 'sep') { m.append(el('div.sep', { role: 'separator' })); continue; }
    if (it.header) { m.append(el('div.mh', { text: it.header })); continue; }
    if (it.node) { m.append(it.node); continue; }
    m.append(el('button.mi', { role: 'menuitem', type: 'button', on: { click: () => { closeMenu(); it.act(); } } }, it.label, it.hint ? el('small', { text: it.hint }) : null));
  }
  document.body.append(m);
  const r = anchor.getBoundingClientRect();
  const w = m.offsetWidth, h = m.offsetHeight;
  m.style.left = Math.max(6, Math.min(r.left, innerWidth - w - 6)) + 'px';
  m.style.top = (r.bottom + h + 6 < innerHeight ? r.bottom + 4 : Math.max(6, r.top - h - 4)) + 'px';
  openMenu = m;
  const first = m.querySelector('button, input');
  if (first) first.focus();
  m.addEventListener('keydown', e => {
    const btns = [...m.querySelectorAll('button.mi, label.mi input')];
    const i = btns.indexOf(document.activeElement);
    if (e.key === 'ArrowDown') { e.preventDefault(); btns[(i + 1) % btns.length].focus(); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); btns[(i - 1 + btns.length) % btns.length].focus(); }
    else if (e.key === 'Escape') { e.preventDefault(); closeMenu(); anchor.focus(); }
  });
  return m;
}
document.addEventListener('mousedown', e => { if (openMenu && !openMenu.contains(e.target)) closeMenu(); }, true);

/** Modal overlay; returns {root, close}. Esc and scrim clicks close it; focus returns. */
export function modal(node, { onClose } = {}) {
  const prevFocus = document.activeElement;
  const scrim = el('div.scrim');
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    scrim.remove(); node.remove();
    document.removeEventListener('keydown', key, true);
    if (onClose) onClose();
    if (prevFocus && prevFocus.focus) prevFocus.focus();
  };
  const key = e => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    if (e.key === 'Tab') { // focus trap
      const f = [...node.querySelectorAll('button, input, select, textarea, [tabindex="0"], a[href]')].filter(x => !x.disabled && x.offsetParent);
      if (!f.length) return;
      if (e.shiftKey && document.activeElement === f[0]) { e.preventDefault(); f[f.length - 1].focus(); }
      else if (!e.shiftKey && document.activeElement === f[f.length - 1]) { e.preventDefault(); f[0].focus(); }
    }
  };
  scrim.addEventListener('mousedown', close);
  document.addEventListener('keydown', key, true);
  node.setAttribute('role', node.getAttribute('role') || 'dialog');
  node.setAttribute('aria-modal', 'true');
  document.body.append(scrim, node);
  return { root: node, close };
}

// Bootstrap: token, plugins, session and runs; the workspace panels; the Quick Start.

import { store, api, on, el, startEvents, TOKEN, setToken } from './core.js';
import { renderAll, wire } from './workspace.js';
import { loadProcesses } from './procdata.js';
import { showQuickStart } from './quickstart.js';
import { initSplitters } from './layout.js';
import { showOptions } from './options.js';
import { wireResults, closeResults, stepTab, openBatchId, focusResultsFilter } from './results.js';

function applyTheme(t) {
  document.documentElement.setAttribute('data-theme', t);
  try { localStorage.setItem('fastvol.theme', t); } catch (e) { /* ignore */ }
}
const toggleTheme = () => applyTheme(document.documentElement.getAttribute('data-theme') === 'light' ? 'dark' : 'light');

/** No valid token: ask for it (it is in the URL `fvol serve` prints). */
function lockScreen(wrong) {
  const inp = el('input.qs-input', { type: 'password', autocomplete: 'off', spellcheck: false, placeholder: 'access token', 'aria-label': 'Access token' });
  const go = () => { if (inp.value.trim()) { setToken(inp.value.trim()); location.reload(); } };
  inp.addEventListener('keydown', e => { if (e.key === 'Enter') go(); });
  const logo = document.querySelector('.brand-logo').cloneNode(true);
  document.body.append(el('div.qs', { role: 'dialog', 'aria-label': 'Access token needed' },
    el('div.qs-lock', {},
      el('div.qs-brand', {}, logo),
      el('h2.qs-title', { text: 'Locked' }),
      el('p.qs-sub', { text: 'This server gives access to a memory image. Open the URL printed by fvol serve (it carries the token), or paste the token here.' }),
      wrong ? el('p.qs-err', { role: 'alert', text: 'The saved token is not valid for this server (it changes every time fvol serve starts).' }) : null,
      el('div.qs-pathrow', {}, inp, el('button.qs-btn.primary', { type: 'button', text: 'Unlock', on: { click: go } })))));
  inp.focus();
}

function shortcuts() {
  document.addEventListener('keydown', e => {
    if (openBatchId() !== null && (e.ctrlKey || e.metaKey) && (e.key === 'ArrowLeft' || e.key === 'ArrowRight') && !(e.target && e.target.tagName === 'INPUT')) { e.preventDefault(); stepTab(e.key === 'ArrowRight' ? 1 : -1); return; }
    const t = e.target;
    if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable) || e.ctrlKey || e.metaKey || e.altKey) return;
    if (document.querySelector('.qs, .wb-dialog, .menu')) return;
    if (e.key === 'T') { e.preventDefault(); toggleTheme(); }
    // results: [ and ] step through the plugin tabs, / filters, Esc closes
    if (openBatchId() !== null) {
      if (e.key === '[' || e.key === ']') { e.preventDefault(); stepTab(e.key === ']' ? 1 : -1); }
      if (e.key === '/' && focusResultsFilter()) e.preventDefault();
      if (e.key === 'Escape') { e.preventDefault(); closeResults(); }
    }
  });
}

async function main() {
  document.getElementById('wb-theme').addEventListener('click', toggleTheme);
  document.getElementById('wb-quickstart').addEventListener('click', () => showQuickStart());
  document.getElementById('wb-options').addEventListener('click', () => showOptions());
  // narrow windows hide the File Overview first; this button slides it in over the plugins
  const ovBtn = document.getElementById('wb-ovbtn');
  const setOv = open => { document.body.classList.toggle('ov-open', open); ovBtn.setAttribute('aria-pressed', String(open)); };
  ovBtn.addEventListener('click', () => setOv(!document.body.classList.contains('ov-open')));
  document.addEventListener('keydown', e => { if (e.key === 'Escape' && document.body.classList.contains('ov-open')) setOv(false); });
  matchMedia('(min-width: 1280px)').addEventListener('change', m => { if (m.matches) setOv(false); });
  shortcuts();
  wireResults();
  initSplitters();
  if (!TOKEN) { lockScreen(false); return; }
  try {
    const [plugins, session, runs, batches] = await Promise.all([api('plugins'), api('session'), api('runs'), api('batches').catch(() => [])]);
    store.batches = batches;
    store.plugins = plugins;
    store.pluginMap = new Map(plugins.map(p => [p.name, p]));
    store.session = session;
    for (const r of runs) store.runs.set(r.id, r);
  } catch (e) {
    if (e.status === 401) { lockScreen(true); return; }
    document.getElementById('main').replaceChildren(el('div.wb-empty', {}, el('div', { text: 'Can’t reach the fastvol server' }), el('div.wb-empty-sub', { text: e.message })));
    return;
  }
  wire();
  renderAll();
  // the process list feeds the triage hints; load it whenever an image is ready
  const procs = () => { if (store.session && store.session.state === 'ready') loadProcesses(); };
  on('session', procs);
  procs();
  startEvents();
  await showQuickStart();
  document.getElementById('main').focus();
}

main();

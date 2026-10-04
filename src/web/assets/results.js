// The results of one run, in place of the File Overview and Plugins columns: "<name>: Results",
// a tab per plugin (status and rows, live), and for the chosen plugin a virtualised table with
// a filter, per-column filters and exports. Runs and Triage Hints stay on the right.

import { store, api, on, el, clear, fmtCount, shortName, download, menu, toast } from './core.js';
import { VirtualTable } from './table.js';

const $ = id => document.getElementById(id);
const ACTIVE = st => st === 'queued' || st === 'running';
const clock = ms => { const t = Math.max(0, Math.round(ms / 1000)); return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}`; };
const EXPORTS = [['csv', 'CSV'], ['tsv', 'TSV'], ['json', 'JSON'], ['jsonl', 'JSON Lines'], ['md', 'Markdown']];

// q: the Filter rows text of the open run, shared by all of its plugin tabs. Each run keeps its
// own, stored with the run on the server (so a saved analysis brings it back).
// `filters` holds what was typed in this page, ahead of the server's copy arriving back.
const rv = { batch: null, tab: null, tables: new Map(), timer: null, q: '', save: null, filters: new Map() };

export const openBatchId = () => rv.batch;

/** Show a run's results; `runId` picks the plugin tab (default: the first). */
export function openResults(batchId, runId = null) {
  const b = store.batches.find(x => x.id === batchId);
  if (!b) return;
  if (rv.batch !== batchId) { flushFilter(); closeTables(); rv.q = rv.filters.has(batchId) ? rv.filters.get(batchId) : b.filter || ''; }
  rv.batch = batchId;
  rv.tab = runId && b.runs.includes(runId) ? runId : rv.tab && b.runs.includes(rv.tab) ? rv.tab : b.runs[0];
  document.body.classList.add('results-open');
  $('results').hidden = false;
  render();
  document.dispatchEvent(new CustomEvent('fastvol-results', { detail: { batch: batchId } }));
  if (!rv.timer) rv.timer = setInterval(renderHead, 1000);
}

export function closeResults() {
  if (rv.batch === null) return;
  flushFilter();
  closeTables();
  rv.batch = null;
  rv.tab = null;
  clearInterval(rv.timer);
  rv.timer = null;
  document.body.classList.remove('results-open');
  $('results').hidden = true;
  document.dispatchEvent(new CustomEvent('fastvol-results', { detail: { batch: null } }));
}

function closeTables() {
  for (const t of rv.tables.values()) t.table && t.table.destroy();
  rv.tables.clear();
}

function batch() { return store.batches.find(x => x.id === rv.batch); }

/** Store the open run's filter on the server, 400 ms after the last keystroke. */
function saveFilter() {
  const b = batch();
  if (!b) return;
  b.filter = rv.q;
  rv.filters.set(b.id, rv.q);
  clearTimeout(rv.save && rv.save.timer);
  const pending = { id: b.id, q: rv.q };
  pending.timer = setTimeout(() => sendFilter(pending), 400);
  rv.save = pending;
}
function flushFilter() {
  if (!rv.save) return;
  clearTimeout(rv.save.timer);
  sendFilter(rv.save);
}
function sendFilter(p) {
  if (rv.save === p) rv.save = null;
  api(`batches/${p.id}/filter`, { method: 'POST', body: { q: p.q } }).catch(e => toast('Could not save the filter: ' + e.message, 'bad'));
}

function render() {
  renderHead();
  renderTabs();
  renderBody();
}

function renderHead() {
  const b = batch();
  if (!b) return;
  const runs = b.runs.map(id => store.runs.get(id)).filter(Boolean);
  const active = runs.filter(r => ACTIVE(r.status)).length;
  const ends = runs.filter(r => r.started).map(r => r.started + r.elapsed);
  const elapsed = active ? Date.now() - b.created : ends.length ? Math.max(...ends) - b.created : 0;
  $('rs-title').textContent = `${b.name}: Results`;
  $('rs-sum').textContent = `${runs.length - active}/${b.runs.length} · ${clock(elapsed)}`;
  $('rs-sum').classList.toggle('active', active > 0);
  if (!active && rv.timer) { clearInterval(rv.timer); rv.timer = null; }
}

function badge(r) {
  if (!r) return el('span.rs-badge', { text: '?' });
  if (r.status === 'done') return el('span.rs-badge', { text: fmtCount(r.rows) });
  if (r.status === 'running') return el('span.rs-badge.running', { text: r.rows ? fmtCount(r.rows) : '…' });
  if (r.status === 'queued') return el('span.rs-badge.queued', { text: 'queued' });
  return el('span', { class: 'rs-badge ' + r.status, text: r.status });
}

/** The tab bar; `scroll` brings the chosen tab into view (not on the progress ticks). */
function renderTabs(scroll = true) {
  const b = batch();
  const bar = $('rs-tabs');
  // rebuilt on every progress tick while plugins run: keyboard focus stays on its tab
  const focused = bar.contains(document.activeElement) ? document.activeElement.dataset.run : null;
  clear(bar);
  for (const id of b.runs) {
    const r = store.runs.get(id);
    const sel = id === rv.tab;
    const tab = el('button.rs-tab', { type: 'button', role: 'tab', 'aria-selected': String(sel), tabindex: sel ? 0 : -1, dataset: { run: id },
      title: r ? `${r.plugin} ${r.args.join(' ')}` : '', on: { click: () => selectTab(id) } },
      el('span', { class: 'rs-dot ' + (r ? r.status : '') }),
      el('span.rs-tname', { text: r ? shortName(r.plugin) : 'run ' + id }),
      r && r.args.length ? el('span.rs-targs', { text: r.args.join(' ') }) : null,
      badge(r));
    bar.append(tab);
  }
  if (focused) { const f = bar.querySelector(`[data-run="${focused}"]`); if (f) f.focus({ preventScroll: true }); }
  const cur = bar.querySelector('[aria-selected="true"]');
  if (scroll && cur) cur.scrollIntoView({ block: 'nearest', inline: 'nearest' });
}

function selectTab(id, focusTable = false) {
  if (id === rv.tab) return;
  rv.tab = id;
  renderTabs();
  renderBody();
  document.dispatchEvent(new CustomEvent('fastvol-results', { detail: { batch: rv.batch, run: id } }));
  const t = rv.tables.get(id);
  if (focusTable && t && t.table) t.table.root.focus();
  else { const tab = $('rs-tabs').querySelector('[aria-selected="true"]'); if (tab) tab.focus(); }
}

/** Step to the previous (-1) or next (+1) plugin tab. */
export function stepTab(d) {
  const b = batch();
  if (!b) return;
  const k = b.runs.indexOf(rv.tab);
  const next = b.runs[(k + d + b.runs.length) % b.runs.length];
  selectTab(next);
}

/** The body of the chosen tab: a table once the plugin has columns, else its state. */
function renderBody() {
  const body = $('rs-body');
  const r = store.runs.get(rv.tab);
  let t = rv.tables.get(rv.tab);
  if (!t) { t = { node: el('div.rs-pane'), table: null }; rv.tables.set(rv.tab, t); }
  body.replaceChildren(t.node);
  fillPane(t, r);
}

function fillPane(t, r) {
  if (!r) { t.node.replaceChildren(el('div.wb-empty', { text: 'This plugin is no longer on the server.' })); return; }
  if (r.status === 'failed' || r.status === 'cancelled') {
    if (t.table) { t.table.destroy(); t.table = null; }
    const e = r.error || {};
    t.node.replaceChildren(el('div.rs-state', {},
      el('div', { class: 'rs-state-t ' + r.status, text: e.title || (r.status === 'failed' ? 'The plugin failed' : 'Cancelled') }),
      e.message ? el('div.rs-state-m', { text: e.message }) : null,
      ...(e.hints || []).map(h => el('div.rs-state-h', { text: h })),
      e.detail ? el('pre.rs-state-d', { text: e.detail }) : null));
    return;
  }
  if (!t.table && r.cols && r.cols.length) {
    const table = new VirtualTable({ runId: r.id, plugin: r.plugin, label: `${shortName(r.plugin)} results` });
    table.q = rv.q;   // the first view is built already filtered
    t.table = table;
    const q = el('input.wb-input.rs-q', { type: 'search', value: rv.q, placeholder: 'Filter rows', spellcheck: false, autocomplete: 'off', 'aria-label': 'Filter rows' });
    q.addEventListener('input', () => { rv.q = q.value; table.setQuery(q.value); saveFilter(); });
    q.addEventListener('keydown', e => { if (e.key === 'ArrowDown' || e.key === 'Enter') { e.preventDefault(); table.root.focus(); } });
    const colsBtn = el('button.wb-btn.ghost.sm', { type: 'button', text: 'Filters', title: 'A filter box per column: text, =exact, !not, >0x10, /regex/', on: { click: () => { table.toggleFilters(); colsBtn.classList.toggle('on', table.showFilters); } } });
    const exp = el('button.wb-btn.ghost.sm', { type: 'button', text: 'Export', title: 'Download the rows shown, with the visible columns' });
    exp.addEventListener('click', () => menu(exp, [
      ...EXPORTS.map(([f, label]) => ({ label, act: () => download(table.exportUrl(f)) })),
      'sep',
      // what `fvol` prints for this plugin: -r, --filters and --hide-columns from Options
      { label: 'fvol output', hint: 'as the command line', act: () => download(`/api/runs/${r.id}/vol`) },
    ]));
    const count = el('span.rs-count');
    table.o.onState = () => { count.textContent = table.matched !== table.total || table.q ? `${fmtCount(table.matched)} of ${fmtCount(table.stored || table.total)} rows` : `${fmtCount(table.total)} rows`; };
    t.q = q;
    t.node.replaceChildren(el('div.rs-tools', {}, q, colsBtn, el('span.rs-sp'), count, exp), table.root);
    table.update(r);
    return;
  }
  if (t.table) {
    // coming back to a tab: apply the shared filter if it changed meanwhile
    if (t.table.q !== rv.q) { t.q.value = rv.q; t.table.setQuery(rv.q); }
    t.table.update(r);
    return;
  }
  t.node.replaceChildren(el('div.wb-empty', {}, r.status === 'queued'
    ? el('div', { text: 'Queued: waiting for a free slot (a few plugins run at a time).' })
    : [el('span.wb-spin'), 'Running…']));
}

export function focusResultsFilter() {
  const t = rv.tables.get(rv.tab);
  if (t && t.q) { t.q.focus(); t.q.select(); return true; }
  return false;
}

export function wireResults() {
  $('rs-close').addEventListener('click', closeResults);
  $('rs-tabs').addEventListener('keydown', e => {
    if (e.key === 'ArrowRight') { e.preventDefault(); stepTab(1); }
    if (e.key === 'ArrowLeft') { e.preventDefault(); stepTab(-1); }
    if (e.key === 'ArrowDown' || e.key === 'Enter') { const t = rv.tables.get(rv.tab); if (t && t.table) { e.preventDefault(); t.table.root.focus(); } }
  });
  on('runs', changed => {
    const b = batch();
    if (!b || !changed) return;
    const mine = changed.filter(r => b.runs.includes(r.id));
    if (!mine.length) return;
    renderTabs(false);
    renderHead();
    if (!rv.timer && mine.some(r => ACTIVE(r.status))) rv.timer = setInterval(renderHead, 1000);
    for (const r of mine) {
      const t = rv.tables.get(r.id);
      if (!t) continue;
      if (r.id === rv.tab) fillPane(t, r);
      else if (t.table) t.table.update(r);
    }
  });
  on('batches', () => {
    if (rv.batch === null) return;
    if (!batch()) { closeResults(); toast('That run was removed.'); return; }
    renderHead();
  });
  on('session', ({ prev, cur }) => { if (prev && cur && prev.id !== cur.id) closeResults(); });
}

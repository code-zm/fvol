// The workspace: four panels. File overview (left, terminal style), plugins (middle), runs (top
// right) and triage hints (bottom right). Each panel re-renders itself from the store.

import { store, api, on, el, clear, fmtBytes, fmtCount, fmtMs, fmtTime, pluginOs, shortName, sessionBatches, prefs, toast } from './core.js';
import { blurb } from './catalog.js';
import { captureTime, triageGroups } from './procdata.js';
import { openResults, openBatchId } from './results.js';
import { builtinPresets, userPresets, loadUserPresets, savePreset, deletePreset } from './presets.js';
import { loadSavedRules, setRules, clearRules, currentRules, ruleGroups } from './rules.js';

const $ = id => document.getElementById(id);

// ------------------------------------------------------------------ top bar
export function renderTopbar() {
  const s = store.session;
  const name = $('wb-ev-name'), meta = $('wb-ev-meta');
  if (!s || !s.image) { name.textContent = 'no image'; meta.textContent = ''; document.title = 'fastvol'; return; }
  name.textContent = s.name;
  name.title = s.image;
  const parts = [fmtBytes(s.size)];
  const os = osLabel(s);
  if (os) parts.push(os);
  const cap = captureTime();
  if (cap) parts.push('captured ' + fmtTime(cap) + ' UTC');
  if (s.state === 'warming') parts.push((s.phase || 'preparing') + '…');
  if (s.state === 'failed') parts.push('not recognised');
  meta.textContent = parts.join('  ·  ');
  document.title = `${s.name} - fastvol`;
}

function osLabel(s) {
  const F = Object.fromEntries(s.facts || []);
  const arch = s.arch === 'intel64' ? 'x64' : s.arch ? 'x86' : '';
  if (s.os === 'windows') { const b = (F['Major/Minor'] || '').split('.').pop(); return ['Windows', b && 'build ' + b, arch].filter(Boolean).join(' '); }
  if (s.os === 'linux') return ['Linux', ((F.Banner || '').match(/Linux version (\S+)/) || [])[1], arch].filter(Boolean).join(' ');
  if (s.os === 'mac') return ['macOS', ((F.Banner || '').match(/Darwin Kernel Version (\S+?):/) || [])[1], arch].filter(Boolean).join(' ');
  return '';
}

// ------------------------------------------------------------------ file overview (terminal)
/** `key ....... value` lines, key and value on the same line. All sections share one value
 * column (as wide as the longest key shown), so `fvol info` and `fvol windows.info` line up;
 * long values wrap under their own start. */
const present = rows => rows.filter(r => r && r[1] !== undefined && r[1] !== null && r[1] !== '');
function section(rows, w) {
  return rows.map(([k, v, cls]) => el('div.tl', {},
    el('span.tk', { text: k + ' ' + '.'.repeat(w - k.length - 1) }),
    el('span.tv', { class: 'tv ' + (cls || ''), text: String(v) })));
}

// facts too long for the shared column and of no use for triage
const HIDDEN_FACTS = new Set(['PE MajorOperatingSystemVersion', 'PE MinorOperatingSystemVersion', 'PE TimeDateStamp']);

const prompt = cmd => el('div.tl.tp', {}, el('span.tpr', { text: 'fastvol$ ' }), el('span', { text: cmd }));
const comment = t => el('div.tl.tc', { text: '# ' + t });
const blank = () => el('div.tl', { text: ' ' });
const isAddr = v => /^0x[0-9a-f]+$/i.test(v);

export function renderOverview() {
  const box = $('overview');
  const s = store.session;
  clear(box);
  box.append(prompt('fvol info'));
  if (!s || !s.image) {
    box.append(el('div.tl.tdim', { text: 'no memory image loaded.' }), el('div.tl.tdim', { text: 'open one from Quick Start.' }));
    return;
  }
  const cap = captureTime();
  const procs = store.procs;
  const info = present([
    ['image', s.name, 'hi'],
    ['path', s.image],
    ['size', `${fmtBytes(s.size)} (${fmtCount(s.size)} bytes)`],
    ['state', s.state === 'ready' ? `ready${s.warm_ms != null ? ' in ' + fmtMs(s.warm_ms) : ''}` : s.state === 'warming' ? (s.phase || 'preparing') + '…' : s.state, s.state === 'failed' ? 'bad' : s.state === 'ready' ? 'ok' : ''],
    ['os', osLabel(s)],
    ['captured', cap ? fmtTime(cap) + ' UTC' : ''],
    ['processes', procs ? `${fmtCount(procs.length)} (${procs.filter(p => p.exit !== null).length} exited)` : s.state === 'ready' ? (store.procError ? 'unavailable' : 'reading…') : ''],
    ['symbols', s.symbol_dirs.length ? s.symbol_dirs.join('; ') : 'default search path'],
    ['offline', s.offline ? 'yes' : 'no'],
    ['output', s.output_dir],
  ]);
  const facts = present((s.facts || []).filter(([k]) => !HIDDEN_FACTS.has(k)).map(([k, v]) => [k, v, isAddr(v) ? 'addr' : '']));
  const w = Math.max(...info.concat(facts).map(([k]) => k.length)) + 3;
  box.append(...section(info, w));
  if (s.notice) box.append(blank(), comment('saved analysis'), el('div.tl.bad', { text: s.notice }));
  if (s.state === 'failed') {
    box.append(blank(), comment('error'), el('div.tl.bad', { text: s.error || 'The image could not be analysed.' }));
    for (const b of s.banners || []) box.append(el('div.tl.tdim', { text: 'banner: ' + b }));
    for (const n of s.notes || []) box.append(el('div.tl.tdim', { text: n }));
  }
  if (facts.length) {
    box.append(blank(), prompt(s.os === 'windows' ? 'fvol windows.info' : 'fvol banners'));
    box.append(...section(facts, w));
  }
}

// ------------------------------------------------------------------ plugins
// Two views under the Plugins header: Presets, and Select (every plugin with a checkbox; a ticked
// plugin unfolds its options). Once anything is ticked, a Run bar slides up at the bottom.

const OS_TABS = [['all', 'All'], ['windows', 'Windows'], ['linux', 'Linux'], ['mac', 'macOS'], ['generic', 'Other']];
const pl = { view: 'select', q: '', os: null, sel: new Map(), fromPreset: null };   // sel: plugin name -> { option name: value }

const optReqs = p => p.reqs.filter(r => r.flag);
const required = r => !r.optional && (r.default === null || r.default === undefined);
const PLACEHOLDER = { int: 'number, e.g. 1234 or 0x4d2', list_int: 'numbers, e.g. 4 628 1136', str: 'text', list_str: 'values, space separated', uri: 'path or file:// URL' };

export function renderPlugins() {
  const box = $('plugins');
  const s = store.session;
  if (pl.os === null) pl.os = prefs.get('plOs', null) || (s && s.os) || 'all';
  $('pl-tab-presets').setAttribute('aria-selected', String(pl.view === 'presets'));
  $('pl-tab-select').setAttribute('aria-selected', String(pl.view === 'select'));
  updateActions();
  clear(box);
  if (pl.view === 'presets') { renderPresets(box); return; }
  const search = el('input.wb-input', { type: 'search', value: pl.q, placeholder: 'Search plugins', spellcheck: false, autocomplete: 'off', 'aria-label': 'Search plugins' });
  const tabs = el('div.wb-seg', { role: 'tablist', 'aria-label': 'Operating system' });
  for (const [id, label] of OS_TABS) {
    const n = id === 'all' ? store.plugins.length : store.plugins.filter(p => pluginOs(p.name) === id).length;
    tabs.append(el('button', { type: 'button', role: 'tab', 'aria-selected': String(pl.os === id), on: { click: () => { pl.os = id; prefs.set('plOs', id); renderPlugins(); } } }, label, el('span', { text: String(n) })));
  }
  const list = el('div.pl-list', { role: 'list' });
  const fill = () => {
    clear(list);
    const q = pl.q.trim().toLowerCase();
    const shown = store.plugins.filter(p => (pl.os === 'all' || pluginOs(p.name) === pl.os) &&
      (!q || p.name.toLowerCase().includes(q) || (blurb(p.name) || p.description || '').toLowerCase().includes(q)));
    for (const p of shown) list.append(pluginRow(p));
    if (!shown.length) list.append(el('div.wb-empty', { text: 'No plugin matches “' + pl.q + '”.' }));
  };
  search.addEventListener('input', () => { pl.q = search.value; fill(); });
  box.append(el('div.pl-tools', {}, search, tabs), list);
  fill();
}

function pluginRow(p) {
  const reqs = optReqs(p);
  const on = pl.sel.has(p.name);
  const cb = el('input.wb-check', { type: 'checkbox', checked: on, 'aria-label': 'Select ' + p.name, tabindex: -1 });
  const head = el('div.pl-row', { role: 'listitem', tabindex: 0, title: p.name, 'aria-checked': String(on), class: 'pl-row' + (on ? ' on' : '') },
    cb,
    el('div.pl-text', {},
      el('div.pl-main', {}, el('span.pl-name', { text: shortName(p.name) }), el('span.pl-full', { text: p.name })),
      el('div.pl-desc', { text: blurb(p.name) || p.description || '' })),
    reqs.length ? el('span.pl-opts', { text: reqs.length === 1 ? '1 option' : `${reqs.length} options` }) : el('span'));
  const wrap = el('div.pl-item', {}, head);
  const toggle = () => {
    if (pl.sel.has(p.name)) pl.sel.delete(p.name); else pl.sel.set(p.name, {});
    wrap.replaceWith(pluginRow(p));
    updateActions();
    const again = document.querySelector(`.pl-row[title="${CSS.escape(p.name)}"]`);
    if (again) again.focus({ preventScroll: true });
  };
  head.addEventListener('click', e => { if (e.target !== cb) e.preventDefault(); toggle(); });
  head.addEventListener('keydown', e => {
    if (e.key === ' ' || e.key === 'Enter') { e.preventDefault(); toggle(); }
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault();
      const rows = [...document.querySelectorAll('.pl-row')];
      const k = rows.indexOf(head) + (e.key === 'ArrowDown' ? 1 : -1);
      if (rows[k]) rows[k].focus();
    }
  });
  if (on && reqs.length) wrap.append(optionsBlock(p, reqs));
  return wrap;
}

/** The options of a ticked plugin: checkboxes for flags, small inputs for values. */
function optionsBlock(p, reqs) {
  const vals = pl.sel.get(p.name);
  const box = el('div.pl-options', { role: 'group', 'aria-label': 'Options of ' + p.name });
  for (const r of reqs) {
    const id = `opt-${p.name}-${r.name}`.replace(/[^\w-]/g, '_');
    const desc = el('span.po-desc', { text: r.description || '' });
    if (r.kind === 'bool') {
      const cb = el('input.wb-check', { id, type: 'checkbox', checked: !!vals[r.name], on: { change: () => { if (cb.checked) vals[r.name] = true; else delete vals[r.name]; } } });
      box.append(el('label.po-row.po-flag', { for: id }, cb, el('code.po-flagname', { text: r.flag }), desc));
      continue;
    }
    let input;
    if (r.kind === 'choice') {
      input = el('select.wb-select', { id }, el('option', { value: '', text: r.default ? `default (${r.default})` : '-' }), ...(r.choices || []).map(c => el('option', { value: c, text: c })));
      input.value = vals[r.name] || '';
      input.addEventListener('change', () => { if (input.value) vals[r.name] = input.value; else delete vals[r.name]; });
    } else {
      input = el('input.wb-input.po-input', { id, type: 'text', value: vals[r.name] || '', spellcheck: false, autocomplete: 'off',
        placeholder: r.default !== null && r.default !== undefined && !(Array.isArray(r.default) && !r.default.length) ? `default ${r.default}` : PLACEHOLDER[r.kind] || '' });
      input.addEventListener('input', () => { if (input.value.trim()) vals[r.name] = input.value.trim(); else delete vals[r.name]; });
    }
    box.append(el('div.po-row.po-value', {},
      el('label.po-flagname', { for: id }, el('code', { text: r.flag }), required(r) ? el('span.po-req', { text: 'required' }) : null),
      input, desc));
  }
  return box;
}

// ------------------------------------------------------------------ presets

/** The Presets view: built-in sets for the image's OS, then the user's own from ~/.fvol/presets. */
function renderPresets(box) {
  const s = store.session;
  const os = s && s.os;
  const list = el('div.ps-list');
  const builtin = builtinPresets(os);
  list.append(el('div.ps-sec', {}, el('span', { text: 'Built-in' }), el('span.ps-sec-note', { text: os ? `for ${osName(os)} images` : 'all systems' })));
  for (const p of builtin) list.append(presetRow(p));
  if (!builtin.length) list.append(el('div.wb-empty', { text: 'No built-in presets for this system.' }));
  list.append(el('div.ps-sec', {}, el('span', { text: 'Yours' }), el('span.ps-sec-note', { text: userPresets.dir ? userPresets.dir.replace(/^\/home\/[^/]+/, '~') : '~/.fvol/presets' })));
  if (!userPresets.loaded) {
    list.append(el('div.wb-empty', {}, el('span.wb-spin'), 'Reading your presets…'));
    loadUserPresets().then(() => { if (pl.view === 'presets') renderPlugins(); });
  } else if (userPresets.error) {
    list.append(el('div.wb-empty', { text: 'Could not read your presets: ' + userPresets.error }));
  } else {
    for (const p of userPresets.list) list.append(presetRow(p));
    if (!userPresets.list.length) list.append(el('div.wb-empty', {}, el('div', { text: 'No saved presets yet.' }), el('div.wb-empty-sub', { text: 'Tick plugins under Select, then press Save Preset.' })));
    for (const e of userPresets.errors) list.append(el('div.ps-bad', { text: `${e.file}: ${e.error}` }));
  }
  box.append(list);
}

const osName = os => ({ windows: 'Windows', linux: 'Linux', mac: 'macOS', generic: 'any', any: 'any system', mixed: 'mixed' }[os] || os || 'any');

function presetRow(p) {
  const missing = p.plugins.filter(e => !store.pluginMap.has(e.plugin)).length;
  const names = p.plugins.map(e => shortName(e.plugin) + (e.args && Object.keys(e.args).length ? ' (' + Object.keys(e.args).map(k => '--' + k.replace(/_/g, '-')).join(' ') + ')' : ''));
  const actions = el('div.ps-actions');
  if (!p.builtin) {
    actions.append(el('button.wb-btn.ghost.sm', { type: 'button', text: 'Delete', title: `Delete ${p.id}.json`, on: { click: async e => {
      e.stopPropagation();
      if (!confirm(`Delete the preset “${p.name}”? This removes ~/.fvol/presets/${p.id}.json.`)) return;
      try { await deletePreset(p.id); toast(`Deleted “${p.name}”`); } catch (x) { toast(x.message, 'bad'); }
      await loadUserPresets();
      renderPlugins();
    } } }));
  }
  const row = el('div.ps-row', { role: 'button', tabindex: 0, title: 'Select these plugins' },
    el('div.ps-text', {},
      el('div.ps-head', {}, el('span.ps-name', { text: p.name }), el('span.ps-os', { text: osName(p.os) }),
        el('span.ps-n', { text: p.plugins.length === 1 ? '1 plugin' : `${p.plugins.length} plugins` })),
      el('div.ps-plugins', { text: names.join(', ') }),
      missing ? el('div.ps-bad', { text: `${missing} plugin(s) not in this build will be skipped.` }) : null),
    actions);
  const use = () => {
    pl.sel.clear();
    for (const e of p.plugins) {
      if (!store.pluginMap.has(e.plugin)) continue;
      const vals = {};
      for (const [k, v] of Object.entries(e.args || {})) vals[k] = Array.isArray(v) ? v.join(' ') : v;
      pl.sel.set(e.plugin, vals);
    }
    pl.fromPreset = p.name;
    pl.view = 'select';
    renderPlugins();
    toast(`Selected ${pl.sel.size} plugin${pl.sel.size === 1 ? '' : 's'} from “${p.name}”`);
  };
  row.addEventListener('click', use);
  row.addEventListener('keydown', e => { if (e.target !== row) return; if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); use(); } });
  return row;
}

/** Save Preset: a name, and exactly what will be saved. */
function savePresetDialog() {
  if (!pl.sel.size || document.querySelector('.wb-dialog')) return;
  const oses = new Set([...pl.sel.keys()].map(pluginOs));
  const os = oses.size === 1 ? [...oses][0] : 'mixed';
  const name = el('input.wb-input', { type: 'text', maxlength: 80, placeholder: 'e.g. Ransomware sweep', spellcheck: false, autocomplete: 'off', 'aria-label': 'Preset name' });
  const errBox = el('div.dl-err', { role: 'alert' });
  const saveBtn = el('button.wb-btn.primary', { type: 'button', text: 'Save' });
  const cancel = el('button.wb-btn.ghost', { type: 'button', text: 'Cancel' });
  const items = el('ul.dl-items');
  for (const [plugin, args] of pl.sel) {
    const opts = Object.entries(args).map(([k, v]) => '--' + k.replace(/_/g, '-') + (v === true ? '' : ' ' + v)).join(' ');
    items.append(el('li', {}, el('span.dl-p', { text: shortName(plugin) }), opts ? el('code', { text: opts }) : null));
  }
  const scrim = el('div.wb-scrim');
  const box = el('div.wb-dialog', { role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'dl-h' },
    el('h2#dl-h', { text: 'Save Preset' }),
    el('label.dl-field', {}, el('span', { text: 'Name' }), name),
    el('div.dl-field', {}, el('span', { text: `${pl.sel.size} plugin${pl.sel.size === 1 ? '' : 's'} · ${osName(os)}` }), items),
    errBox,
    el('div.dl-actions', {}, cancel, saveBtn));
  const close = () => { scrim.remove(); box.remove(); removeEventListener('keydown', onKey, true); };
  const onKey = e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); } if (e.key === 'Enter' && e.target.tagName === 'INPUT') { e.preventDefault(); save(false); } };
  async function save(overwrite) {
    errBox.replaceChildren();
    if (!name.value.trim()) { errBox.textContent = 'Give the preset a name.'; name.focus(); return; }
    saveBtn.disabled = true;
    try {
      const p = await savePreset({ name: name.value.trim(), os, selection: pl.sel, overwrite });
      close();
      toast(`Saved “${p.name}” to ~/.fvol/presets/${p.id}.json`);
      await loadUserPresets();
      if (pl.view === 'presets') renderPlugins();
    } catch (e) {
      if (e.status === 409) errBox.append(e.message + '. ', el('button.dl-link', { type: 'button', text: 'Replace it', on: { click: () => save(true) } }));
      else errBox.textContent = e.message;
    }
    saveBtn.disabled = false;
  }
  saveBtn.addEventListener('click', () => save(false));
  cancel.addEventListener('click', close);
  scrim.addEventListener('click', close);
  addEventListener('keydown', onKey, true);
  document.body.append(scrim, box);
  name.focus();
}

/** Save Preset and Run in the Plugins header: shown once a plugin is ticked. */
function updateActions() {
  const n = pl.sel.size;
  $('pl-save').hidden = n === 0;
  $('pl-run').hidden = n === 0;
  $('pl-run-label').textContent = n > 1 ? `Run ${n}` : 'Run';
  $('pl-run').title = [...pl.sel.keys()].join('\n');
}

/** Run: every ticked plugin, with its options, starts together as one run. */
async function startSelected() {
  const btn = $('pl-run');
  btn.disabled = true;
  try {
    const entries = [...pl.sel].map(([plugin, args]) => ({ plugin, args }));
    const b = await api('batches', { method: 'POST', body: { name: pl.fromPreset || undefined, entries } });
    if (!store.batches.some(x => x.id === b.id)) store.batches.push(b);
    runsUi.open.add(b.id);
    pl.sel.clear();
    pl.fromPreset = null;
    renderPlugins();
    renderRuns();
    openResults(b.id);
  } catch (e) { toast(e.message, 'bad'); }   // nothing started: fix the option and press Run again
  btn.disabled = false;
}

// ------------------------------------------------------------------ runs
// Each run is a batch of plugin executions started together. A run expands to its plugins,
// each with its status, rows and time. The name can be changed by clicking it.

const runsUi = { open: new Set(), renaming: null, timer: null };

const ACTIVE = st => st === 'queued' || st === 'running';
const clock = ms => { const t = Math.max(0, Math.round(ms / 1000)); return t < 3600 ? `${Math.floor(t / 60)}:${String(t % 60).padStart(2, '0')}` : `${Math.floor(t / 3600)}:${String(Math.floor(t / 60) % 60).padStart(2, '0')}:${String(t % 60).padStart(2, '0')}`; };

function batchState(b) {
  const runs = b.runs.map(id => store.runs.get(id)).filter(Boolean);
  const count = st => runs.filter(r => r.status === st).length;
  const active = runs.filter(r => ACTIVE(r.status)).length;
  const failed = count('failed'), cancelled = count('cancelled'), done = count('done');
  const status = active ? 'running' : failed ? 'failed' : cancelled && !done ? 'cancelled' : 'done';
  const ends = runs.filter(r => r.started).map(r => r.started + r.elapsed);
  const elapsed = active ? Date.now() - b.created : ends.length ? Math.max(...ends) - b.created : 0;
  return { runs, active, failed, finished: runs.length - active, total: b.runs.length, status, elapsed };
}

export function renderRuns() {
  const box = $('runs');
  const batches = sessionBatches();
  $('rn-count').textContent = batches.length ? String(batches.length) : '';
  const focused = document.activeElement && box.contains(document.activeElement) ? document.activeElement.dataset.key : null;
  clear(box);
  if (!batches.length) {
    box.append(el('div.wb-empty', {}, el('div', { text: 'No runs yet.' }), el('div.wb-empty-sub', { text: 'Tick plugins in the middle and press Run.' })));
  } else {
    const list = el('ol.rn-list');
    for (const b of batches) list.append(batchItem(b));
    box.append(list);
  }
  if (focused) { const f = box.querySelector(`[data-key="${focused}"]`); if (f) f.focus({ preventScroll: true }); }
  // tick the clocks while anything runs
  const anyActive = batches.some(b => batchState(b).active);
  if (anyActive && !runsUi.timer) runsUi.timer = setInterval(() => { if (!runsUi.renaming) renderRuns(); }, 1000);
  if (!anyActive && runsUi.timer) { clearInterval(runsUi.timer); runsUi.timer = null; }
}

function batchItem(b) {
  const st = batchState(b);
  const open = runsUi.open.has(b.id);
  const li = el('li.rn-batch', { class: 'rn-batch ' + st.status + (open ? ' open' : '') + (openBatchId() === b.id ? ' shown' : '') });
  const toggle = () => { if (open) runsUi.open.delete(b.id); else runsUi.open.add(b.id); renderRuns(); };
  // a click on the block: expand and show the results, or collapse when already expanded
  const show = () => { if (open) { runsUi.open.delete(b.id); renderRuns(); return; } runsUi.open.add(b.id); openResults(b.id); renderRuns(); };
  let name;
  if (runsUi.renaming === b.id) {
    name = el('input.rn-rename', { type: 'text', value: b.name, maxlength: 80, 'aria-label': 'Run name', spellcheck: false });
    const finish = async save => {
      if (runsUi.renaming !== b.id) return;
      runsUi.renaming = null;
      const v = name.value.trim();
      if (save && v && v !== b.name) {
        try { const nb = await api(`batches/${b.id}/name`, { method: 'POST', body: { name: v } }); b.name = nb.name; } catch (e) { toast(e.message, 'bad'); }
      }
      renderRuns();
    };
    name.addEventListener('keydown', e => { e.stopPropagation(); if (e.key === 'Enter') finish(true); if (e.key === 'Escape') finish(false); });
    name.addEventListener('blur', () => finish(true));
    name.addEventListener('click', e => e.stopPropagation());
    setTimeout(() => { name.focus(); name.select(); }, 0);
  } else {
    name = el('button.rn-bname', { type: 'button', text: b.name, title: 'Rename', dataset: { key: `b${b.id}n` }, on: { click: e => { e.stopPropagation(); runsUi.renaming = b.id; renderRuns(); } } });
  }
  const action = st.active
    ? el('button.rn-act', { type: 'button', text: 'Cancel', title: 'Cancel the plugins still queued or running', dataset: { key: `b${b.id}a` }, on: { click: async e => { e.stopPropagation(); try { await api(`batches/${b.id}/cancel`, { method: 'POST' }); } catch (x) { toast(x.message, 'bad'); } } } })
    : el('button.rn-act', { type: 'button', text: 'Remove', title: 'Remove this run and its results', dataset: { key: `b${b.id}a` }, on: { click: async e => {
      e.stopPropagation();
      if (!confirm(`Remove “${b.name}” and its results?`)) return;
      try { await api(`batches/${b.id}`, { method: 'DELETE' }); store.batches = store.batches.filter(x => x.id !== b.id); renderRuns(); } catch (x) { toast(x.message, 'bad'); }
    } } });
  const head = el('div.rn-head', { tabindex: 0, role: 'button', 'aria-expanded': String(open), dataset: { key: 'b' + b.id } },
    el('span.rn-caret', { 'aria-hidden': 'true', title: open ? 'Collapse' : 'Expand', on: { click: e => { e.stopPropagation(); toggle(); } } }),
    name,
    el('span.rn-prog', { title: `${st.finished} of ${st.total} plugins finished${st.failed ? `, ${st.failed} failed` : ''}` },
      el('span', { class: 'rn-state ' + st.status, 'aria-hidden': 'true' }), `${st.finished}/${st.total}`),
    el('span.rn-time', { text: clock(st.elapsed) }),
    action);
  head.addEventListener('click', show);
  head.addEventListener('keydown', e => { if (e.target !== head) return; if (e.key === 'Enter') { e.preventDefault(); show(); } if (e.key === ' ' || e.key === 'ArrowRight' || e.key === 'ArrowLeft') { e.preventDefault(); toggle(); } if (e.key === 'F2') { runsUi.renaming = b.id; renderRuns(); } });
  li.append(head);
  if (open) {
    const ul = el('ul.rn-entries');
    for (const id of b.runs) {
      const r = store.runs.get(id);
      if (!r) continue;
      const meta = r.status === 'done' ? `${fmtCount(r.rows)} rows` : r.status === 'running' ? `running ${clock(r.started ? Date.now() - r.started : 0)}` : r.status === 'queued' ? 'queued' : r.status;
      const row = el('li.rn-entry', { title: `${r.plugin} ${r.args.join(' ')}` + (r.error && r.error.message ? '\n' + r.error.message : '') },
        el('span', { class: 'rn-dot ' + r.status }),
        el('span.rn-name', {}, shortName(r.plugin), r.args.length ? el('span.rn-args', { text: ' ' + r.args.join(' ') }) : null),
        el('span', { class: 'rn-meta ' + r.status, text: meta }),
        ACTIVE(r.status) ? el('button.rn-act', { type: 'button', text: 'Cancel', title: 'Cancel this plugin', dataset: { key: `r${r.id}c` }, on: { click: async e => { e.stopPropagation(); try { await api(`runs/${r.id}/cancel`, { method: 'POST' }); } catch (x) { toast(x.message, 'bad'); } } } }) : el('span'));
      row.addEventListener('click', () => { openResults(b.id, r.id); renderRuns(); });
      if (openBatchId() === b.id) row.classList.add('pick');
      ul.append(row);
    }
    li.append(ul);
  }
  return li;
}

// ------------------------------------------------------------------ triage hints

/** The header control: "Load rules…", or the loaded file with a way to replace or remove it. */
function renderRulesControl() {
  const box = $('tr-rules');
  clear(box);
  const file = el('input', { type: 'file', accept: '.json,application/json', hidden: true });
  file.addEventListener('change', async () => {
    const f = file.files && file.files[0];
    if (!f) return;
    if (f.size > 1 << 20) { toast(`${f.name}: a rules file can be at most 1 MiB`, 'bad'); return; }
    try {
      const r = setRules(await f.text(), f.name);
      toast(`Loaded ${r.rules.length} rule${r.rules.length === 1 ? '' : 's'} from ${f.name}`);
    } catch (e) { toast(`${f.name}: ${e.message}`, 'bad'); }
    renderRulesControl();
    renderTriage();
  });
  const pick = () => file.click();
  const r = currentRules();
  if (!r) {
    box.append(file, el('button.wb-btn.outline.sm', { type: 'button', text: 'Load Rules…', title: 'Add your own triage rules from a JSON file', on: { click: pick } }));
    return;
  }
  box.append(file, el('span.tr-loaded', {},
    el('button.tr-file', { type: 'button', title: `${r.file}: ${r.rules.length} rules\nClick to load another file`, text: r.name, on: { click: pick } }),
    el('button.tr-x', { type: 'button', 'aria-label': 'Remove these rules', title: 'Remove these rules', text: '×', on: { click: () => { clearRules(); renderRulesControl(); renderTriage(); } } })));
}

function triageItem(g) {
  const n = g.list.length;
  const chips = el('div.tr-chips');
  for (const p of g.list.slice(0, 12)) chips.append(el('span.tr-chip', { title: p.flags.join('\n') || p.name, text: `${p.name} ${p.pid}` }));
  if (n > 12) chips.append(el('span.tr-chip.more', { text: `+${n - 12}` }));
  return el('div.tr-item', { class: 'tr-item' + (g.hot && n ? ' hot' : '') + (n ? '' : ' zero') },
    el('div.tr-n', { text: String(n) }),
    el('div.tr-body', {},
      el('div.tr-t', {}, g.title, g.rule ? el('span', { class: 'tr-sev ' + g.severity, text: g.severity }) : null),
      g.sub ? el('div.tr-s', { text: g.sub }) : null, n ? chips : null));
}

export function renderTriage() {
  const box = $('triage');
  const s = store.session;
  clear(box);
  if (!s || !s.image) { box.append(el('div.wb-empty', { text: 'Open a memory image to see hints.' })); return; }
  if (s.state === 'failed') { box.append(el('div.wb-empty', { text: 'The kernel was not identified, so there is nothing to triage.' })); return; }
  if (store.procError) { box.append(el('div.wb-empty', { text: 'No process list: ' + store.procError })); return; }
  if (!store.procs) { box.append(el('div.wb-empty', {}, el('span.wb-spin'), s.state === 'warming' ? 'Preparing the image…' : 'Reading the process list…')); return; }
  const r = currentRules();
  if (r) {
    box.append(el('div.tr-sec', { text: r.name }));
    for (const g of ruleGroups(store.procs)) box.append(triageItem(g));
    box.append(el('div.tr-sec', { text: 'Built-in' }));
  }
  for (const g of triageGroups()) box.append(triageItem(g));
}

export function renderAll() {
  renderRulesControl();
  refreshRules();
  renderTopbar();
  renderOverview();
  renderPlugins();
  renderRuns();
  renderTriage();
}

/** The open analysis's rules (they change when another dump is opened). */
function refreshRules() { loadSavedRules().then(() => { renderRulesControl(); renderTriage(); }); }

export function wire() {
  $('pl-tab-presets').addEventListener('click', () => { pl.view = 'presets'; renderPlugins(); });
  $('pl-tab-select').addEventListener('click', () => { pl.view = 'select'; renderPlugins(); });
  $('pl-run').addEventListener('click', startSelected);
  $('pl-save').addEventListener('click', savePresetDialog);
  let sid = store.session && store.session.id;
  let notice = null;
  const tellNotice = () => { const n = store.session && store.session.notice; if (n && n !== notice) toast(n, 'bad'); notice = n; };
  tellNotice();
  on('session', () => { renderTopbar(); renderOverview(); renderTriage(); renderRuns(); tellNotice(); if (store.session.id !== sid) { sid = store.session.id; refreshRules(); } });
  on('procs', () => { renderTopbar(); renderOverview(); renderTriage(); });
  on('runs', () => { if (!runsUi.renaming) renderRuns(); });
  on('batches', () => { if (!runsUi.renaming) renderRuns(); });
  document.addEventListener('fastvol-results', () => renderRuns());
}

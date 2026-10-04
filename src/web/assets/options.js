// Options: every global fvol option, ticked on or off, with a short note on what it does. Saved
// with the analysis on the server; options that decide how the image is opened reopen it.

import { api, el, toast } from './core.js';

// kind: flag (on/off), text, list (split by `sep`), lines (one per line), number, choice
const GROUPS = [
  ['Opening the image', 'Changing these reopens the image (quick: the caches stay warm).', [
    { key: 'symbol_dirs', flag: '-s, --symbol-dirs', kind: 'list', sep: ';', hint: 'folder1;folder2', note: 'Extra folders to look for symbol tables in.' },
    { key: 'offline', flag: '--offline', kind: 'flag', note: 'Never download symbol tables.' },
    { key: 'remote_isf_url', flag: '-u, --remote-isf-url', kind: 'text', hint: 'https://…', note: 'Look symbol tables up in this online index.' },
    { key: 'cache_path', flag: '--cache-path', kind: 'text', hint: '/path/to/folder', note: "Use another folder for python volatility3's cache." },
    { key: 'clear_cache', flag: '--clear-cache', kind: 'flag', note: "Empty fastvol's caches once, when the image reopens." },
    { key: 'single_location', flag: '--single-location', kind: 'text', hint: 'file:///… or https://…', note: 'Open this location instead of the image file.' },
    { key: 'stackers', flag: '--stackers', kind: 'list', sep: ' ', hint: 'LayerStacker names', note: 'Use only these layer stackers.' },
    { key: 'single_swap_locations', flag: '--single-swap-locations', kind: 'list', sep: ' ', hint: 'file:///…/pagefile.sys', note: 'Swap files (e.g. pagefile.sys) to read with the image.' },
    { key: 'verbosity', flag: '-v, --verbosity', kind: 'number', min: 1, max: 5, hint: '1', note: "More detail in the server's log output." },
    { key: 'output_dir', flag: '-o, --output-dir', kind: 'text', hint: '/path/to/folder', note: 'Folder for files plugins write.' },
  ]],
  ['Each run', 'Applied to plugins started from now on.', [
    { key: 'config', flag: '-c, --config', kind: 'text', hint: '/path/to/config.json', note: 'Take plugin option values from this JSON file.' },
    { key: 'extend', flag: '-e, --extend', kind: 'lines', hint: 'plugins.PsList.pid=[4]', note: 'Set a configuration value (one per line).' },
    { key: 'write_config', flag: '--write-config', kind: 'flag', note: "Write each run's configuration to config.json in its output folder." },
    { key: 'save_config', flag: '--save-config', kind: 'text', hint: 'file name', note: 'Same, with this file name.' },
    { key: 'log', flag: '-l, --log', kind: 'text', hint: '/path/to/fvol.log', note: 'Log every run to this file.' },
    { key: 'parallelism', flag: '--parallelism', kind: 'choice', choices: ['processes', 'threads', 'off'], note: '"off" runs one plugin at a time.' },
  ]],
  ['fvol output export', 'The "fvol output" export: exactly what the command line prints.', [
    { key: 'renderer', flag: '-r, --renderer', kind: 'choice', choices: ['quick', 'none', 'csv', 'pretty', 'json', 'jsonl', 'mermaid'], note: 'Output format.' },
    { key: 'filters', flag: '--filters', kind: 'lines', hint: '+ImageFileName,svchost', note: 'Keep or drop rows: [+-]column,pattern[!] (one per line).' },
    { key: 'hide_columns', flag: '--hide-columns', kind: 'list', sep: ' ', hint: 'Offset Handles', note: 'Leave out columns starting with these names.' },
  ]],
  ['No effect in the web UI', 'Accepted so a saved analysis records them.', [
    { key: 'quiet', flag: '-q, --quiet', kind: 'flag', note: 'The web UI shows no console progress to remove.' },
    { key: 'plugin_dirs', flag: '-p, --plugin-dirs', kind: 'text', hint: 'folder1;folder2', note: 'fastvol does not load python plugins.' },
  ]],
];

const isSet = (o, v) => o.kind === 'flag' ? v === true : o.kind === 'number' ? v > 0 : Array.isArray(v) ? v.length > 0 : v !== null && v !== undefined && v !== '';
const shown = (o, v) => Array.isArray(v) ? v.join(o.kind === 'lines' ? '\n' : o.sep === ';' ? ';' : ' ') : v ?? '';

/** Open the Options dialog. */
export async function showOptions() {
  if (document.querySelector('.wb-dialog')) return;
  let cur;
  try { cur = await api('options'); } catch (e) { toast('Could not read the options: ' + e.message, 'bad'); return; }
  const rows = [];
  const body = el('div.op-body');
  for (const [title, sub, list] of GROUPS) {
    body.append(el('div.op-group', {}, el('h3', { text: title }), el('p', { text: sub })));
    for (const o of list) {
      const v = cur[o.key];
      const id = 'op-' + o.key;
      const on = el('input.wb-check', { id, type: 'checkbox', checked: isSet(o, v) });
      let input = null;
      if (o.kind === 'choice') {
        input = el('select.wb-select', {}, ...o.choices.map(c => el('option', { value: c, text: c })));
        if (v) input.value = v;
      } else if (o.kind === 'lines') {
        input = el('textarea.wb-input.op-lines', { rows: 2, spellcheck: false, placeholder: o.hint });
        input.value = shown(o, v);
      } else if (o.kind === 'number') {
        input = el('input.wb-input.op-num', { type: 'number', min: o.min, max: o.max, value: v > 0 ? v : 1 });
      } else if (o.kind !== 'flag') {
        input = el('input.wb-input', { type: 'text', spellcheck: false, autocomplete: 'off', placeholder: o.hint, value: shown(o, v) });
      }
      const sync = () => row.classList.toggle('on', on.checked);
      // the flag and its note toggle the option; the value box stays usable, and typing ticks it
      const row = el('div.op-row', {}, on, el('label.op-text', { for: id }, el('code', { text: o.flag }), el('span.op-note', { text: o.note })), input || el('span'));
      if (input) {
        input.setAttribute('aria-label', o.flag + ' value');
        const tick = () => { if (!on.checked) { on.checked = true; sync(); } };
        input.addEventListener('input', tick);
        input.addEventListener('change', tick);
      }
      on.addEventListener('change', sync);
      sync();
      rows.push({ o, on, input });
      body.append(row);
    }
  }
  const errBox = el('div.dl-err', { role: 'alert' });
  const save = el('button.wb-btn.primary', { type: 'button', text: 'Save' });
  const cancel = el('button.wb-btn.ghost', { type: 'button', text: 'Cancel' });
  const scrim = el('div.wb-scrim');
  const box = el('div.wb-dialog.op-dialog', { role: 'dialog', 'aria-modal': 'true', 'aria-labelledby': 'op-h' },
    el('div.op-head', {}, el('h2#op-h', { text: 'Options' }), el('span.op-sub', { text: 'The fvol command-line options. Saved with this analysis.' })),
    body, errBox, el('div.dl-actions', {}, cancel, save));
  const close = () => { scrim.remove(); box.remove(); removeEventListener('keydown', onKey, true); };
  const onKey = e => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); } };
  cancel.addEventListener('click', close);
  scrim.addEventListener('click', close);
  save.addEventListener('click', async () => {
    const out = {};
    for (const { o, on, input } of rows) {
      if (!on.checked) { out[o.key] = o.kind === 'flag' ? false : o.kind === 'number' ? 0 : null; continue; }
      if (o.kind === 'flag') out[o.key] = true;
      else if (o.kind === 'number') out[o.key] = Math.max(o.min, Math.min(o.max, Number(input.value) || 1));
      else if (o.kind === 'list') out[o.key] = input.value.split(o.sep === ';' ? ';' : /\s+/).map(x => x.trim()).filter(Boolean);
      else if (o.kind === 'lines') out[o.key] = input.value.split('\n').map(x => x.trim()).filter(Boolean);
      else out[o.key] = input.value.trim() || null;
    }
    save.disabled = true;
    errBox.textContent = '';
    try {
      const r = await api('options', { method: 'POST', body: out });
      close();
      toast(r.reopened ? 'Options saved: reopening the image with them' : 'Options saved');
    } catch (e) { errBox.textContent = e.message; }
    save.disabled = false;
  });
  addEventListener('keydown', onKey, true);
  document.body.append(scrim, box);
  save.focus();
}

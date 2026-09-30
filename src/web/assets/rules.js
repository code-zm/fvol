// Triage rules: your own "worth a look" checks from a JSON file, matched in the browser against
// the process list. The format is in docs/web-ui.md (Triage rules).

import { api, toast } from './core.js';
import { captureTime } from './procdata.js';

export const FIELDS = {
  name: p => p.name,
  pid: p => p.pid,
  ppid: p => p.ppid,
  parent: p => (p.parent ? p.parent.name : null),
  path: p => p.path ?? null,
  cmdline: p => p.cmd ?? null,
  threads: p => num(p.threads),
  handles: p => num(p.handles),
  session: p => num(p.session),
  wow64: p => (p.wow64 === null || p.wow64 === undefined ? null : String(p.wow64).toLowerCase() === 'true'),
  exited: p => p.exit !== null,
  age: p => { const cap = captureTime(); return cap && p.create ? cap - p.create : null; },
};
const SEVERITIES = ['high', 'medium', 'low'];

function num(v) {
  if (v === null || v === undefined) return null;
  const n = typeof v === 'number' ? v : Number(String(v).trim());
  return Number.isFinite(n) ? n : null;
}
function int0(s) {
  const t = s.trim().toLowerCase();
  const n = /^-?0x[0-9a-f]+$/.test(t) ? parseInt(t, 16) : /^-?\d+(\.\d+)?$/.test(t) ? Number(t) : NaN;
  return Number.isFinite(n) ? n : null;
}

/** Compile one match value into a test function; throws with a readable message. */
function matcher(field, spec) {
  if (typeof spec === 'boolean') return v => v === spec;
  if (typeof spec === 'number') return v => v === spec;
  if (typeof spec !== 'string') throw new Error(`"${field}" must be text, a number or true/false`);
  const re = /^\/(.*)\/([a-z]*)$/s.exec(spec);
  if (re) {
    let rx;
    try { rx = new RegExp(re[1], re[2]); } catch (e) { throw new Error(`"${field}": bad regular expression: ${e.message}`); }
    return v => v !== null && rx.test(String(v));
  }
  const cmp = /^(>=|<=|>|<|=)\s*(.+)$/.exec(spec);
  if (cmp && int0(cmp[2]) !== null) {
    const n = int0(cmp[2]);
    const op = { '>': (a, b) => a > b, '<': (a, b) => a < b, '>=': (a, b) => a >= b, '<=': (a, b) => a <= b, '=': (a, b) => a === b }[cmp[1]];
    return v => typeof v === 'number' && op(v, n);
  }
  if (spec.startsWith('=')) { const t = spec.slice(1).toLowerCase(); return v => v !== null && String(v).toLowerCase() === t; }
  if (spec.startsWith('!')) { const t = spec.slice(1).toLowerCase(); return v => v === null || !String(v).toLowerCase().includes(t); }
  const t = spec.toLowerCase();
  return v => v !== null && String(v).toLowerCase().includes(t);
}

/** Parse and check a rules file. Returns { name, rules: [{ title, description, severity, test }] }. */
export function compileRules(text, fileName) {
  let j;
  try { j = JSON.parse(text); } catch (e) { throw new Error(`not valid JSON: ${e.message}`); }
  const list = Array.isArray(j) ? j : j && Array.isArray(j.rules) ? j.rules : null;
  if (!list) throw new Error('expected {"rules": [ ... ]}');
  const rules = list.map((r, i) => {
    const where = `rule ${i + 1}${r && r.title ? ` ("${r.title}")` : ''}`;
    if (!r || typeof r !== 'object') throw new Error(`${where}: must be an object`);
    if (typeof r.title !== 'string' || !r.title.trim()) throw new Error(`${where}: needs a "title"`);
    if (!r.match || typeof r.match !== 'object' || !Object.keys(r.match).length) throw new Error(`${where}: needs a "match" with at least one field`);
    const sev = r.severity === undefined ? 'medium' : r.severity;
    if (!SEVERITIES.includes(sev)) throw new Error(`${where}: "severity" must be ${SEVERITIES.join(', ')}`);
    const tests = Object.entries(r.match).map(([f, spec]) => {
      if (!FIELDS[f]) throw new Error(`${where}: unknown field "${f}" (use ${Object.keys(FIELDS).join(', ')})`);
      try { const m = matcher(f, spec); return p => m(FIELDS[f](p)); } catch (e) { throw new Error(`${where}: ${e.message}`); }
    });
    return { title: r.title.trim(), description: typeof r.description === 'string' ? r.description : '', severity: sev, test: p => tests.every(t => t(p)) };
  });
  return { name: (j && typeof j.name === 'string' && j.name.trim()) || fileName, file: fileName, rules };
}

// the loaded rules; the server keeps the file with the saved analysis (~/.fvol/<dump_id>-metadata.json)
let loaded = null;

/** The rules of the open analysis, from the server. */
export async function loadSavedRules() {
  let saved = null;
  try { saved = await api('rules'); } catch (e) { saved = null; }
  loaded = null;
  if (saved && saved.text) {
    try { loaded = compileRules(saved.text, saved.file); } catch (e) { toast(`${saved.file}: ${e.message}`, 'bad'); }
  }
  return loaded;
}
export function setRules(text, file) {
  loaded = compileRules(text, file);   // throws on a bad file: the old rules stay
  api('rules', { method: 'POST', body: { file, text } }).catch(e => toast('Could not save the rules: ' + e.message, 'bad'));
  return loaded;
}
export function clearRules() {
  loaded = null;
  api('rules', { method: 'DELETE' }).catch(e => toast('Could not remove the rules: ' + e.message, 'bad'));
}
export function currentRules() { return loaded; }

/** Triage groups from the loaded rules, in file order. */
export function ruleGroups(procs) {
  if (!loaded) return [];
  return loaded.rules.map(r => ({ key: 'rule:' + r.title, title: r.title, sub: r.description, hot: r.severity !== 'low', rule: true, severity: r.severity, list: procs.filter(r.test) }));
}

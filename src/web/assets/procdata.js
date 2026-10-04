// The process list of the open image, as data: parents, children and "find evil" flags.
// No DOM here; the triage panel and (later) the process tree both read store.procs.

import { store, emit, runPlugin, whenDone, allRows, cellText, parseTime } from './core.js';
import { PROCLIST, PCOLS, WIN_PARENTS, WIN_SINGLETONS, SUSPICIOUS_CHILDREN } from './catalog.js';

// key: the opening of the image the list belongs to (session id and warm-up), gen: bumped when it
// is dropped, so a list still loading for an earlier image is thrown away
const state = { key: null, gen: 0, loading: null, byPid: new Map(), t0: null, t1: null };

export const firstPlugin = cands => cands.find(n => store.pluginMap.has(n));

function col(cols, names) {
  for (const n of names) { const i = cols.findIndex(c => c.name === n); if (i >= 0) return i; }
  return -1;
}

/** Capture time of the image: SystemTime from the kernel, else the latest process time seen. */
export function captureTime() {
  const s = store.session;
  if (!s) return null;
  const f = (s.facts || []).find(([k]) => k === 'SystemTime');
  return f ? parseTime(f[1]) : store.captureGuess || null;
}

export function relToCapture(t, cap) {
  const span = Math.abs(cap - t);
  if (span < 1) return 'at capture time';
  const txt = span < 60 ? `${Math.round(span)} s` : span < 3600 ? `${Math.round(span / 60)} min` : span < 172800 ? `${(span / 3600).toFixed(1)} h` : `${Math.round(span / 86400)} days`;
  return cap - t > 0 ? `${txt} before capture` : `${txt} after capture`;
}

/** Follow the session: the process list of the image once it is ready. Another image, or the
 * same one reopened with new options, drops the list and loads it again. */
export function syncProcesses() {
  const s = store.session;
  const ready = !!(s && s.state === 'ready' && s.os);
  const key = ready ? `${s.id}/${s.warm_ms}` : null;
  if (key !== state.key) {
    state.key = key;
    state.gen++;
    state.loading = null;
    if (store.procs || store.procError || store.captureGuess) {
      store.procs = null;
      store.procError = null;
      store.captureGuess = null;
      emit('procs', null);
    }
  }
  return ready ? loadProcesses() : Promise.resolve(null);
}

/** Load the process list of the current session once (the server keeps the run). */
export function loadProcesses() {
  const s = store.session;
  if (!s || s.state !== 'ready' || !s.os) return Promise.resolve(null);
  if (state.loading) return state.loading;
  const gen = state.gen;
  state.loading = loadFor(s, gen).catch(e => {
    if (gen === state.gen) { store.procError = e.message; emit('procs', null); }
    return null;
  });
  return state.loading;
}

async function loadFor(s, gen) {
  const name = firstPlugin(PROCLIST[s.os] || []);
  if (!name) throw new Error('No process list plugin for this OS in this build.');
  const run = await runPlugin(name, {}, { reuse: true, origin: 'spine' });
  store.procRun = run.id;
  const done = await whenDone(run.id);
  if (done.status !== 'done') throw new Error((done.error && (done.error.title || done.error.message)) || 'The process list failed.');
  const rows = await allRows(run.id);
  if (gen !== state.gen) return null;   // another image meanwhile
  const ix = Object.fromEntries(Object.entries(PCOLS).map(([k, names]) => [k, col(done.cols, names)]));
  const g = (row, k) => (ix[k] >= 0 ? row[ix[k] + 2] : null);
  const procs = rows.map((row, i) => ({
    i, pid: Number(cellText(g(row, 'pid'))), ppid: Number(cellText(g(row, 'ppid'))), name: cellText(g(row, 'name')),
    create: parseTime(g(row, 'create')), exit: parseTime(g(row, 'exit')), offset: g(row, 'offset'),
    threads: g(row, 'threads'), handles: g(row, 'handles'), session: g(row, 'session'), wow64: g(row, 'wow64'), uid: g(row, 'uid'),
    row, cols: done.cols, flags: [], kids: [], parent: null,
  }));
  link(procs);
  flag(procs, s.os);
  store.procs = procs;
  emit('procs', procs);
  enrich(s, procs);
  return procs;
}

/** Image paths and command lines from pstree (Windows), filled in the background. */
function enrich(s, procs) {
  const name = s.os === 'windows' && firstPlugin(['windows.pstree.PsTree']);
  if (!name) return;
  runPlugin(name, {}, { reuse: true, origin: 'spine' }).then(r => whenDone(r.id)).then(async r => {
    if (r.status !== 'done' || store.procs !== procs) return;
    const rows = await allRows(r.id);
    const pi = col(r.cols, ['PID']), path = col(r.cols, ['Path']), cmd = col(r.cols, ['Cmd']);
    for (const row of rows) {
      const p = state.byPid.get(Number(row[pi + 2]));
      if (!p) continue;
      if (path >= 0 && typeof row[path + 2] === 'string') p.path = row[path + 2];
      if (cmd >= 0 && typeof row[cmd + 2] === 'string') p.cmd = row[cmd + 2];
    }
    emit('procs', procs);
  }).catch(() => {});
}

function isAncestor(a, b) { for (let x = b, n = 0; x && n < 512; x = x.parent, n++) if (x === a) return true; return false; }

function link(procs) {
  state.byPid = new Map();
  for (const p of procs) if (!state.byPid.has(p.pid) || p.exit === null) state.byPid.set(p.pid, p);
  for (const p of procs) {
    const par = p.ppid !== 0 || p.pid === 0 ? state.byPid.get(p.ppid) : null;
    if (par && par !== p && !isAncestor(p, par)) { p.parent = par; par.kids.push(p); }
  }
  const times = procs.flatMap(p => [p.create, p.exit]).filter(t => t !== null && t > 0);
  const cap = captureTime() || (times.length ? Math.max(...times) : null);
  store.captureGuess = cap;
  const starts = procs.map(p => p.create).filter(t => t !== null && t > 0);
  state.t0 = starts.length ? Math.min(...starts) : null;
  state.t1 = cap;
}

/** "Find evil" hints: recent starts; on Windows also unexpected parents, duplicated
 * singletons and shells spawned by services. Heuristics, not verdicts. */
function flag(procs, os) {
  const cap = state.t1, t0 = state.t0;
  const uptime = t0 && cap ? cap - t0 : 0;
  const recentWin = Math.min(3600, Math.max(60, uptime * 0.1));
  for (const p of procs) {
    if (cap && p.create && cap - p.create >= 0 && cap - p.create < recentWin && p.create - t0 > Math.min(300, uptime * 0.3) && p.exit === null) p.flags.push(`started ${relToCapture(p.create, cap)}`);
  }
  if (os !== 'windows') return;
  const lower = n => (n || '').toLowerCase();
  const counts = new Map();
  for (const p of procs) if (p.exit === null) counts.set(lower(p.name), (counts.get(lower(p.name)) || 0) + 1);
  for (const p of procs) {
    const exp = Object.entries(WIN_PARENTS).find(([k]) => lower(k) === lower(p.name));
    if (exp && p.parent && !exp[1].some(n => lower(n) === lower(p.parent.name))) p.flags.push(`unexpected parent ${p.parent.name} (${p.ppid})`);
    if (exp && !p.parent && lower(p.name) === 'svchost.exe') p.flags.push(`parent ${p.ppid} not in the process list`);
    if (WIN_SINGLETONS.some(n => lower(n) === lower(p.name)) && counts.get(lower(p.name)) > 1 && p.exit === null) p.flags.push(`${counts.get(lower(p.name))} instances of a normally unique process`);
    if (SUSPICIOUS_CHILDREN.test(p.name) && p.parent && !/^(explorer|cmd|powershell|pwsh|conhost|windowsterminal|code)\.exe$/i.test(p.parent.name)) p.flags.push(`${p.name} spawned by ${p.parent.name}`);
  }
}

/** The triage groups shown in the Triage hints panel. */
export function triageGroups() {
  const s = store.session, procs = store.procs || [];
  const groups = [
    { key: 'look', title: 'Processes worth a look', sub: s.os === 'windows' ? 'Unexpected parents, duplicated singletons, shells spawned by services.' : 'Heuristic flags.', hot: true, list: procs.filter(p => p.flags.some(f => !f.startsWith('started'))) },
    { key: 'recent', title: 'Started shortly before capture', sub: 'New processes are where an intrusion is most likely to be visible.', hot: true, list: procs.filter(p => p.flags.some(f => f.startsWith('started'))) },
  ];
  // smss.exe and userinit.exe exit by design, so their children are expected orphans
  if (s.os === 'windows') groups.push({ key: 'orphan', title: 'Parent not in the process list', sub: 'Normal for some (parent exited), odd for others.', list: procs.filter(p => !p.parent && p.ppid !== 0 && !/^(System|smss\.exe|Registry|MemCompression|csrss\.exe|wininit\.exe|winlogon\.exe|explorer\.exe)$/i.test(p.name)) });
  groups.push({ key: 'exited', title: 'Exited but still listed', sub: 'Terminated processes whose objects are still in memory.', list: procs.filter(p => p.exit !== null) });
  return groups;
}

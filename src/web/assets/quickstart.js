// Quick Start: the launch screen, like a desktop tool's start dialog. Choose to open a memory
// image, to continue with the image the server already has, to work without one ("go"), or to
// reopen a saved analysis. The workspace stays behind it and appears when a choice is made.

import { store, api, on, el, clear, prefs, fmtBytes, fmtAgo, debounce } from './core.js';

// icons: small inline SVG (not markup from the network, so no CSP concern)
const ICONS = {
  open: '<path d="M3 6.5V17a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8.5a1 1 0 0 0-1-1h-6L8.3 5.6a1 1 0 0 0-.7-.3H4a1 1 0 0 0-1 1.2z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>',
  cont: '<path d="M6 4.5v11l9-5.5z" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round"/>',
  prev: '<circle cx="10" cy="10" r="6.5" fill="none" stroke="currentColor" stroke-width="1.6"/><path d="M10 6.5V10l2.5 1.7" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/>',
  dir: '<path d="M2.5 5.5v9a1 1 0 0 0 1 1h13a1 1 0 0 0 1-1V7.5a1 1 0 0 0-1-1H9.5L8 5a1 1 0 0 0-.7-.3H3.5a1 1 0 0 0-1 .8z" fill="currentColor" opacity=".55"/>',
  file: '<path d="M5 2.5h6.5L15 6v11a.5.5 0 0 1-.5.5h-9.5A.5.5 0 0 1 4.5 17V3a.5.5 0 0 1 .5-.5z" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M11.5 2.5V6H15" fill="none" stroke="currentColor" stroke-width="1.3"/>',
  chip: '<rect x="5" y="5" width="10" height="10" rx="1.5" fill="none" stroke="currentColor" stroke-width="1.5"/><path d="M8 2.5v2.5M12 2.5v2.5M8 15v2.5M12 15v2.5M2.5 8h2.5M2.5 12h2.5M15 8h2.5M15 12h2.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>',
  up: '<path d="M10 15.5v-11M5.5 9 10 4.5 14.5 9" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>',
};

function icon(name, cls) {
  const t = document.createElement('template');
  t.innerHTML = `<svg viewBox="0 0 20 20" aria-hidden="true"${cls ? ` class="${cls}"` : ''}>${ICONS[name]}</svg>`;
  return t.content.firstChild;
}

// what a memory image looks like in a directory listing: a known extension, or simply big
const IMAGE_EXT = /\.(raw|mem|vmem|vmss|vmsn|lime|dmp|img|bin|elf|core|dump|avml|sav|qcow2?|gz|xz|bz2)$/i;
const looksLikeImage = e => !e.dir && (IMAGE_EXT.test(e.name) || e.size >= 64 << 20);

const joinPath = (dir, name) => (dir.endsWith('/') ? dir : dir + '/') + name;
const parentOf = dir => dir.replace(/\/[^/]+\/?$/, '') || '/';

/** Show the Quick Start. Resolves when the user has made a choice and it has closed. */
export function showQuickStart() {
  if (document.querySelector('.qs')) return Promise.resolve();
  return new Promise(resolve => {
    const s = store.session;
    const hasImage = !!(s && s.image);
    const root = el('div.qs', { role: 'dialog', 'aria-modal': 'true', 'aria-label': 'fastvol quick start' });

    const done = () => { root.remove(); removeEventListener('keydown', onKey, true); offSession(); resolve(); };
    // another client (a script, the MCP server) opened an image: show its workspace
    const offSession = on('session', ({ prev, cur }) => { if (prev && cur && prev.id !== cur.id) done(); });

    // ---- left: logo, choices
    const logo = document.querySelector('.brand-logo').cloneNode(true);
    const version = (document.querySelector('meta[name="fastvol-version"]') || {}).content || '';
    const choices = [];
    if (hasImage) choices.push({ id: 'cont', ico: 'cont', title: 'Continue', desc: s.name, render: renderContinue });
    choices.push(
      { id: 'open', ico: 'open', title: 'New analysis', desc: 'Open a memory image', render: renderOpen },
      { id: 'prev', ico: 'prev', title: 'Previous', desc: 'Reopen a saved analysis', render: renderPrev },
    );
    const buttons = choices.map((c, i) => el('button.qs-choice', { type: 'button', 'aria-pressed': 'false', on: { click: () => select(c.id) } },
      el('span.qs-ico', {}, icon(c.ico)),
      el('span', {}, el('div.qs-ct', { text: c.title }), el('div.qs-cd', { text: c.desc, title: c.desc })),
      el('span.qs-key', { text: String(i + 1) })));

    const side = el('div.qs-side', {},
      el('div.qs-brand', {}, logo, el('div.qs-ver', { text: version })),
      el('div.qs-h', { text: 'Start' }),
      el('div.qs-choices', {}, buttons),
      el('div.qs-foot', {}, el('span', { text: 'Esc — straight to the workspace' })));

    const main = el('div.qs-main');
    root.append(el('div.qs-win', {}, side, main));
    document.body.append(root);

    function select(id) {
      const i = choices.findIndex(c => c.id === id);
      buttons.forEach((b, j) => b.setAttribute('aria-pressed', String(i === j)));
      clear(main);
      choices[i].render();
    }

    function onKey(e) {
      if (e.target && e.target.tagName === 'INPUT') { if (e.key === 'Escape') e.target.blur(); return; }
      if (e.key === 'Escape') { e.preventDefault(); done(); return; }
      const n = +e.key;
      if (n >= 1 && n <= choices.length && !e.ctrlKey && !e.altKey && !e.metaKey) { e.preventDefault(); select(choices[n - 1].id); buttons[n - 1].focus(); }
    }
    addEventListener('keydown', onKey, true);

    // ---- right: one panel per choice
    function renderContinue() {
      const F = Object.fromEntries(s.facts || []);
      const rows = [['File', s.image], ['Size', fmtBytes(s.size)]];
      if (s.os) rows.push(['System', [s.os, s.arch, F['Major/Minor'] || ''].filter(Boolean).join(' · ')]);
      rows.push(['State', s.state === 'ready' ? 'analysed, ready' : (s.phase || s.state)]);
      const go = el('button.qs-btn.primary', { type: 'button', text: 'Open workspace', on: { click: done } });
      main.append(
        el('h2.qs-title', { text: 'Continue with ' + s.name }),
        el('p.qs-sub', { text: 'This image was given to fvol serve when it started.' }),
        el('dl.qs-current', {}, rows.map(([k, v]) => [el('dt', { text: k }), el('dd', { text: v, title: v })])),
        el('div.qs-actions', {}, el('span.qs-picked'), go));
      go.focus();
    }

    function renderOpen() {
      let dir = null;
      let picked = null;
      const path = el('input.qs-input', { type: 'text', spellcheck: false, autocomplete: 'off', placeholder: '/cases/host1/memory.raw', 'aria-label': 'Folder or file path' });
      const crumbs = el('nav.qs-crumbs', { 'aria-label': 'Folder' });
      const list = el('div.qs-files', { role: 'listbox', 'aria-label': 'Files' });
      const pickedLabel = el('span.qs-picked', { text: 'Choose a memory image' });
      const errBox = el('span.qs-err', { role: 'alert' });
      const openBtn = el('button.qs-btn.primary', { type: 'button', text: 'Open image', disabled: true, on: { click: () => submit() } });

      function pick(full, row) {
        picked = full;
        for (const r of list.children) r.setAttribute('aria-selected', String(r === row));
        pickedLabel.textContent = full;
        pickedLabel.title = full;
        errBox.textContent = '';
        openBtn.disabled = false;
      }

      async function browse(p) {
        errBox.textContent = '';
        let r;
        try { r = await api('fs?path=' + encodeURIComponent(p || '.')); } catch (e) { errBox.textContent = e.message; return; }
        dir = r.dir;
        prefs.set('qsDir', dir);
        if (document.activeElement !== path) path.value = dir;
        // breadcrumbs
        clear(crumbs);
        const parts = dir.split('/').filter(Boolean);
        crumbs.append(el('button', { type: 'button', text: '/', on: { click: () => browse('/') } }));
        parts.forEach((part, i) => {
          if (i) crumbs.append(el('span', { text: '/' }));
          const to = '/' + parts.slice(0, i + 1).join('/');
          crumbs.append(el('button', { type: 'button', text: part, on: { click: () => browse(to) } }));
        });
        // listing: folders, then memory images, then everything else
        const filter = (!path.value.endsWith('/') && document.activeElement === path) ? path.value.slice(path.value.lastIndexOf('/') + 1).toLowerCase() : '';
        const entries = r.entries.filter(e => !e.name.startsWith('.') && (!filter || e.name.toLowerCase().startsWith(filter)));
        const rank = e => (e.dir ? 0 : looksLikeImage(e) ? 1 : 2);
        entries.sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name));
        clear(list);
        picked = null;
        openBtn.disabled = true;
        pickedLabel.textContent = 'Choose a memory image';
        if (dir !== '/') {
          list.append(el('button.qs-file.dir', { type: 'button', on: { click: () => browse(parentOf(dir)) } },
            el('span.fi', {}, icon('up')), el('span.fn', { text: 'Parent folder' }), el('span'), el('span.fs')));
        }
        for (const e of entries) {
          const full = joinPath(dir, e.name);
          const img = looksLikeImage(e);
          const row = el('button.qs-file', { type: 'button', role: 'option', 'aria-selected': 'false', class: 'qs-file ' + (e.dir ? 'dir' : img ? 'image' : 'other') },
            el('span.fi', {}, icon(e.dir ? 'dir' : img ? 'chip' : 'file')),
            el('span.fn', { text: e.name, title: e.name }),
            img ? el('span.qs-tag', { text: 'image' }) : el('span'),
            el('span.fs', { text: e.dir ? '' : fmtBytes(e.size) }));
          if (e.dir) row.addEventListener('click', () => browse(full));
          else {
            row.addEventListener('click', () => pick(full, row));
            row.addEventListener('dblclick', () => { pick(full, row); submit(); });
          }
          list.append(row);
        }
        if (!entries.length) list.append(el('div.qs-empty', { text: filter ? 'Nothing here starts with “' + filter + '”.' : 'This folder is empty.' }));
      }

      async function submit() {
        const file = picked || path.value.trim();
        if (!file) return;
        openBtn.disabled = true;
        errBox.textContent = '';
        try {
          await api('session', { method: 'POST', body: { file } });
          done();
        } catch (e) { errBox.textContent = e.message; openBtn.disabled = false; }
      }

      // the desktop's own file dialog, opened by the server (browsers never reveal full paths)
      const nativeBtn = el('button.qs-btn.outline', { type: 'button', hidden: true, on: { click: () => nativePick() } }, icon('open', 'btn-ico'), 'Open file…');
      api('pick-file').then(r => { if (r.available) { nativeBtn.hidden = false; nativeBtn.title = 'Choose the image in your desktop’s file dialog (' + r.tool + ')'; } }).catch(() => {});
      async function nativePick() {
        nativeBtn.disabled = true;
        const label = nativeBtn.lastChild;
        label.textContent = 'Choose in the dialog…';
        pickedLabel.textContent = 'The file dialog is open on your desktop — it may be behind this window.';
        errBox.textContent = '';
        try {
          const r = await api('pick-file', { method: 'POST', body: { dir: dir || '' } });
          if (r.path) { picked = r.path; pickedLabel.textContent = r.path; await submit(); }
          else pickedLabel.textContent = 'Choose a memory image';
        } catch (e) { errBox.textContent = e.message; pickedLabel.textContent = ''; }
        label.textContent = 'Open file…';
        nativeBtn.disabled = false;
      }

      path.addEventListener('input', debounce(() => browse(path.value.endsWith('/') ? path.value : path.value.slice(0, path.value.lastIndexOf('/') + 1) || '/'), 150));
      path.addEventListener('keydown', e => {
        if (e.key === 'Enter') { e.preventDefault(); if (path.value.endsWith('/')) browse(path.value); else { picked = null; submit(); } }
        if (e.key === 'ArrowDown') { e.preventDefault(); const f = list.querySelector('.qs-file'); if (f) f.focus(); }
      });
      list.addEventListener('keydown', e => {
        const rows = [...list.querySelectorAll('.qs-file')];
        const i = rows.indexOf(document.activeElement);
        if (e.key === 'ArrowDown' && i < rows.length - 1) { e.preventDefault(); rows[i + 1].focus(); }
        if (e.key === 'ArrowUp') { e.preventDefault(); (i > 0 ? rows[i - 1] : path).focus(); }
        if (e.key === 'Backspace') { e.preventDefault(); browse(parentOf(dir)); }
      });

      main.append(
        el('h2.qs-title', { text: 'Open a memory image' }),
        el('p.qs-sub', { text: 'Raw, LiME, ELF core, crash dump, VMware, QEMU, AVML, Xen - also gzip, bzip2 and xz. The file is only read, never changed.' }),
        el('div.qs-pathrow', {}, path, nativeBtn), crumbs, list,
        el('div.qs-actions', {}, pickedLabel, errBox, openBtn));
      const start = hasImage ? parentOf(s.image) : prefs.get('qsDir', '.');
      browse(start);
      path.focus();
    }

    function renderPrev() {
      const list = el('div.qs-files', { role: 'listbox', 'aria-label': 'Saved analyses' }, el('div.qs-empty', { text: 'Reading saved analyses…' }));
      const errBox = el('span.qs-err', { role: 'alert' });
      main.append(
        el('h2.qs-title', { text: 'Reopen a saved analysis' }),
        el('p.qs-sub', { text: 'Every dump you analyse is saved in ~/.fvol: its runs, results, filters, options and rules come back when you reopen it.' }),
        list, el('div.qs-actions', {}, errBox));
      api('analyses').then(items => {
        clear(list);
        if (!items.length) { list.append(el('div.qs-empty', { text: 'No saved analyses yet: open a memory image and run some plugins.' })); return; }
        for (const a of items) {
          const ok = a.state === 'ok';
          const why = a.state === 'missing' ? 'the dump is no longer at this path' : a.state === 'changed' ? 'the dump changed since (a different size or date)' : '';
          const open = async () => {
            errBox.textContent = '';
            try { await api('session', { method: 'POST', body: { file: a.image } }); done(); }
            catch (e) { errBox.textContent = e.message; }
          };
          const del = el('button.qs-del', { type: 'button', text: 'Delete', title: 'Delete this saved analysis from ~/.fvol', on: { click: async e => {
            e.stopPropagation();
            const current = store.session && store.session.image === a.image;
            if (!confirm(`Delete the saved analysis of ${a.name}?\n\nIts runs, results, filters and options are removed from ~/.fvol. The dump itself is not touched.` + (current ? '\n\nThis dump is open: its runs are removed and it starts over.' : ''))) return;
            try { await api('analyses/' + encodeURIComponent(a.dump_id), { method: 'DELETE' }); row.remove(); if (!list.querySelector('.qs-prev')) list.append(el('div.qs-empty', { text: 'No saved analyses left.' })); }
            catch (x) { errBox.textContent = x.message; }
          } } });
          // a row, not a button: it holds the Delete button
          const row = el('div.qs-file.qs-prev', { role: 'option', tabindex: ok ? 0 : -1, 'aria-disabled': String(!ok), class: 'qs-file qs-prev' + (ok ? '' : ' off'), title: ok ? a.image : `${a.image}: ${why}` },
            el('span.fi', {}, icon('chip')),
            el('span.fn', {}, el('span.qs-pn', { text: a.name }), el('span.qs-pp', { text: a.image })),
            ok ? el('span.qs-pm', { text: `${a.runs} run${a.runs === 1 ? '' : 's'} · ${a.plugins} plugin${a.plugins === 1 ? '' : 's'}` }) : el('span.qs-tag.warn', { text: a.state }),
            el('span.fs', { text: `${fmtBytes(a.size)} · ${fmtAgo(a.updated)}` }),
            del);
          if (ok) {
            row.addEventListener('click', open);
            row.addEventListener('keydown', e => { if (e.key === 'Enter') { e.preventDefault(); open(); } if (e.key === 'Delete') del.click(); });
          }
          list.append(row);
        }
      }).catch(e => { clear(list); list.append(el('div.qs-empty', { text: 'Could not read the saved analyses: ' + e.message })); });
    }

    select(hasImage ? 'cont' : 'open');
  });
}

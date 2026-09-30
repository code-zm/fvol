//! Saved analyses: `~/.fvol/<dump_id>-metadata.json` plus the result rows of each plugin in
//! `~/.fvol/<dump_id>/<run>/<n>.jsonl` (layout in docs/web-ui.md, Saved files). `dump_id` is the
//! key of the image caches in `~/.cache/fastvol`, so a reopened dump finds them warm.

use super::jsonw::{self, W};
use super::runs::{ErrInfo, Run, SavedRun, Status};
use super::table::{K_ABSENT, K_DEC, K_HEX, K_NA, K_TEXT, Table};
use crate::cli::json::{self, Json};
use crate::renderers::{ColType, Column};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

const FORMAT: i128 = 1;
/// Largest metadata file read (runs and their plugins, not their rows).
const MAX_META: u64 = 16 << 20;

/// The dump's identity: `(dump_id, canonical path, size, mtime in ns)`.
pub fn dump_id(image: &Path) -> Option<(String, PathBuf, u64, i128)> {
    let canon = crate::util::paths::canonicalize(image).ok()?;
    let (size, mtime) = crate::util::paths::file_stamp(&canon)?;
    let c = canon.to_string_lossy();
    // the key material of the image caches (automagic/mod.rs `cache::key_for`) without their
    // per-cache kind and format version
    let mut k: Vec<u8> = Vec::with_capacity(c.len() + 32);
    k.extend_from_slice(&(c.len() as u64).to_le_bytes());
    k.extend_from_slice(c.as_bytes());
    k.extend_from_slice(&size.to_le_bytes());
    k.extend_from_slice(&mtime.to_le_bytes());
    Some((format!("{:016x}", crate::layers::scancache::key_hash(&k)), canon, size, mtime))
}

pub fn metadata_path(id: &str) -> PathBuf {
    super::presets::fvol_dir().join(format!("{id}-metadata.json"))
}

fn results_dir(id: &str) -> PathBuf {
    super::presets::fvol_dir().join(id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Write `data` to `path` through a temporary file, readable by the user only.
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent"))?;
    super::presets::ensure_private_dir(dir)?;
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), std::process::id()));
    let r = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).and_then(|mut f| f.write_all(data)).and_then(|_| std::fs::rename(&tmp, path));
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

// ------------------------------------------------------------------------------------------
// result rows (sidecars)

pub fn coltype_from(name: &str) -> ColType {
    match name {
        "Int" => ColType::Int,
        "Bytes" => ColType::Bytes,
        "Float" => ColType::Float,
        "Bool" => ColType::Bool,
        "DateTime" => ColType::DateTime,
        "Hex" => ColType::Hex,
        "Bin" => ColType::Bin,
        "HexBytes" => ColType::HexBytes,
        "MultiTypeData" => ColType::MultiTypeData,
        "Disassembly" => ColType::Disassembly,
        "LayerData" => ColType::LayerData,
        _ => ColType::Str,
    }
}

/// A run's rows as JSON lines: a header, then `[depth, "kinds", cell, ...]` per row, `kinds`
/// holding each cell's kind (text, absent, N/A, decimal, hex) so the table comes back exactly.
pub fn write_rows(path: &Path, plugin: &str, columns: &[Column], t: &Table) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(t.text.len() + t.rows() * 16 + 256);
    let mut h = W::new();
    h.obj().ks("fastvol_results", "1").ks("plugin", plugin).key("columns").arr();
    for c in columns {
        h.obj().ks("name", &c.name).ks("type", super::runs::coltype_name(c.ty)).end_obj();
    }
    h.end_arr().end_obj();
    out.extend_from_slice(&h.done());
    out.push(b'\n');
    for r in 0..t.rows() {
        out.push(b'[');
        out.extend_from_slice(t.depth[r].to_string().as_bytes());
        out.extend_from_slice(b",\"");
        for c in 0..t.ncols {
            out.push(b'0' + t.kind(r, c));
        }
        out.push(b'"');
        for c in 0..t.ncols {
            out.push(b',');
            match t.kind(r, c) {
                K_DEC | K_HEX => out.extend_from_slice(t.value(r, c).unwrap_or(0).to_string().as_bytes()),
                K_TEXT => jsonw::bytes_str(&mut out, t.cell(r, c)),
                _ => out.extend_from_slice(b"null"),
            }
        }
        out.extend_from_slice(b"]\n");
    }
    write_private(path, &out)
}

/// Read rows written by [`write_rows`] into a table of `ncols` columns.
pub fn read_rows(path: &Path, ncols: usize) -> Result<Table, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut t = Table::new(ncols);
    let bad = |n: usize| format!("{}: damaged saved results (line {n})", path.display());
    for (n, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 || line.is_empty() {
            continue;
        }
        let Ok(Json::Arr(v)) = json::parse(&line) else { return Err(bad(n + 1)) };
        let (Some(Json::Int(depth)), Some(Json::Str(kinds))) = (v.first(), v.get(1)) else { return Err(bad(n + 1)) };
        if kinds.len() != ncols || v.len() != ncols + 2 {
            return Err(bad(n + 1));
        }
        for (c, k) in kinds.bytes().enumerate() {
            match (k.wrapping_sub(b'0'), &v[c + 2]) {
                (K_TEXT, Json::Str(s)) => {
                    t.text.extend_from_slice(s.as_bytes());
                    t.push_cell(K_TEXT);
                }
                (K_DEC, Json::Int(i)) => t.push_num(K_DEC, *i as i64 as u64),
                (K_HEX, Json::Int(i)) => t.push_num(K_HEX, *i as u64),
                (K_ABSENT, _) => t.push_cell(K_ABSENT),
                (K_NA, _) => t.push_cell(K_NA),
                _ => return Err(bad(n + 1)),
            }
        }
        t.depth.push((*depth).clamp(0, u16::MAX as i128) as u16);
    }
    Ok(t)
}

// ------------------------------------------------------------------------------------------
// the open analysis

/// The analysis of the open dump.
pub struct Analysis {
    pub id: String,
    pub image: PathBuf,
    pub size: u64,
    pub mtime: i128,
    pub created_ms: u64,
    /// plugin executions whose rows are on disk
    pub saved_rows: HashSet<u64>,
    /// the metadata last written (only a change is written again)
    pub last: Vec<u8>,
}

impl Analysis {
    pub fn new(image: &Path) -> Option<Analysis> {
        let (id, canon, size, mtime) = dump_id(image)?;
        Some(Analysis { id, image: canon, size, mtime, created_ms: super::runs::now_ms(), saved_rows: HashSet::new(), last: Vec::new() })
    }
}

/// The saved metadata of a dump, if there is any.
pub fn load(id: &str) -> Option<Json> {
    let p = metadata_path(id);
    if std::fs::metadata(&p).ok()?.len() > MAX_META {
        return None;
    }
    let j = json::parse(&std::fs::read_to_string(p).ok()?).ok()?;
    matches!(j.get("fastvol_analysis"), Some(Json::Int(FORMAT))).then_some(j)
}

fn sidecar_path(id: &str, batch: u64, n: usize) -> PathBuf {
    results_dir(id).join(batch.to_string()).join(format!("{n}.jsonl"))
}

fn write_error(w: &mut W, e: &Option<ErrInfo>) {
    match e {
        None => {
            w.key("error").null();
        }
        Some(e) => {
            w.key("error").obj().ks("kind", e.kind).ks("title", &e.title).ks("message", &e.message).key("hints").arr();
            for h in &e.hints {
                w.s(h);
            }
            w.end_arr().ks("detail", &e.detail).end_obj();
        }
    }
}

/// Save the open analysis: rows of newly finished plugins, then the metadata when it changed.
pub fn save(app: &super::App) {
    let session = app.session();
    let mut guard = app.analysis.lock().unwrap_or_else(|e| e.into_inner());
    let Some(a) = guard.as_mut() else { return };
    let batches: Vec<_> = app.runs.batches().into_iter().filter(|b| b.session == session.id).collect();
    let mut w = W::new();
    w.obj().ki("fastvol_analysis", FORMAT).ks("dump_id", &a.id).ks("image", &a.image.to_string_lossy());
    w.ks("name", &a.image.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    w.ku("size", a.size).ki("mtime_ns", a.mtime).ku("created", a.created_ms);
    w.key("options");
    app.options.lock().unwrap_or_else(|e| e.into_inner()).write(&mut w);
    match &*app.rules.lock().unwrap_or_else(|e| e.into_inner()) {
        Some((file, text)) => {
            w.key("rules").obj().ks("file", file).ks("text", text).end_obj();
        }
        None => {
            w.key("rules").null();
        }
    }
    let mut keep_dirs: HashSet<String> = HashSet::new();
    w.key("runs").arr();
    for b in &batches {
        keep_dirs.insert(b.id.to_string());
        w.obj().ku("id", b.id).ks("name", &b.name.lock().unwrap_or_else(|e| e.into_inner())).ks("filter", &b.filter.lock().unwrap_or_else(|e| e.into_inner())).ku("created", b.created_ms);
        w.key("entries").arr();
        for (n, rid) in b.runs.iter().enumerate() {
            let Some(run) = app.runs.get(*rid) else { continue };
            let sidecar = sidecar_path(&a.id, b.id, n);
            save_rows(a, &run, &sidecar);
            let d = run.read();
            w.obj().ks("plugin", run.plugin.name());
            w.key("args").raw(run.req_args.lock().unwrap_or_else(|e| e.into_inner()).dump(None).as_bytes());
            w.key("argv").arr();
            for x in &run.args {
                w.s(x);
            }
            w.end_arr();
            // a plugin still going when the server stops comes back as interrupted
            w.ks("status", d.status.as_str()).ku("rows", d.produced).ku("created", run.created_ms).ku("started", d.started_ms).ku("elapsed", d.elapsed_ms);
            w.key("columns").arr();
            for c in &d.columns {
                w.obj().ks("name", &c.name).ks("type", super::runs::coltype_name(c.ty)).end_obj();
            }
            w.end_arr();
            write_error(&mut w, &d.error);
            w.ks("out_dir", &run.out_dir.to_string_lossy());
            match run.sidecar.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|_| a.saved_rows.contains(&run.id)) {
                Some(p) => w.ks("results", &p.to_string_lossy()),
                None => w.key("results").null(),
            };
            w.end_obj();
        }
        w.end_arr().end_obj();
    }
    w.end_arr().end_obj();
    let mut data = w.done();
    data.push(b'\n');
    if data != a.last && write_private(&metadata_path(&a.id), &data).is_ok() {
        a.last = data;
    }
    // results of runs that were removed
    if let Ok(rd) = std::fs::read_dir(results_dir(&a.id)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.bytes().all(|c| c.is_ascii_digit()) && !keep_dirs.contains(&name) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

/// Write a finished plugin's rows once (only complete tables: a truncated or dropped one is not
/// saved, and the metadata says so).
fn save_rows(a: &mut Analysis, run: &Run, path: &Path) {
    if a.saved_rows.contains(&run.id) {
        return;
    }
    let d = run.read();
    if d.status != Status::Done || d.truncated || d.evicted || d.busy || (d.table.rows() as u64) < d.produced {
        return;
    }
    if write_rows(path, run.plugin.name(), &d.columns, &d.table).is_ok() {
        drop(d);
        *run.sidecar.lock().unwrap_or_else(|e| e.into_inner()) = Some(path.to_path_buf());
        a.saved_rows.insert(run.id);
    }
}

/// Bring back the runs of a saved analysis into `session`.
pub fn restore(app: &super::App, session: &std::sync::Arc<super::session::Session>, meta: &Json) {
    let text = |j: &Json, k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let num = |j: &Json, k: &str| match j.get(k) {
        Some(Json::Int(i)) if *i >= 0 => *i as u64,
        _ => 0,
    };
    let mut guard = app.analysis.lock().unwrap_or_else(|e| e.into_inner());
    let Some(a) = guard.as_mut() else { return };
    a.created_ms = num(meta, "created").max(1);
    for b in meta.get("runs").map(|r| r.as_arr()).unwrap_or(&[]) {
        let mut runs = Vec::new();
        for e in b.get("entries").map(|r| r.as_arr()).unwrap_or(&[]) {
            let Some(plugin) = app.plugins.iter().copied().find(|p| Some(p.name()) == e.get("plugin").and_then(|x| x.as_str())) else { continue };
            let req = e.get("args").cloned().unwrap_or(Json::Null);
            let Ok((cfg, argv)) = super::api::parse_config(plugin, Some(&req)) else { continue };
            let key = super::api::config_key(plugin, &cfg);
            let columns: Vec<Column> = e.get("columns").map(|c| c.as_arr()).unwrap_or(&[]).iter().map(|c| Column { name: text(c, "name"), ty: coltype_from(&text(c, "type")) }).collect();
            let (status, error) = match text(e, "status").as_str() {
                "done" => (Status::Done, None),
                "failed" => (Status::Failed, e.get("error").map(read_error)),
                "cancelled" => (Status::Cancelled, e.get("error").map(read_error)),
                _ => (Status::Cancelled, Some(ErrInfo { kind: "cancelled", title: "Interrupted".into(), message: "fvol serve stopped before this plugin finished. Run it again to get its results.".into(), ..Default::default() })),
            };
            let sidecar = e.get("results").and_then(|r| r.as_str()).map(PathBuf::from).filter(|p| p.is_file());
            let saved = SavedRun {
                status,
                rows: num(e, "rows"),
                columns,
                error,
                created_ms: num(e, "created"),
                started_ms: num(e, "started"),
                elapsed_ms: num(e, "elapsed"),
                out_dir: PathBuf::from(text(e, "out_dir")),
                sidecar: sidecar.clone(),
            };
            let run = app.runs.restore(session.clone(), plugin, cfg, argv, key, req, saved);
            if sidecar.is_some() {
                a.saved_rows.insert(run.id);
            }
            runs.push(run);
        }
        let id = num(b, "id");
        if id > 0 {
            app.runs.restore_batch(id, session.id, text(b, "name"), text(b, "filter"), num(b, "created"), &runs);
        }
    }
}

fn read_error(j: &Json) -> ErrInfo {
    let s = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    // the kinds the server itself gives (a static set)
    let kind = ["cancelled", "crash", "error", "io", "layer", "page", "requirement", "symbol", "symbols", "wrong-os"].into_iter().find(|k| *k == s("kind")).unwrap_or("error");
    ErrInfo { kind, title: s("title"), message: s("message"), hints: j.get("hints").map(|h| h.as_arr().iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(), detail: s("detail") }
}

/// Delete a saved analysis from `root` (`~/.fvol`): its metadata and its result rows. The dump
/// itself is never touched.
pub fn delete_in(root: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("no such analysis".into());
    }
    std::fs::remove_file(root.join(format!("{id}-metadata.json"))).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "no such analysis".to_string() } else { e.to_string() })?;
    match std::fs::remove_dir_all(root.join(id)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// Every saved analysis, newest first: for the Quick Start's "Previous" list.
pub fn list(w: &mut W) {
    let mut items: Vec<(u64, Json)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(super::presets::fvol_dir()) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix("-metadata.json").filter(|i| valid_id(i)) else { continue };
            let Some(j) = load(id) else { continue };
            let updated = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            items.push((updated, j));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    w.arr();
    for (updated, j) in &items {
        let text = |k: &str| j.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let image = text("image");
        let (size, mtime) = (j.get("size").cloned(), j.get("mtime_ns").cloned());
        // is the dump still there, unchanged?
        let state = match crate::util::paths::file_stamp(Path::new(&image)) {
            None => "missing",
            Some((s, m)) if Some(Json::Int(s as i128)) == size && Some(Json::Int(m)) == mtime => "ok",
            Some(_) => "changed",
        };
        let runs = j.get("runs").map(|r| r.as_arr()).unwrap_or(&[]);
        let plugins: usize = runs.iter().map(|b| b.get("entries").map(|e| e.as_arr().len()).unwrap_or(0)).sum();
        w.obj().ks("dump_id", &text("dump_id")).ks("image", &image).ks("name", &text("name")).ks("state", state);
        w.key("size").raw(size.unwrap_or(Json::Int(0)).dump(None).as_bytes());
        w.ku("updated", *updated).ku("runs", runs.len() as u64).ku("plugins", plugins as u64).end_obj();
    }
    w.end_arr();
}

/// Save in the background: after any change (every run, rename, filter, option), at most once
/// a second.
pub fn start_saver(app: std::sync::Arc<super::App>) {
    let _ = std::thread::Builder::new().name("analysis-saver".into()).spawn(move || {
        let mut seen = 0u64;
        loop {
            let now = app.hub.wait(seen, std::time::Duration::from_secs(30));
            if now != seen {
                seen = now;
                save(&app);
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_exactly() {
        let cols = vec![Column { name: "PID".into(), ty: ColType::Int }, Column { name: "Offset".into(), ty: ColType::Hex }, Column { name: "Name".into(), ty: ColType::Str }];
        let mut t = Table::new(3);
        t.push_num(K_DEC, (-5i64) as u64);
        t.push_num(K_HEX, u64::MAX);
        t.text.extend_from_slice("sv\"c\\host\n\u{e9}".as_bytes());
        t.push_cell(K_TEXT);
        t.depth.push(0);
        t.push_cell(K_ABSENT);
        t.push_cell(K_NA);
        t.push_cell(K_TEXT);
        t.depth.push(2);
        let p = std::env::temp_dir().join(format!("fastvol-rows-{}.jsonl", std::process::id()));
        write_rows(&p, "windows.pslist.PsList", &cols, &t).unwrap();
        let back = read_rows(&p, 3).unwrap();
        assert_eq!((back.text, back.ends, back.kinds, back.depth), (t.text.clone(), t.ends.clone(), t.kinds.clone(), t.depth.clone()));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&p, "{}\n[0,\"9\",1]\n").unwrap();
        assert!(matches!(read_rows(&p, 1), Err(e) if e.contains("damaged")));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn delete_removes_metadata_and_rows_only() {
        let root = std::env::temp_dir().join(format!("fastvol-analyses-{}", std::process::id()));
        let id = "0123456789abcdef";
        std::fs::create_dir_all(root.join(id).join("1")).unwrap();
        std::fs::write(root.join(format!("{id}-metadata.json")), "{}").unwrap();
        std::fs::write(root.join(id).join("1").join("0.jsonl"), "{}").unwrap();
        std::fs::create_dir_all(root.join("presets")).unwrap();
        assert!(delete_in(&root, id).is_ok());
        assert!(!root.join(format!("{id}-metadata.json")).exists() && !root.join(id).exists());
        assert!(root.join("presets").is_dir());
        assert!(delete_in(&root, id).unwrap_err().contains("no such"));
        assert!(delete_in(&root, "../presets").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dump_id_follows_the_cache_key_material() {
        let p = std::env::temp_dir().join(format!("fastvol-dump-{}.raw", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let (a, ..) = dump_id(&p).unwrap();
        assert!(valid_id(&a));
        assert_eq!(dump_id(&p).unwrap().0, a);
        std::fs::write(&p, b"abcd").unwrap();
        assert_ne!(dump_id(&p).unwrap().0, a);
        let _ = std::fs::remove_file(&p);
    }
}

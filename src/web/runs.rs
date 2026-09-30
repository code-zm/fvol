//! Plugin runs: a registry of runs, a small scheduler (bounded concurrency, FIFO queue), the
//! streaming `RowSink` that renders rows into the run's table as they arrive, cancellation,
//! and plain-language error explanations.

use super::jsonw::W;
use super::session::Session;
use super::table::{K_ABSENT, K_DEC, K_HEX, K_NA, K_TEXT, MAX_TEXT, Table, View};
use crate::error::{Error, Result};
use crate::plugins::{Config, Plugin};
use crate::renderers::{ColType, Column, RowSink, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Change notification for event streams: a global sequence number + condvar.
#[derive(Default)]
pub struct Hub {
    seq: Mutex<u64>,
    cv: Condvar,
}

impl Hub {
    /// Record a change; returns the new sequence number.
    pub fn bump(&self) -> u64 {
        let mut g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        *g += 1;
        self.cv.notify_all();
        *g
    }
    pub fn current(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Wait until the sequence passes `seen` (or the timeout); returns the current sequence.
    pub fn wait(&self, seen: u64, timeout: Duration) -> u64 {
        let g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = self.cv.wait_timeout_while(g, timeout, |s| *s <= seen).unwrap_or_else(|e| e.into_inner());
        *g
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }
    pub fn finished(self) -> bool {
        matches!(self, Status::Done | Status::Failed | Status::Cancelled)
    }
}

/// A failure explained for humans, plus the technical text.
#[derive(Clone, Debug, Default)]
pub struct ErrInfo {
    pub kind: &'static str,
    pub title: String,
    pub message: String,
    pub hints: Vec<String>,
    pub detail: String,
}

pub struct RunData {
    pub status: Status,
    pub columns: Vec<Column>,
    pub types: Vec<ColType>,
    pub table: Table,
    /// rows produced (including rows not stored once the table hit its size cap)
    pub produced: u64,
    pub truncated: bool,
    pub error: Option<ErrInfo>,
    pub started: Option<Instant>,
    /// epoch ms when the plugin started (0 while queued)
    pub started_ms: u64,
    pub elapsed_ms: u64,
    pub files: Vec<(String, u64)>,
    /// hub sequence of the last change
    pub seq: u64,
    /// the worker thread is still inside the plugin (a cancelled run may still be unwinding)
    pub busy: bool,
    /// the table was dropped to make room for newer results (memory budget)
    pub evicted: bool,
}

pub struct Run {
    pub id: u64,
    /// bytes of table storage used by all runs, and the budget for them
    pub used: Arc<AtomicU64>,
    pub budget: u64,
    /// the registry (to evict other runs' tables when memory runs out)
    pub registry: std::sync::Weak<Runs>,
    /// epoch ms of the last time the browser looked at this run's rows
    pub last_access: AtomicU64,
    pub session: Arc<Session>,
    pub plugin: &'static dyn Plugin,
    pub cfg: Config,
    /// the options as `vol` arguments, e.g. ["--pid", "4"]
    pub args: Vec<String>,
    /// cache key: plugin + canonical config
    pub key: String,
    pub origin: String,
    /// the batch (a user's "run" of several plugins) this execution belongs to; 0 = none
    pub batch: AtomicU64,
    /// the options as the browser gave them (JSON), so a saved analysis can rebuild the run
    pub req_args: Mutex<crate::cli::json::Json>,
    /// the saved rows of a finished run (`~/.fvol/<dump_id>/...jsonl`), read back on first use
    pub sidecar: Mutex<Option<PathBuf>>,
    pub created_ms: u64,
    pub out_dir: PathBuf,
    pub cancel: AtomicBool,
    pub data: RwLock<RunData>,
    pub views: Mutex<Vec<(u64, String, Arc<View>)>>,
    pub next_view: AtomicU64,
}

impl Drop for Run {
    fn drop(&mut self) {
        let b = self.data.get_mut().map(|d| d.table.bytes()).unwrap_or(0);
        self.used.fetch_sub(b as u64, Ordering::Relaxed);
    }
}

impl Run {
    pub fn read(&self) -> std::sync::RwLockReadGuard<'_, RunData> {
        self.data.read().unwrap_or_else(|e| e.into_inner())
    }
    pub fn write(&self) -> std::sync::RwLockWriteGuard<'_, RunData> {
        self.data.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Summary JSON (what the run list and event stream carry).
    pub fn json(&self, w: &mut W) {
        let d = self.read();
        w.obj();
        w.ku("id", self.id);
        w.ku("session", self.session.id);
        w.ks("plugin", self.plugin.name());
        w.key("args").arr();
        for a in &self.args {
            w.s(a);
        }
        w.end_arr();
        w.ks("origin", &self.origin);
        w.ku("batch", self.batch.load(Ordering::Relaxed));
        w.ks("status", d.status.as_str());
        w.kb("busy", d.busy);
        w.ku("rows", d.produced);
        w.ku("stored", d.table.rows() as u64);
        w.kb("truncated", d.truncated);
        w.kb("evicted", d.evicted);
        w.ku("created", self.created_ms);
        w.ku("started", d.started_ms);
        let el = match (d.status, d.started) {
            (Status::Running, Some(s)) => s.elapsed().as_millis() as u64,
            _ => d.elapsed_ms,
        };
        w.ku("elapsed", el);
        w.ku("seq", d.seq);
        w.key("cols").arr();
        for c in &d.columns {
            w.obj().ks("name", &c.name).ks("type", coltype_name(c.ty)).end_obj();
        }
        w.end_arr();
        w.key("files").arr();
        for (n, s) in &d.files {
            w.obj().ks("name", n).ku("size", *s).end_obj();
        }
        w.end_arr();
        match &d.error {
            None => {
                w.key("error").null();
            }
            Some(e) => {
                w.key("error").obj();
                w.ks("kind", e.kind).ks("title", &e.title).ks("message", &e.message);
                w.key("hints").arr();
                for h in &e.hints {
                    w.s(h);
                }
                w.end_arr();
                w.ks("detail", &e.detail);
                w.end_obj();
            }
        }
        w.end_obj();
    }
}

pub fn coltype_name(t: ColType) -> &'static str {
    match t {
        ColType::Int => "Int",
        ColType::Str => "Str",
        ColType::Bytes => "Bytes",
        ColType::Float => "Float",
        ColType::Bool => "Bool",
        ColType::DateTime => "DateTime",
        ColType::Hex => "Hex",
        ColType::Bin => "Bin",
        ColType::HexBytes => "HexBytes",
        ColType::MultiTypeData => "MultiTypeData",
        ColType::Disassembly => "Disassembly",
        ColType::LayerData => "LayerData",
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ------------------------------------------------------------------------------------------
// the streaming sink

struct WebSink<'a> {
    run: &'a Run,
    hub: &'a Hub,
    batch: Table,
    types: Vec<ColType>,
    ncols: usize,
    depth_len: usize,
    last_flush: Instant,
    begun: bool,
}

impl WebSink<'_> {
    fn flush(&mut self) {
        let n = self.batch.rows() as u64;
        if n == 0 {
            return;
        }
        let add = self.batch.bytes() as u64;
        // memory budget: one run may use half of it; past that its rows are counted, not kept.
        // Before giving up, drop the tables of the least recently viewed finished runs (they can
        // be re-run in moments; the event stream tells the browser).
        let (mine, truncated) = {
            let d = self.run.read();
            (d.table.bytes() as u64, d.truncated)
        };
        let fits_run = !truncated && mine + add <= self.run.budget / 2;
        if fits_run
            && self.run.used.load(Ordering::Relaxed) + add > self.run.budget
            && let Some(reg) = self.run.registry.upgrade()
        {
            reg.evict(add, self.run.id);
        }
        {
            let mut d = self.run.write();
            if !fits_run || d.table.text.len() + self.batch.text.len() > MAX_TEXT || self.run.used.load(Ordering::Relaxed) + add > self.run.budget {
                d.truncated = true;
            } else {
                self.run.used.fetch_add(add, Ordering::Relaxed);
                d.table.append(&self.batch);
            }
            d.produced += n;
            d.seq = self.hub.bump();
        }
        self.batch.clear();
        self.last_flush = Instant::now();
    }
}

impl RowSink for WebSink<'_> {
    fn begin(&mut self, columns: Vec<Column>) -> Result<()> {
        self.types = columns.iter().map(|c| c.ty).collect();
        self.ncols = columns.len();
        self.batch = Table::new(self.ncols);
        self.begun = true;
        let mut d = self.run.write();
        d.table = Table::new(self.ncols);
        d.types = self.types.clone();
        d.columns = columns;
        d.seq = self.hub.bump();
        Ok(())
    }

    fn row(&mut self, depth: usize, values: Vec<Value>) -> Result<()> {
        if self.run.cancel.load(Ordering::Relaxed) {
            return Err(Error::msg("cancelled by the user"));
        }
        if values.len() != self.ncols {
            return Err(Error::msg("Values must be a list of objects made up of simple types and number the same as the columns"));
        }
        // TreeGrid.populate: at most one level deeper than the previous row
        let d = depth.min(self.depth_len);
        self.depth_len = d + 1;
        for (i, v) in values.iter().enumerate() {
            match (self.types[i], v) {
                (_, Value::NotApplicable) => self.batch.push_cell(K_NA),
                (_, v) if v.is_absent() => self.batch.push_cell(K_ABSENT),
                (ColType::Int, Value::Int(x)) if i64::try_from(*x).is_ok() => self.batch.push_num(K_DEC, *x as i64 as u64),
                (ColType::Hex, Value::Int(x)) if (0..=u64::MAX as i128).contains(x) => self.batch.push_num(K_HEX, *x as u64),
                (ty, v) => {
                    crate::renderers::text::render_cell(&mut self.batch.text, ty, v, false);
                    self.batch.push_cell(K_TEXT);
                }
            }
        }
        self.batch.depth.push(d.min(u16::MAX as usize) as u16);
        if self.batch.rows() >= 4096 || self.last_flush.elapsed() >= Duration::from_millis(40) {
            self.flush();
        }
        Ok(())
    }
}

// ------------------------------------------------------------------------------------------
// errors

fn plugin_os(name: &str) -> Option<&'static str> {
    match name.split('.').next() {
        Some("windows") => Some("windows"),
        Some("linux") => Some("linux"),
        Some("mac") => Some("mac"),
        _ => None,
    }
}

fn os_label(os: &str) -> &'static str {
    match os {
        "windows" => "Windows",
        "linux" => "Linux",
        "mac" => "macOS",
        _ => "this",
    }
}

/// Explain a plugin failure in plain language.
pub fn explain(e: &Error, run: &Run) -> ErrInfo {
    let session = &run.session;
    let image_os = session.os();
    let want = plugin_os(run.plugin.name());
    let detail = e.to_string();
    match e {
        Error::Unsatisfied(s) => {
            let banners = session.banners();
            let banner_os = banners.first().map(|b| if b.starts_with("Darwin") { "mac" } else { "linux" });
            if let (Some(img), Some(w)) = (image_os.or(banner_os), want)
                && img != w
            {
                return ErrInfo {
                    kind: "wrong-os",
                    title: format!("This is a {} image", os_label(img)),
                    message: format!("{} runs on {} memory images only. Pick the {}.* equivalent instead.", run.plugin.name(), os_label(w), img),
                    hints: vec![format!("Press Ctrl+K and type \"{img}.\" to see the plugins for this image.")],
                    detail,
                };
            }
            let symbols = (s.contains("symbol_table_name") && !s.contains("layer_name")) || (banner_os.is_some() && want.is_none_or(|w| Some(w) == banner_os));
            if symbols {
                let mut hints = Vec::new();
                match want.or(image_os).or(banner_os) {
                    Some("windows") => {
                        let pdb = session.ctx.windows_kernel().ok().map(|k| format!("{} {}-{}", k.pdb_name, k.guid, k.age));
                        hints.push(format!(
                            "fastvol needs the kernel's symbol table{}. It is downloaded from Microsoft's symbol server on first use{}.",
                            pdb.map(|p| format!(" ({p})")).unwrap_or_default(),
                            if session.offline { ", but this session was started with --offline" } else { "" }
                        ));
                        hints.push("If this machine is offline, convert the PDB elsewhere and put the .json.xz under a symbols directory, then restart with -s DIR.".into());
                    }
                    Some("linux") => {
                        if let Some(b) = banners.first() {
                            hints.push(format!("Kernel banner found in the image: {b}"));
                        }
                        hints.push("Linux symbol tables must match that exact kernel build: generate one with dwarf2json from the matching vmlinux with debug info (e.g. the linux-image-…-dbgsym package).".into());
                        hints.push("Put the .json(.xz) under a directory and open the image again with that symbols directory (or restart `fvol serve` with -s DIR).".into());
                    }
                    Some("mac") => {
                        if let Some(b) = banners.first() {
                            hints.push(format!("Kernel banner found in the image: {b}"));
                        }
                        hints.push("macOS symbol tables must match that exact kernel build; generate one with dwarf2json from the matching Kernel Debug Kit.".into());
                        hints.push("Put the .json(.xz) under a directory and open the image again with that symbols directory (or restart `fvol serve` with -s DIR).".into());
                    }
                    _ => {}
                }
                if !session.symbol_dirs.is_empty() {
                    hints.push(format!("Symbol directories searched: {}", session.symbol_dirs.join(", ")));
                }
                return ErrInfo { kind: "symbols", title: "No symbols for this kernel".into(), message: "The kernel was found, but no matching symbol table (ISF) is available, so its structures can't be interpreted.".into(), hints, detail };
            }
            let other: Vec<&str> = s.lines().filter(|l| !l.contains("layer_name") && !l.contains("symbol_table_name")).collect();
            if !other.is_empty() && !s.contains("layer_name") {
                return ErrInfo {
                    kind: "requirement",
                    title: "A requirement of this plugin isn't met".into(),
                    message: other.iter().map(|l| l.split('\t').last().unwrap_or(l)).collect::<Vec<_>>().join("; "),
                    hints: vec!["Check the plugin's options.".into()],
                    detail,
                };
            }
            ErrInfo {
                kind: "layer",
                title: format!("No {} kernel found in this image", want.map(os_label).unwrap_or("supported")),
                message: "The memory image couldn't be matched to this operating system (no kernel page tables were found).".into(),
                hints: vec![
                    "Make sure the file is a complete memory image (raw, LiME, ELF core, Windows crash dump, VMware, QEMU, AVML...).".into(),
                    "If the image belongs to another OS, use that OS's plugins.".into(),
                ],
                detail,
            }
        }
        Error::InvalidAddress { addr } | Error::Swapped { addr } => ErrInfo {
            kind: "page",
            title: "A memory page this plugin needed isn't in the image".into(),
            message: format!("Reading {addr:#x} failed: the page is paged out, swapped, or the image was smeared during acquisition."),
            hints: vec!["Results up to this point are shown. Try the scan-based variant of the plugin (e.g. psscan instead of pslist), which tolerates missing pages.".into()],
            detail,
        },
        Error::Symbol(s) => ErrInfo {
            kind: "symbol",
            title: "The symbol table lacks something this plugin needs".into(),
            message: format!("{s}. This usually means the plugin doesn't support this OS version, or the symbol table is incomplete."),
            hints: vec![],
            detail,
        },
        Error::Layer(s) => ErrInfo { kind: "layer", title: "The memory image couldn't be read as expected".into(), message: s.clone(), hints: vec![], detail },
        Error::Io(io) => ErrInfo { kind: "io", title: "A file couldn't be read or written".into(), message: io.to_string(), hints: vec![format!("Output directory: {}", run.out_dir.display())], detail },
        Error::Msg(m) if m == "cancelled by the user" => ErrInfo { kind: "cancelled", title: "Cancelled".into(), message: String::new(), hints: vec![], detail },
        Error::Msg(m) => ErrInfo { kind: "error", title: "The plugin stopped with an error".into(), message: m.clone(), hints: vec![], detail },
    }
}

// ------------------------------------------------------------------------------------------
// scheduler

/// What a saved analysis records about one finished plugin execution.
pub struct SavedRun {
    pub status: Status,
    pub rows: u64,
    pub columns: Vec<Column>,
    pub error: Option<ErrInfo>,
    pub created_ms: u64,
    pub started_ms: u64,
    pub elapsed_ms: u64,
    pub out_dir: PathBuf,
    pub sidecar: Option<PathBuf>,
}

/// A batch: what the UI calls a "run", several plugins (each with its own options) started
/// together. Each plugin is an ordinary [`Run`] (queued, streamed, cancellable) that carries the
/// batch id; the batch only names and groups them.
pub struct Batch {
    pub id: u64,
    pub session: u64,
    pub name: Mutex<String>,
    /// the results panel's "Filter rows" text for this run (kept so a saved analysis restores it)
    pub filter: Mutex<String>,
    pub created_ms: u64,
    /// the plugin executions, in the order they were given
    pub runs: Vec<u64>,
}

impl Batch {
    pub fn json(&self, w: &mut W) {
        w.obj().ku("id", self.id).ku("session", self.session).ks("name", &self.name.lock().unwrap_or_else(|e| e.into_inner()));
        w.ks("filter", &self.filter.lock().unwrap_or_else(|e| e.into_inner()));
        w.ku("created", self.created_ms).key("runs").arr();
        for id in &self.runs {
            w.u(*id);
        }
        w.end_arr().end_obj();
    }
}

pub struct Runs {
    pub list: Mutex<Vec<Arc<Run>>>,
    pub batches: Mutex<Vec<Arc<Batch>>>,
    next_batch: AtomicU64,
    next_id: AtomicU64,
    sched: Mutex<(usize, VecDeque<Arc<Run>>)>,
    /// plugins that may run at once (`--parallel`), and the limit in force (`--parallelism off` = 1)
    pub max_parallel: usize,
    pub parallel: std::sync::atomic::AtomicUsize,
    /// `-l`: a file that gets a line per run started, finished or failed
    pub log: Mutex<Option<String>>,
    /// `--write-config` / `--save-config`: the file each run's configuration is written to
    pub config_name: Mutex<Option<String>>,
    pub hub: Arc<Hub>,
    /// table storage in use / allowed (bytes)
    pub used: Arc<AtomicU64>,
    pub budget: u64,
}

impl Runs {
    pub fn new(hub: Arc<Hub>, max_parallel: usize, budget: u64) -> Runs {
        Runs {
            list: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
            next_batch: AtomicU64::new(1),
            next_id: AtomicU64::new(1),
            sched: Mutex::new((0, VecDeque::new())),
            max_parallel: max_parallel.max(1),
            parallel: std::sync::atomic::AtomicUsize::new(max_parallel.max(1)),
            log: Mutex::new(None),
            config_name: Mutex::new(None),
            hub,
            used: Arc::new(AtomicU64::new(0)),
            budget,
        }
    }

    pub fn get(&self, id: u64) -> Option<Arc<Run>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|r| r.id == id).cloned()
    }

    pub fn all(&self) -> Vec<Arc<Run>> {
        self.list.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// An existing run of the same key in `session` that is done or still going.
    pub fn find_reusable(&self, session: u64, key: &str) -> Option<Arc<Run>> {
        let l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        l.iter()
            .rev()
            .find(|r| {
                r.session.id == session && r.key == key && {
                    let d = r.read();
                    matches!(d.status, Status::Done | Status::Running | Status::Queued) && !d.truncated && !d.evicted
                }
            })
            .cloned()
    }

    /// Free at least `need` bytes of table storage by dropping the tables of finished runs,
    /// least recently viewed first (never `except`, never running ones).
    pub fn evict(&self, need: u64, except: u64) {
        let mut cands: Vec<(u64, Arc<Run>)> = self
            .all()
            .into_iter()
            .filter(|r| r.id != except)
            .filter(|r| r.data.try_read().map(|d| d.status.finished() && !d.busy && d.table.rows() > 0).unwrap_or(false))
            .map(|r| (r.last_access.load(Ordering::Relaxed).max(r.created_ms), r))
            .collect();
        cands.sort_by_key(|c| c.0);
        for (_, r) in cands {
            if self.used.load(Ordering::Relaxed) + need <= self.budget {
                break;
            }
            let Ok(mut d) = r.data.try_write() else { continue };
            let b = d.table.bytes() as u64;
            d.table = Table::new(d.table.ncols);
            d.evicted = true;
            d.seq = self.hub.bump();
            drop(d);
            self.used.fetch_sub(b, Ordering::Relaxed);
            r.views.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    pub fn create(self: &Arc<Self>, session: Arc<Session>, plugin: &'static dyn Plugin, cfg: Config, args: Vec<String>, key: String, origin: &str) -> Arc<Run> {
        self.create_in(session, plugin, cfg, args, key, origin, None)
    }

    /// [`Runs::create`]; `keep` gives a restored run its original output folder and creation time.
    #[allow(clippy::too_many_arguments)]
    fn create_in(self: &Arc<Self>, session: Arc<Session>, plugin: &'static dyn Plugin, cfg: Config, args: Vec<String>, key: String, origin: &str, keep: Option<(PathBuf, u64)>) -> Arc<Run> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let short: String = plugin.name().chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
        // run ids restart with the server but output directories persist: never reuse one
        let mut out_dir = session.out_root.join(format!("run-{id:04}-{short}"));
        let mut k = 2;
        while keep.is_none() && std::fs::symlink_metadata(&out_dir).is_ok() {
            out_dir = session.out_root.join(format!("run-{id:04}-{short}-{k}"));
            k += 1;
        }
        let created_ms = keep.as_ref().map(|k| k.1).unwrap_or_else(now_ms);
        if let Some((dir, _)) = keep {
            out_dir = dir;
        }
        let run = Arc::new(Run {
            id,
            used: self.used.clone(),
            budget: self.budget,
            registry: Arc::downgrade(self),
            last_access: AtomicU64::new(0),
            session,
            plugin,
            cfg,
            args,
            key,
            origin: origin.to_string(),
            batch: AtomicU64::new(0),
            req_args: Mutex::new(crate::cli::json::Json::Null),
            sidecar: Mutex::new(None),
            created_ms,
            out_dir,
            cancel: AtomicBool::new(false),
            data: RwLock::new(RunData {
                status: Status::Queued,
                columns: Vec::new(),
                types: Vec::new(),
                table: Table::new(0),
                produced: 0,
                truncated: false,
                error: None,
                started: None,
                started_ms: 0,
                elapsed_ms: 0,
                files: Vec::new(),
                seq: 0,
                busy: false,
                evicted: false,
            }),
            views: Mutex::new(Vec::new()),
            next_view: AtomicU64::new(1),
        });
        run.write().seq = self.hub.bump();
        self.list.lock().unwrap_or_else(|e| e.into_inner()).push(run.clone());
        run
    }

    pub fn batches(&self) -> Vec<Arc<Batch>> {
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn batch(&self, id: u64) -> Option<Arc<Batch>> {
        self.batches().into_iter().find(|b| b.id == id)
    }

    /// Group already created runs into a new batch, then queue them in order.
    pub fn submit_batch(self: &Arc<Self>, session: u64, name: String, runs: Vec<Arc<Run>>) -> Arc<Batch> {
        let id = self.next_batch.fetch_add(1, Ordering::Relaxed);
        for r in &runs {
            r.batch.store(id, Ordering::Relaxed);
        }
        let b = Arc::new(Batch { id, session, name: Mutex::new(name), filter: Mutex::new(String::new()), created_ms: now_ms(), runs: runs.iter().map(|r| r.id).collect() });
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).push(b.clone());
        for r in runs {
            self.submit(r);
        }
        self.hub.bump();
        b
    }

    pub fn rename_batch(&self, b: &Batch, name: String) {
        *b.name.lock().unwrap_or_else(|e| e.into_inner()) = name;
        self.hub.bump();
    }

    pub fn set_batch_filter(&self, b: &Batch, q: String) {
        *b.filter.lock().unwrap_or_else(|e| e.into_inner()) = q;
        self.hub.bump();
    }

    /// Remove a batch and its plugin executions (cancelling any still going).
    pub fn remove_batch(&self, id: u64) -> bool {
        let Some(b) = self.batch(id) else { return false };
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).retain(|x| x.id != id);
        for r in &b.runs {
            if let Some(run) = self.get(*r) {
                self.cancel(&run);
            }
            self.remove(*r);
        }
        self.hub.bump();
        true
    }

    /// Queue a run; it starts as soon as a slot is free.
    pub fn submit(self: &Arc<Self>, run: Arc<Run>) {
        let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        if s.0 < self.parallel.load(Ordering::Relaxed) {
            s.0 += 1;
            drop(s);
            self.spawn(run);
        } else {
            s.1.push_back(run);
        }
    }

    fn spawn(self: &Arc<Self>, run: Arc<Run>) {
        let me = self.clone();
        let spawned = std::thread::Builder::new().name(format!("run-{}", run.id)).stack_size(64 << 20).spawn({
            let run = run.clone();
            move || {
                me.execute(&run);
                me.slot_done();
            }
        });
        if spawned.is_err() {
            let mut d = run.write();
            d.status = Status::Failed;
            d.error = Some(ErrInfo { kind: "error", title: "Could not start a worker thread".into(), ..Default::default() });
            d.seq = self.hub.bump();
            drop(d);
            self.slot_done();
        }
    }

    fn slot_done(self: &Arc<Self>) {
        let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        // (the limit may have dropped meanwhile: then this slot just closes)
        if s.0 <= self.parallel.load(Ordering::Relaxed) {
            while let Some(next) = s.1.pop_front() {
                if next.cancel.load(Ordering::Relaxed) {
                    continue;
                }
                drop(s);
                self.spawn(next);
                return;
            }
        }
        s.0 -= 1;
    }

    /// A finished run from a saved analysis: its summary now, its rows from `sidecar` on first
    /// use ([`Runs::load_saved`]). Never queued.
    #[allow(clippy::too_many_arguments)]
    pub fn restore(self: &Arc<Self>, session: Arc<Session>, plugin: &'static dyn Plugin, cfg: Config, args: Vec<String>, key: String, req_args: crate::cli::json::Json, saved: SavedRun) -> Arc<Run> {
        let run = self.create_in(session, plugin, cfg, args, key, "user", Some((saved.out_dir.clone(), saved.created_ms)));
        *run.req_args.lock().unwrap_or_else(|e| e.into_inner()) = req_args;
        *run.sidecar.lock().unwrap_or_else(|e| e.into_inner()) = saved.sidecar;
        let mut d = run.write();
        d.status = saved.status;
        d.produced = saved.rows;
        d.types = saved.columns.iter().map(|c| c.ty).collect();
        d.table = Table::new(saved.columns.len());
        d.columns = saved.columns;
        d.error = saved.error;
        d.started_ms = saved.started_ms;
        d.elapsed_ms = saved.elapsed_ms;
        d.files = list_files(&saved.out_dir);
        d.seq = self.hub.bump();
        drop(d);
        run
    }

    /// Group restored runs into their saved batch (same id, name and filter).
    pub fn restore_batch(&self, id: u64, session: u64, name: String, filter: String, created_ms: u64, runs: &[Arc<Run>]) {
        for r in runs {
            r.batch.store(id, Ordering::Relaxed);
        }
        let b = Arc::new(Batch { id, session, name: Mutex::new(name), filter: Mutex::new(filter), created_ms, runs: runs.iter().map(|r| r.id).collect() });
        self.batches.lock().unwrap_or_else(|e| e.into_inner()).push(b);
        self.next_batch.fetch_max(id + 1, Ordering::Relaxed);
        self.hub.bump();
    }

    /// Read a finished run's saved rows back into memory (first view after reopening, or after
    /// its table was dropped for memory). Within the memory budget, like a live run.
    pub fn load_saved(&self, run: &Run) -> std::result::Result<(), String> {
        {
            let d = run.read();
            if d.status != Status::Done || d.table.rows() > 0 || d.produced == 0 {
                return Ok(());
            }
        }
        let Some(path) = run.sidecar.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return Ok(()) };
        let table = super::analysis::read_rows(&path, run.read().columns.len())?;
        let add = table.bytes() as u64;
        if self.used.load(Ordering::Relaxed) + add > self.budget {
            self.evict(add, run.id);
        }
        let mut d = run.write();
        if d.table.rows() > 0 {
            return Ok(());
        }
        self.used.fetch_add(add, Ordering::Relaxed);
        d.truncated = (table.rows() as u64) < d.produced;
        d.table = table;
        d.evicted = false;
        d.seq = self.hub.bump();
        drop(d);
        run.views.lock().unwrap_or_else(|e| e.into_inner()).clear();
        Ok(())
    }

    /// Change how many plugins may run at once; queued runs start if the limit went up.
    pub fn set_parallel(self: &Arc<Self>, n: usize) {
        self.parallel.store(n.max(1), Ordering::Relaxed);
        loop {
            let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
            if s.0 >= n.max(1) {
                return;
            }
            let Some(next) = s.1.pop_front() else { return };
            if next.cancel.load(Ordering::Relaxed) {
                continue;
            }
            s.0 += 1;
            drop(s);
            self.spawn(next);
        }
    }

    /// `-l`: one line per run event, like the CLI's log file.
    fn log(&self, line: &str) {
        let Some(path) = self.log.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        use std::io::Write;
        // one line per event: option values cannot start another line
        let line: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "fastvol.web INFO     {line}");
        }
    }

    /// Cancel: queued runs never start; running ones stop at their next row (the UI shows the
    /// run as cancelled right away).
    pub fn cancel(&self, run: &Run) {
        run.cancel.store(true, Ordering::Relaxed);
        let mut d = run.write();
        if !d.status.finished() {
            if d.status == Status::Running
                && let Some(s) = d.started
            {
                d.elapsed_ms = s.elapsed().as_millis() as u64;
            }
            d.status = Status::Cancelled;
            d.error = Some(ErrInfo { kind: "cancelled", title: "Cancelled".into(), ..Default::default() });
            d.seq = self.hub.bump();
        }
        drop(d);
        let mut s = self.sched.lock().unwrap_or_else(|e| e.into_inner());
        s.1.retain(|r| r.id != run.id);
    }

    pub fn remove(&self, id: u64) -> bool {
        let mut l = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let n = l.len();
        if let Some(r) = l.iter().find(|r| r.id == id) {
            r.cancel.store(true, Ordering::Relaxed);
        }
        l.retain(|r| r.id != id);
        let removed = l.len() != n;
        drop(l);
        if removed {
            self.hub.bump();
        }
        removed
    }

    fn execute(&self, run: &Arc<Run>) {
        {
            let mut d = run.write();
            if run.cancel.load(Ordering::Relaxed) {
                return;
            }
            d.status = Status::Running;
            d.busy = true;
            d.started = Some(Instant::now());
            d.started_ms = now_ms();
            d.seq = self.hub.bump();
        }
        self.log(&format!("run {} started: {} {}", run.id, run.plugin.name(), run.args.join(" ")));
        let t = Instant::now();
        let mut sink = WebSink {
            run,
            hub: &self.hub,
            batch: Table::new(0),
            types: Vec::new(),
            ncols: 0,
            depth_len: 0,
            last_flush: Instant::now() - Duration::from_secs(1),
            begun: false,
        };
        let dir = run.out_dir.to_string_lossy().into_owned();
        // Runs that write files get a Context of their own whose output_dir is the run's
        // directory: some plugins read `opts.output_dir` directly instead of going through
        // `create_output_file`. A fresh Context costs a millisecond or two (automagic results
        // and symbol tables are cached); everything else shares the session's warm Context.
        let ctx = if writes_files(run.plugin, &run.cfg) {
            let mut o = run.session.ctx.opts.clone();
            o.output_dir = dir.clone();
            o.clear_cache = false;
            match crate::context::Context::new(o) {
                Ok(c) => Arc::new(c),
                Err(_) => run.session.ctx.clone(),
            }
        } else {
            run.session.ctx.clone()
        };
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::context::with_output_dir(&dir, || run.plugin.run(&ctx, &run.cfg, &mut sink))));
        sink.flush();
        let begun = sink.begun;
        drop(sink);
        let files = list_files(&run.out_dir);
        let mut d = run.write();
        d.busy = false;
        d.files = files;
        if d.status != Status::Cancelled {
            d.elapsed_ms = t.elapsed().as_millis() as u64;
            match res {
                Ok(Ok(())) => {
                    d.status = Status::Done;
                    if !begun {
                        d.columns.clear();
                    }
                }
                Ok(Err(e)) => {
                    let info = explain(&e, run);
                    d.status = if info.kind == "cancelled" { Status::Cancelled } else { Status::Failed };
                    d.error = Some(info);
                }
                Err(p) => {
                    let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "plugin panicked".into());
                    d.status = Status::Failed;
                    d.error = Some(ErrInfo {
                        kind: "crash",
                        title: "The plugin crashed".into(),
                        message: "This is a bug in fastvol, not a problem with your image. Rows produced before the crash are shown.".into(),
                        hints: vec!["Please report it with the plugin name, its options and the details below.".into()],
                        detail: format!("RuntimeError: {msg}"),
                    });
                }
            }
        }
        d.seq = self.hub.bump();
        let (status, rows) = (d.status, d.produced);
        drop(d);
        self.log(&format!("run {} {}: {} ({} rows)", run.id, status.as_str(), run.plugin.name(), rows));
        // python writes the configuration once the automagics satisfied the plugin
        if status == Status::Done
            && let Some(name) = self.config_name.lock().unwrap_or_else(|e| e.into_inner()).clone()
            && let Ok(items) = crate::plugins::generic::pyconfig::plugin_configuration(&ctx, run.plugin.name(), &run.cfg, false)
            && std::fs::create_dir_all(&run.out_dir).is_ok()
        {
            let _ = std::fs::write(run.out_dir.join(&name), format!("{}\n", crate::cli::json::Json::Obj(items).dump(Some(2))));
            let mut d = run.write();
            d.files = list_files(&run.out_dir);
            d.seq = self.hub.bump();
        }
    }
}

/// Whether a run may write files (so it needs its own output directory end to end).
pub fn writes_files(plugin: &dyn Plugin, cfg: &Config) -> bool {
    let n = plugin.name();
    if ["dumpfiles", "pedump", "layerwriter", "configwriter", "module_extract"].iter().any(|p| n.contains(p)) {
        return true;
    }
    cfg.values
        .iter()
        .any(|(k, v)| matches!(v, crate::plugins::ConfigValue::Bool(true)) && (k.contains("dump") || k.contains("bodyfile") || k.contains("record-config") || k.contains("record_config")))
}

/// Regular files directly inside `dir` (name, size), sorted by name.
pub fn list_files(dir: &std::path::Path) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .flatten()
            .filter_map(|e| {
                let md = e.metadata().ok()?;
                if !md.is_file() {
                    return None;
                }
                Some((e.file_name().to_str()?.to_string(), md.len()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

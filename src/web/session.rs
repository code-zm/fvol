//! One analysis session = one memory image + its long-lived `Context` (mapped image, loaded
//! symbol tables, automagic results, process layers), shared by every plugin run.
//!
//! When a session starts, a background "warm-up" opens the image, finds the kernel and
//! gathers the facts for the landing view, so the first plugin the analyst runs is already
//! warm.

use super::jsonw::W;
use crate::context::{Context, GlobalOptions};
use crate::objects::LayerRef;
use crate::renderers::CollectSink;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum Warm {
    /// no image loaded
    Idle,
    Running(&'static str),
    Ready,
    Failed(String),
}

#[derive(Clone, Default)]
pub struct Summary {
    pub os: Option<&'static str>,
    /// ordered facts (label, value) for the landing view
    pub facts: Vec<(String, String)>,
    /// physical layer stack, top first
    pub layers: Vec<String>,
    pub arch: Option<&'static str>,
    pub warm_ms: u64,
    /// why each OS was ruled out (shown when nothing matched)
    pub notes: Vec<String>,
    /// kernel banners found in the image when no kernel could be set up (missing symbols)
    pub banners: Vec<String>,
}

/// Appends a landing fact unless one with the same label (ignoring ASCII case) is already
/// there: the first wins, so windows.info's own rows ("Kernel Base", "DTB") are not repeated.
pub fn add_fact(facts: &mut Vec<(String, String)>, label: String, value: String) {
    if !facts.iter().any(|(k, _)| k.eq_ignore_ascii_case(&label)) {
        facts.push((label, value));
    }
}

/// Linux / macOS kernel banners in the physical layer, most frequent first (what the analyst
/// needs to find the right symbol table when none matched).
pub fn find_banners(phys: crate::objects::LayerRef) -> Vec<String> {
    use crate::layers::scan::{MultiStringScanner, scan};
    let pats: [&[u8]; 2] = [b"Linux version ", b"Darwin Kernel Version "];
    let hits = scan(phys, &MultiStringScanner::new(&pats), None);
    let mut counts: Vec<(String, usize)> = Vec::new();
    for (off, _) in hits.into_iter().take(4000) {
        let mut buf = [0u8; 320];
        phys.read_padded(off, &mut buf);
        let end = buf.iter().position(|&b| b == 0 || b == b'\n').unwrap_or(buf.len());
        let s = &buf[..end];
        // real banners are long, printable and carry a build description
        if s.len() < 40 || !s.iter().all(|&b| (0x20..0x7f).contains(&b)) || !(s.windows(2).any(|w| w == b" (") || s.windows(4).any(|w| w == b"xnu-")) {
            continue;
        }
        let t = String::from_utf8_lossy(s).trim().to_string();
        match counts.iter_mut().find(|(b, _)| *b == t) {
            Some(c) => c.1 += 1,
            None => counts.push((t, 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    // one banner per kernel version (copies in memory are often truncated or have trailing junk)
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for (b, _) in counts {
        let ver = b.split_whitespace().nth(2).unwrap_or("").to_string();
        if !seen.contains(&ver) {
            seen.push(ver);
            out.push(b);
        }
    }
    out.truncate(3);
    out
}

pub struct Session {
    pub id: u64,
    pub ctx: Arc<Context>,
    pub image: Option<PathBuf>,
    pub image_size: u64,
    pub symbol_dirs: Vec<String>,
    pub out_root: PathBuf,
    pub offline: bool,
    pub created: Instant,
    pub warm: Mutex<(Warm, Summary)>,
    proc_layers: Mutex<HashMap<i128, Option<LayerRef>>>,
}

/// python `quote()` of a path for a file:// URL (same rules as the CLI).
pub fn quote_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    for &b in p.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[derive(Clone, Default)]
pub struct SessionOpts {
    pub image: Option<PathBuf>,
    pub symbol_dirs: Vec<String>,
    pub out_root: PathBuf,
    pub offline: bool,
    pub remote_isf_url: Option<String>,
    pub cache_path: Option<String>,
    /// `--clear-cache` (once, when this session opens)
    pub clear_cache: bool,
    /// `--single-location`: open this location instead of the image file
    pub single_location: Option<String>,
    pub stackers: Option<Vec<String>>,
    pub swap_locations: Vec<String>,
    pub verbosity: u8,
}

impl Session {
    pub fn new(id: u64, o: &SessionOpts) -> crate::error::Result<Session> {
        let mut g = GlobalOptions {
            symbol_dirs: o.symbol_dirs.clone(),
            offline: o.offline,
            remote_isf_url: o.remote_isf_url.clone(),
            cache_path: o.cache_path.clone(),
            output_dir: o.out_root.to_string_lossy().into_owned(),
            quiet: true,
            clear_cache: o.clear_cache,
            stackers: o.stackers.clone(),
            swap_locations: o.swap_locations.clone(),
            verbosity: o.verbosity,
            ..Default::default()
        };
        let mut size = 0;
        if let Some(p) = &o.image {
            let s = p.to_string_lossy().into_owned();
            g.single_location = Some(format!("file://{}", quote_path(&s)));
            g.file = Some(s);
            size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
        }
        // --single-location wins over the file, as on the command line
        if let Some(u) = o.single_location.as_ref().filter(|u| !u.is_empty()) {
            g.file = u.strip_prefix("file://").map(|p| crate::util::paths::unquote(p));
            g.single_location = Some(u.clone());
        }
        let ctx = Context::new(g)?;
        let warm = if o.image.is_some() { Warm::Running("Opening image") } else { Warm::Idle };
        Ok(Session {
            id,
            ctx: Arc::new(ctx),
            image: o.image.clone(),
            image_size: size,
            symbol_dirs: o.symbol_dirs.clone(),
            out_root: o.out_root.clone(),
            offline: o.offline,
            created: Instant::now(),
            warm: Mutex::new((warm, Summary::default())),
            proc_layers: Mutex::new(HashMap::new()),
        })
    }

    pub fn os(&self) -> Option<&'static str> {
        self.warm.lock().unwrap_or_else(|e| e.into_inner()).1.os
    }

    pub fn banners(&self) -> Vec<String> {
        self.warm.lock().unwrap_or_else(|e| e.into_inner()).1.banners.clone()
    }

    fn set_phase(&self, p: &'static str, hub: &super::runs::Hub) {
        self.warm.lock().unwrap_or_else(|e| e.into_inner()).0 = Warm::Running(p);
        hub.bump();
    }

    /// Open the image, find the kernel, collect the landing facts. Runs on its own thread.
    pub fn warm_up(self: &Arc<Self>, hub: &super::runs::Hub, plugins: &[&'static dyn crate::plugins::Plugin]) {
        if self.image.is_none() {
            return;
        }
        let t = Instant::now();
        let mut sum = Summary::default();
        let ctx = &self.ctx;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
            self.set_phase("Opening image", hub);
            ctx.physical().map_err(|e| format!("The image could not be opened: {}", plain_err(&e)))?;
            if let Ok(listing) = ctx.physical_listing() {
                sum.layers = listing.iter().map(|e| format!("{} ({})", e.name, e.class)).collect();
            }
            let has = |prefix: &str| plugins.iter().any(|p| p.name().starts_with(prefix));
            // cheap format hint: ELF cores and LiME dumps are rarely Windows
            let magic = self.image.as_ref().and_then(|p| {
                use std::io::Read;
                let mut b = [0u8; 8];
                std::fs::File::open(p).ok()?.read_exact(&mut b).ok()?;
                Some(b)
            });
            let unixy = magic.is_some_and(|m| &m[..4] == b"\x7fELF" || &m[..4] == b"EMiL");
            let order: [&'static str; 3] = if unixy { ["linux", "mac", "windows"] } else { ["windows", "linux", "mac"] };
            for os in order {
                if !has(&format!("{os}.")) {
                    continue;
                }
                match os {
                    "windows" => {
                        self.set_phase("Finding the Windows kernel", hub);
                        match ctx.windows_kernel() {
                            Ok(k) => {
                                sum.os = Some("windows");
                                sum.arch = Some(if k.table.is_64bit() { "intel64" } else { "intel" });
                                self.set_phase("Reading OS details", hub);
                                sum.facts.push(("Kernel".into(), format!("{} {}-{}", k.pdb_name, k.guid, k.age)));
                                if let Some(info) = plugins.iter().find(|p| p.name() == "windows.info.Info") {
                                    let mut sink = CollectSink::default();
                                    let cfg = crate::plugins::Config::default();
                                    if info.run(ctx, &cfg, &mut sink).is_ok() {
                                        for (_, row) in &sink.rows {
                                            if row.len() >= 2 {
                                                let mut a = Vec::new();
                                                let mut b = Vec::new();
                                                let c0 = sink.columns.first().map(|c| c.ty).unwrap_or(crate::renderers::ColType::Str);
                                                let c1 = sink.columns.get(1).map(|c| c.ty).unwrap_or(crate::renderers::ColType::Str);
                                                crate::renderers::text::render_cell(&mut a, c0, &row[0], false);
                                                crate::renderers::text::render_cell(&mut b, c1, &row[1], false);
                                                add_fact(&mut sum.facts, String::from_utf8_lossy(&a).into_owned(), String::from_utf8_lossy(&b).into_owned());
                                            }
                                        }
                                    }
                                }
                                // windows.info already reports both (as "Kernel Base" and "DTB")
                                // unless it failed
                                add_fact(&mut sum.facts, "Kernel Base".into(), format!("{:#x}", k.base));
                                add_fact(&mut sum.facts, "DTB".into(), format!("{:#x}", k.dtb));
                                break;
                            }
                            Err(e) => sum.notes.push(format!("Windows: {}", plain_err(&e))),
                        }
                    }
                    "linux" => {
                        self.set_phase("Finding the Linux kernel", hub);
                        match ctx.linux_kernel() {
                            Ok(k) => {
                                sum.os = Some("linux");
                                sum.arch = Some(if k.table.is_64bit() { "intel64" } else { "intel" });
                                let banner = String::from_utf8_lossy(&k.banner).trim_end_matches(['\0', '\n']).to_string();
                                sum.facts.push(("Banner".into(), banner));
                                sum.facts.push(("KASLR shift".into(), format!("{:#x}", k.kaslr_shift)));
                                sum.facts.push(("DTB".into(), format!("{:#x}", k.dtb)));
                                sum.facts.push(("Layer".into(), k.stacker.to_string()));
                                break;
                            }
                            Err(e) => sum.notes.push(format!("Linux: {}", plain_err(&e))),
                        }
                    }
                    _ => {
                        self.set_phase("Finding the macOS kernel", hub);
                        match ctx.mac_kernel() {
                            Ok(k) => {
                                sum.os = Some("mac");
                                sum.arch = Some(if k.table.is_64bit() { "intel64" } else { "intel" });
                                let banner = String::from_utf8_lossy(&k.banner).trim_end_matches(['\0', '\n']).to_string();
                                sum.facts.push(("Banner".into(), banner));
                                sum.facts.push(("KASLR shift".into(), format!("{:#x}", k.kaslr_shift)));
                                sum.facts.push(("DTB".into(), format!("{:#x}", k.dtb)));
                                break;
                            }
                            Err(e) => sum.notes.push(format!("macOS: {}", plain_err(&e))),
                        }
                    }
                }
            }
            Ok(())
        }));
        if sum.os.is_none()
            && let Ok(phys) = self.ctx.physical()
        {
            self.set_phase("Looking for kernel banners", hub);
            sum.banners = find_banners(phys);
        }
        sum.warm_ms = t.elapsed().as_millis() as u64;
        let state = match result {
            Ok(Ok(())) if sum.os.is_some() => Warm::Ready,
            Ok(Ok(())) if !sum.banners.is_empty() => {
                let os = if sum.banners[0].starts_with("Darwin") { "macOS" } else { "Linux" };
                Warm::Failed(format!("This is a {os} image, but no symbol table (ISF) matches its kernel, so its structures can't be read."))
            }
            Ok(Ok(())) => Warm::Failed("No supported operating system kernel was found in this image.".into()),
            Ok(Err(m)) => Warm::Failed(m),
            Err(_) => Warm::Failed("Internal error while analysing the image (fastvol bug).".into()),
        };
        *self.warm.lock().unwrap_or_else(|e| e.into_inner()) = (state, sum);
        hub.bump();
    }

    /// The layer behind a hex-view layer id: "phys", "kernel" or "pid:<n>".
    pub fn layer(&self, id: &str) -> Result<LayerRef, String> {
        let ctx = &self.ctx;
        match id {
            "phys" => ctx.physical().map_err(|e| plain_err(&e)),
            "kernel" => match self.os() {
                Some("windows") => ctx.windows_kernel().map(|k| k.vlayer).map_err(|e| plain_err(&e)),
                Some("linux") => ctx.linux_kernel().map(|k| k.vlayer).map_err(|e| plain_err(&e)),
                Some("mac") => ctx.mac_kernel().map(|k| k.vlayer).map_err(|e| plain_err(&e)),
                _ => Err("the kernel of this image has not been identified".into()),
            },
            _ => {
                let pid = id.strip_prefix("pid:").and_then(|p| crate::renderers::pyfmt::parse_int0(p)).ok_or("unknown layer")?;
                if let Some(l) = self.proc_layers.lock().unwrap_or_else(|e| e.into_inner()).get(&pid) {
                    return l.ok_or_else(|| format!("process {pid} has no user address space"));
                }
                let l = super::mem::process_layer(self, pid)?;
                self.proc_layers.lock().unwrap_or_else(|e| e.into_inner()).insert(pid, l);
                l.ok_or_else(|| format!("process {pid} has no user address space (kernel thread or exited process)"))
            }
        }
    }

    pub fn json(&self, w: &mut W) {
        let (state, sum) = self.warm.lock().unwrap_or_else(|e| e.into_inner()).clone();
        w.obj();
        w.ku("id", self.id);
        w.key("image").opt_s(self.image.as_ref().map(|p| p.to_string_lossy()).as_deref());
        w.key("name").opt_s(self.image.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy()).as_deref());
        w.ku("size", self.image_size);
        w.key("symbol_dirs").arr();
        for d in &self.symbol_dirs {
            w.s(d);
        }
        w.end_arr();
        w.ks("output_dir", &self.out_root.to_string_lossy());
        w.kb("offline", self.offline);
        let (st, phase, err) = match &state {
            Warm::Idle => ("idle", None, None),
            Warm::Running(p) => ("warming", Some(*p), None),
            Warm::Ready => ("ready", None, None),
            Warm::Failed(m) => ("failed", None, Some(m.as_str())),
        };
        w.ks("state", st);
        w.key("phase").opt_s(phase);
        w.key("error").opt_s(err);
        w.key("os").opt_s(sum.os);
        w.key("arch").opt_s(sum.arch);
        w.ku("warm_ms", sum.warm_ms);
        w.key("facts").arr();
        for (k, v) in &sum.facts {
            w.arr().s(k).s(v).end_arr();
        }
        w.end_arr();
        w.key("layers").arr();
        for l in &sum.layers {
            w.s(l);
        }
        w.end_arr();
        w.key("notes").arr();
        for n in &sum.notes {
            w.s(n);
        }
        w.end_arr();
        w.key("banners").arr();
        for b in &sum.banners {
            w.s(b);
        }
        w.end_arr();
        w.end_obj();
    }
}

/// One-line, plain-language text of an error.
pub fn plain_err(e: &crate::error::Error) -> String {
    use crate::error::Error;
    match e {
        Error::Unsatisfied(s) => {
            if s.contains("symbol_table_name") && !s.contains("layer_name") {
                "no matching symbol table (ISF) was found for this kernel".into()
            } else if s.contains("layer_name") {
                "no kernel page tables were found (not this OS, or the image is damaged)".into()
            } else {
                s.lines().next().unwrap_or("requirement not satisfied").to_string()
            }
        }
        Error::Io(io) => format!("I/O error: {io}"),
        e => e.to_string(),
    }
}

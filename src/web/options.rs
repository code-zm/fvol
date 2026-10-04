//! The global `fvol` options of the Options dialog, saved with the analysis. Each does what it
//! does on the command line, applied at the matching step: opening the image, starting a run,
//! or the `fvol` output export (the table is in docs/web-ui.md, Options).

use super::jsonw::W;
use crate::cli::json::{self, Json};
use crate::plugins::Plugin;
use std::path::Path;

pub const PARALLELISM: [&str; 3] = ["processes", "threads", "off"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Options {
    pub config: Option<String>,
    pub parallelism: Option<String>,
    pub extend: Vec<String>,
    pub plugin_dirs: Option<String>,
    pub symbol_dirs: Vec<String>,
    pub verbosity: u8,
    pub log: Option<String>,
    pub output_dir: Option<String>,
    pub quiet: bool,
    pub write_config: bool,
    pub save_config: Option<String>,
    pub clear_cache: bool,
    pub cache_path: Option<String>,
    pub offline: bool,
    pub remote_isf_url: Option<String>,
    pub filters: Vec<String>,
    pub hide_columns: Vec<String>,
    pub renderer: Option<String>,
    pub single_location: Option<String>,
    pub stackers: Option<Vec<String>>,
    pub single_swap_locations: Vec<String>,
}

fn text(j: &Json, k: &str) -> Option<String> {
    j.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn list(j: &Json, k: &str) -> Result<Vec<String>, String> {
    match j.get(k) {
        None | Some(Json::Null) => Ok(Vec::new()),
        Some(Json::Arr(a)) => a.iter().map(|x| x.as_str().map(|s| s.to_string()).ok_or_else(|| format!("\"{k}\" must be a list of text"))).collect::<Result<Vec<_>, _>>().map(|v| v.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()),
        Some(Json::Str(s)) => Ok(s.split(';').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()),
        Some(_) => Err(format!("\"{k}\" must be a list of text")),
    }
}
fn flag(j: &Json, k: &str) -> bool {
    matches!(j.get(k), Some(Json::Bool(true)))
}

impl Options {
    /// Read options (the dialog's, or a saved analysis's); checks what the CLI checks.
    pub fn from_json(j: &Json, cwd: &Path) -> Result<Options, String> {
        let abs = |p: String| cwd.join(p).to_string_lossy().into_owned();
        let o = Options {
            config: text(j, "config").map(abs),
            parallelism: text(j, "parallelism"),
            extend: list(j, "extend")?,
            plugin_dirs: text(j, "plugin_dirs"),
            symbol_dirs: list(j, "symbol_dirs")?.into_iter().map(abs).collect(),
            verbosity: match j.get("verbosity") {
                Some(Json::Int(i)) => (*i).clamp(0, 5) as u8,
                _ => 0,
            },
            log: text(j, "log").map(abs),
            output_dir: text(j, "output_dir").map(abs),
            quiet: flag(j, "quiet"),
            write_config: flag(j, "write_config"),
            save_config: text(j, "save_config"),
            clear_cache: flag(j, "clear_cache"),
            cache_path: text(j, "cache_path").map(abs),
            offline: flag(j, "offline"),
            remote_isf_url: text(j, "remote_isf_url"),
            filters: list(j, "filters")?,
            hide_columns: list(j, "hide_columns")?,
            renderer: text(j, "renderer"),
            single_location: text(j, "single_location"),
            stackers: match j.get("stackers") {
                None | Some(Json::Null) => None,
                _ => Some(list(j, "stackers")?),
            },
            single_swap_locations: list(j, "single_swap_locations")?,
        };
        o.check()?;
        Ok(o)
    }

    fn check(&self) -> Result<(), String> {
        if self.offline && self.remote_isf_url.is_some() {
            return Err("--offline and --remote-isf-url cannot be used together".into());
        }
        if let Some(p) = &self.parallelism
            && !PARALLELISM.contains(&p.as_str())
        {
            return Err(format!("--parallelism: choose from {}", PARALLELISM.join(", ")));
        }
        if let Some(r) = &self.renderer
            && !crate::renderers::text::RENDERER_NAMES.contains(&r.as_str())
        {
            return Err(format!("--renderer: choose from {}", crate::renderers::text::RENDERER_NAMES.join(", ")));
        }
        if let Some(c) = &self.config {
            read_config(c)?;
        }
        for e in &self.extend {
            parse_extend(e)?;
        }
        if let Some(d) = &self.output_dir
            && !Path::new(d).is_dir()
        {
            return Err(format!("--output-dir: {d} is not a directory"));
        }
        if let Some(d) = &self.cache_path
            && !Path::new(d).is_dir()
        {
            return Err(format!("--cache-path: {d} is not a directory"));
        }
        if let Some(l) = &self.log {
            // the server appends to it: only a log file, never a link or another kind of file
            if !l.to_ascii_lowercase().ends_with(".log") {
                return Err("--log: the file name must end in .log".into());
            }
            if !Path::new(l).parent().is_some_and(|p| p.is_dir()) {
                return Err(format!("--log: the folder of {l} does not exist"));
            }
            if std::fs::symlink_metadata(l).is_ok_and(|m| !m.is_file()) {
                return Err(format!("--log: {l} is not a regular file"));
            }
        }
        if let Some(s) = &self.save_config
            && (s.contains('/') || s.starts_with('.'))
        {
            return Err("--save-config: a file name (it is written into each run's output folder)".into());
        }
        Ok(())
    }

    pub fn write(&self, w: &mut W) {
        let opt = |w: &mut W, k: &str, v: &Option<String>| {
            match v {
                Some(s) => w.ks(k, s),
                None => w.key(k).null(),
            };
        };
        let strs = |w: &mut W, k: &str, v: &[String]| {
            w.key(k).arr();
            for s in v {
                w.s(s);
            }
            w.end_arr();
        };
        w.obj();
        opt(w, "config", &self.config);
        opt(w, "parallelism", &self.parallelism);
        strs(w, "extend", &self.extend);
        opt(w, "plugin_dirs", &self.plugin_dirs);
        strs(w, "symbol_dirs", &self.symbol_dirs);
        w.ku("verbosity", self.verbosity as u64);
        opt(w, "log", &self.log);
        opt(w, "output_dir", &self.output_dir);
        w.kb("quiet", self.quiet).kb("write_config", self.write_config);
        opt(w, "save_config", &self.save_config);
        w.kb("clear_cache", self.clear_cache);
        opt(w, "cache_path", &self.cache_path);
        w.kb("offline", self.offline);
        opt(w, "remote_isf_url", &self.remote_isf_url);
        strs(w, "filters", &self.filters);
        strs(w, "hide_columns", &self.hide_columns);
        opt(w, "renderer", &self.renderer);
        opt(w, "single_location", &self.single_location);
        match &self.stackers {
            Some(s) => strs(w, "stackers", s),
            None => {
                w.key("stackers").null();
            }
        }
        strs(w, "single_swap_locations", &self.single_swap_locations);
        w.end_obj();
    }

    /// Fill the options that decide how the image is opened (a change reopens it). The
    /// output folder falls back to `default_out` (the one `fvol serve -o` was given).
    pub fn apply_opening(&self, o: &mut super::session::SessionOpts, default_out: &Path) {
        let (loc, stackers, swaps) = self.automagic();
        o.symbol_dirs = self.symbol_dirs.clone();
        o.offline = self.offline;
        o.remote_isf_url = self.remote_isf_url.clone();
        o.cache_path = self.cache_path.clone();
        o.clear_cache = self.clear_cache;
        o.single_location = loc;
        o.stackers = stackers;
        o.swap_locations = swaps;
        o.verbosity = self.verbosity;
        o.out_root = self.output_dir.as_ref().map(Into::into).unwrap_or_else(|| default_out.to_path_buf());
    }

    /// Whether going from `self` to `new` changes how the image is opened.
    pub fn opening_differs(&self, new: &Options) -> bool {
        let key = |o: &Options| (o.symbol_dirs.clone(), o.offline, o.remote_isf_url.clone(), o.cache_path.clone(), o.clear_cache, o.automagic(), o.verbosity, o.output_dir.clone());
        key(self) != key(new)
    }

    /// Location, stackers and swap files: the options, overridden by `-e automagic.*` keys (as
    /// the CLI applies `-e` after the options).
    pub fn automagic(&self) -> (Option<String>, Option<Vec<String>>, Vec<String>) {
        let (mut loc, mut stackers, mut swaps) = (self.single_location.clone(), self.stackers.clone(), self.single_swap_locations.clone());
        for e in &self.extend {
            let Ok((address, value)) = parse_extend(e) else { continue };
            let strs = |v: &Json| v.as_arr().iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect::<Vec<_>>();
            match address.as_str() {
                "automagic.LayerStacker.single_location" => loc = value.as_str().map(|s| s.to_string()),
                "automagic.LayerStacker.stackers" => stackers = Some(strs(&value)),
                "automagic.WinSwapLayers.single_swap_locations" => swaps = strs(&value),
                _ => {}
            }
        }
        (loc, stackers, swaps)
    }

    /// The run's options with the values `-c` and `-e plugins.<Class>.<option>` give for the
    /// options the run leaves unset (as the CLI: file, then `-e`, then the command line wins).
    pub fn run_args(&self, plugin: &dyn Plugin, args: Option<&Json>) -> Result<Json, String> {
        let mut kv: Vec<(String, Json)> = match args {
            Some(Json::Obj(kv)) => kv.clone(),
            _ => Vec::new(),
        };
        let reqs = plugin.requirements();
        let set = |kv: &mut Vec<(String, Json)>, name: &str, v: Json| {
            if !kv.iter().any(|(k, x)| k == name && !matches!(x, Json::Null)) {
                kv.retain(|(k, _)| k != name);
                kv.push((name.to_string(), v));
            }
        };
        let mut given: Vec<(String, Json)> = Vec::new();
        if let Some(c) = &self.config {
            for (k, v) in read_config(c)? {
                if !k.contains('.') {
                    given.push((k, v));
                }
            }
        }
        let class = plugin.name().rsplit('.').next().unwrap_or("");
        let prefix = format!("plugins.{class}.");
        for e in &self.extend {
            let (address, value) = parse_extend(e)?;
            if let Some(name) = address.strip_prefix(&prefix).filter(|n| !n.contains('.')) {
                given.retain(|(k, _)| k != name);
                given.push((name.to_string(), value));
            }
        }
        for (k, v) in given {
            if reqs.iter().any(|r| r.name == k) {
                set(&mut kv, &k, v);
            }
        }
        Ok(Json::Obj(kv))
    }

    /// The file a run's configuration is written to (`--write-config` / `--save-config`).
    pub fn config_file_name(&self) -> Option<String> {
        match (&self.save_config, self.write_config) {
            (Some(n), _) => Some(n.clone()),
            (None, true) => Some("config.json".into()),
            _ => None,
        }
    }

    pub fn render_options(&self) -> crate::renderers::text::RenderOptions {
        crate::renderers::text::RenderOptions {
            filters: self.filters.clone(),
            hide_columns: if self.hide_columns.is_empty() { None } else { Some(self.hide_columns.clone()) },
            flush_rows: false,
        }
    }

    /// Plugins allowed to run at once: `--parallelism off` runs one at a time.
    pub fn parallel(&self, base: usize) -> usize {
        if self.parallelism.as_deref() == Some("off") { 1 } else { base }
    }
}

/// `-c`: a JSON object of settings (python's HierarchicalDict: no nested objects).
fn read_config(path: &str) -> Result<Vec<(String, Json)>, String> {
    let t = std::fs::read_to_string(path).map_err(|e| format!("--config {path}: {e}"))?;
    match json::parse(&t).map_err(|e| format!("--config {path}: invalid JSON: {}", e.0))? {
        Json::Obj(kv) if kv.iter().all(|(_, v)| !matches!(v, Json::Obj(_))) => Ok(kv),
        _ => Err(format!("--config {path}: expected an object of settings")),
    }
}

/// `-e address=value`, the value being JSON (as the CLI).
pub fn parse_extend(e: &str) -> Result<(String, Json), String> {
    let (address, value) = e.split_once('=').ok_or_else(|| format!("--extend {e}: expected conf.path=value"))?;
    let v = json::parse(value).map_err(|x| format!("--extend {e}: the value is not JSON ({})", x.0))?;
    Ok((address.trim().to_string(), v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_and_round_trip() {
        let cwd = std::env::temp_dir();
        let bad = |s: &str| Options::from_json(&json::parse(s).unwrap(), &cwd).unwrap_err();
        assert!(bad(r#"{"offline": true, "remote_isf_url": "https://x"}"#).contains("together"));
        assert!(bad(r#"{"parallelism": "lots"}"#).contains("choose from"));
        assert!(bad(r#"{"renderer": "xml"}"#).contains("choose from"));
        assert!(bad(r#"{"extend": ["noequals"]}"#).contains("conf.path=value"));
        assert!(bad(r#"{"extend": ["a.b=not json"]}"#).contains("not JSON"));
        assert!(bad(r#"{"log": "/tmp/profile"}"#).contains(".log"));
        let o = Options::from_json(&json::parse(r#"{"offline": true, "symbol_dirs": "a;b", "verbosity": 2, "renderer": "csv", "parallelism": "off", "hide_columns": ["Offset"]}"#).unwrap(), &cwd).unwrap();
        assert_eq!(o.symbol_dirs.len(), 2);
        assert_eq!(o.parallel(3), 1);
        let mut w = W::new();
        o.write(&mut w);
        let back = Options::from_json(&json::parse(&String::from_utf8(w.done()).unwrap()).unwrap(), &cwd).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn extend_automagic_keys() {
        let o = Options { extend: vec![r#"automagic.LayerStacker.stackers=["A","B"]"#.into(), r#"automagic.WinSwapLayers.single_swap_locations=["file:///p"]"#.into()], ..Default::default() };
        let (_, st, sw) = o.automagic();
        assert_eq!(st, Some(vec!["A".to_string(), "B".to_string()]));
        assert_eq!(sw, vec!["file:///p".to_string()]);
        assert_eq!(Options { write_config: true, ..Default::default() }.config_file_name().as_deref(), Some("config.json"));
    }
}

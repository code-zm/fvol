//! Plugin presets the user saves from the web UI: `~/.fvol/presets/<id>.json`, one file each,
//! shared by every image. A preset is a named list of plugins, each with its own options:
//!
//! ```json
//! {"name": "Ransomware sweep", "os": "windows", "created": 1790000000,
//!  "plugins": [{"plugin": "windows.pslist.PsList", "args": {"pid": "4 628"}}]}
//! ```
//!
//! `~/.fvol` also holds the saved analyses, so it is created private (0700). Files are written
//! to a temporary name and renamed, so a crash never leaves half a preset. Read and written with
//! the built-in JSON reader/writer: no dependencies.

use super::jsonw::W;
use crate::cli::json::{self, Json};
use std::path::{Path, PathBuf};

/// Largest preset file read or written (a preset is a short list of plugins).
const MAX_FILE: u64 = 256 << 10;

/// `~/.fvol`: fastvol's own user data (saved analyses, presets); not a cache. Unit tests get a
/// folder of their own, so they never touch the real one.
pub fn fvol_dir() -> PathBuf {
    #[cfg(test)]
    return std::env::temp_dir().join(format!("fastvol-test-home-{}", std::process::id())).join(".fvol");
    #[cfg(not(test))]
    crate::util::paths::home_dir().join(".fvol")
}

pub fn presets_dir() -> PathBuf {
    fvol_dir().join("presets")
}

/// Create `dir` and missing parents, private to the user.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

/// The file id for a preset name: lowercase letters, digits and dashes, at most 64 characters.
pub fn slug(name: &str) -> Option<String> {
    let mut s = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.is_empty() && !s.ends_with('-') {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(64).collect();
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub plugin: String,
    /// option name -> value, as the UI sends it to `POST /api/runs`
    pub args: Vec<(String, Json)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub os: String,
    pub created: u64,
    pub plugins: Vec<Entry>,
}

/// An option value a preset may hold: text, a number, true, or a list of those.
fn plain_value(v: &Json) -> bool {
    match v {
        Json::Str(_) | Json::Int(_) | Json::Bool(true) => true,
        Json::Arr(a) => a.iter().all(|x| matches!(x, Json::Str(_) | Json::Int(_))),
        _ => false,
    }
}

/// Read a preset from its JSON (the file content, or a request body). `known` checks plugin names.
pub fn from_json(id: &str, j: &Json, known: &dyn Fn(&str) -> bool) -> Result<Preset, String> {
    let text = |k: &str| j.get(k).and_then(|v| v.as_str()).map(|s| s.trim().to_string());
    let name = text("name").filter(|n| !n.is_empty()).ok_or("a preset needs a \"name\"")?;
    if name.chars().count() > 80 {
        return Err("the name is longer than 80 characters".into());
    }
    let os = text("os").unwrap_or_default();
    let created = match j.get("created") {
        Some(Json::Int(i)) if *i >= 0 => *i as u64,
        _ => 0,
    };
    let list = match j.get("plugins") {
        Some(Json::Arr(a)) if !a.is_empty() => a,
        _ => return Err("a preset needs at least one plugin in \"plugins\"".into()),
    };
    if list.len() > 200 {
        return Err("a preset can hold at most 200 plugins".into());
    }
    let mut plugins = Vec::with_capacity(list.len());
    for (i, e) in list.iter().enumerate() {
        let plugin = e.get("plugin").and_then(|p| p.as_str()).ok_or_else(|| format!("plugin {}: needs a \"plugin\" name", i + 1))?;
        if !known(plugin) {
            return Err(format!("plugin {}: no plugin named {plugin} in this build", i + 1));
        }
        let args = match e.get("args") {
            None | Some(Json::Null) => Vec::new(),
            Some(Json::Obj(kv)) => {
                if let Some((k, _)) = kv.iter().find(|(_, v)| !plain_value(v)) {
                    return Err(format!("{plugin}: option \"{k}\" must be text, a number, true or a list"));
                }
                kv.clone()
            }
            Some(_) => return Err(format!("{plugin}: \"args\" must be an object")),
        };
        plugins.push(Entry { plugin: plugin.to_string(), args });
    }
    Ok(Preset { id: id.to_string(), name, os, created, plugins })
}

/// The preset as JSON (the file format; the API adds `"id"`).
pub fn write(w: &mut W, p: &Preset, with_id: bool) {
    w.obj();
    if with_id {
        w.ks("id", &p.id);
    }
    w.ks("name", &p.name).ks("os", &p.os).ku("created", p.created);
    w.key("plugins").arr();
    for e in &p.plugins {
        w.obj().ks("plugin", &e.plugin).key("args").obj();
        for (k, v) in &e.args {
            w.key(k).raw(v.dump(None).as_bytes());
        }
        w.end_obj().end_obj();
    }
    w.end_arr().end_obj();
}

/// Every preset file in `dir`, sorted by name; unreadable files come back as `(file, error)`.
pub fn list_in(dir: &Path, known: &dyn Fn(&str) -> bool) -> (Vec<Preset>, Vec<(String, String)>) {
    let (mut ok, mut bad) = (Vec::new(), Vec::new());
    let Ok(rd) = std::fs::read_dir(dir) else { return (ok, bad) };
    for e in rd.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(id) = file.strip_suffix(".json") else { continue };
        if !valid_id(id) {
            continue;
        }
        let r = match e.metadata() {
            Ok(m) if m.len() > MAX_FILE => Err("file too large".to_string()),
            Ok(_) => std::fs::read_to_string(e.path()).map_err(|x| x.to_string()).and_then(|t| json::parse(&t).map_err(|x| format!("invalid JSON: {}", x.0))).and_then(|j| from_json(id, &j, known)),
            Err(x) => Err(x.to_string()),
        };
        match r {
            Ok(p) => ok.push(p),
            Err(x) => bad.push((file, x)),
        }
    }
    ok.sort_by_key(|p| p.name.to_lowercase());
    bad.sort();
    (ok, bad)
}

pub enum SaveError {
    Exists,
    Io(String),
}

/// Write `p` to `dir/<p.id>.json`. Refuses to replace an existing file unless `overwrite`.
pub fn save_in(dir: &Path, p: &Preset, overwrite: bool) -> Result<PathBuf, SaveError> {
    ensure_private_dir(dir).map_err(|e| SaveError::Io(format!("{}: {e}", dir.display())))?;
    let path = dir.join(format!("{}.json", p.id));
    if !overwrite && path.exists() {
        return Err(SaveError::Exists);
    }
    let mut w = W::new();
    write(&mut w, p, false);
    let mut data = w.done();
    data.push(b'\n');
    let tmp = dir.join(format!(".{}.json.tmp-{}", p.id, std::process::id()));
    std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SaveError::Io(format!("{}: {e}", path.display()))
    })?;
    Ok(path)
}

pub fn delete_in(dir: &Path, id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("no such preset".into());
    }
    std::fs::remove_file(dir.join(format!("{id}.json"))).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { "no such preset".into() } else { e.to_string() })
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(n: &str) -> bool {
        n.starts_with("windows.")
    }

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fastvol-presets-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Ransomware sweep #2").as_deref(), Some("ransomware-sweep-2"));
        assert_eq!(slug("  --Weird__Name--  ").as_deref(), Some("weird-name"));
        assert_eq!(slug("!!!"), None);
        assert_eq!(slug(&"a".repeat(100)).unwrap().len(), 64);
    }

    #[test]
    fn validation() {
        let j = |s: &str| json::parse(s).unwrap();
        assert!(from_json("x", &j(r#"{"plugins": [{"plugin": "windows.a.A"}]}"#), &known).unwrap_err().contains("name"));
        assert!(from_json("x", &j(r#"{"name": "n", "plugins": []}"#), &known).unwrap_err().contains("at least one"));
        assert!(from_json("x", &j(r#"{"name": "n", "plugins": [{"plugin": "linux.a.A"}]}"#), &known).unwrap_err().contains("no plugin named"));
        assert!(from_json("x", &j(r#"{"name": "n", "plugins": [{"plugin": "windows.a.A", "args": {"pid": {"x": 1}}}]}"#), &known).unwrap_err().contains("\"pid\""));
        let p = from_json("x", &j(r#"{"name": " n ", "plugins": [{"plugin": "windows.a.A", "args": {"pid": "4 628", "dump": true, "l": [1, "2"]}}]}"#), &known).unwrap();
        assert_eq!(p.name, "n");
        assert_eq!(p.plugins[0].args.len(), 3);
    }

    #[test]
    fn save_list_delete_round_trip() {
        let dir = tmpdir("rt");
        let j = json::parse(r#"{"name": "Triage one", "os": "windows", "plugins": [{"plugin": "windows.pslist.PsList", "args": {"pid": "4"}}, {"plugin": "windows.pslist.PsList", "args": {"dump": true}}]}"#).unwrap();
        let mut p = from_json("triage-one", &j, &known).unwrap();
        p.created = 7;
        assert!(save_in(&dir, &p, false).is_ok());
        assert!(matches!(save_in(&dir, &p, false), Err(SaveError::Exists)));
        assert!(save_in(&dir, &p, true).is_ok());
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        let (ok, bad) = list_in(&dir, &known);
        assert_eq!(ok, vec![p]);
        assert_eq!(bad.len(), 1);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(delete_in(&dir, "triage-one").is_ok());
        assert!(delete_in(&dir, "triage-one").is_err());
        assert!(delete_in(&dir, "../etc").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

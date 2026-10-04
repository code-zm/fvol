//! Request routing and the JSON API.
//!
//! Every `/api/` request needs the access token in the `X-Vol-Token` header (downloads, which
//! a plain link can't give a header, use single-use 60-second tickets bound to one URL), a Host
//! header naming this server, and must not be a cross-site browser request. No cookies: they
//! are not port-isolated, so every other service on 127.0.0.1 would receive them. Nothing here executes anything taken from a request:
//! plugin names are looked up in the registry, options are validated against the plugin's
//! declared requirements, file downloads are matched against a fresh directory listing.

use super::App;
use super::assets;
use super::http::{Body, Method, Request, Response};
use super::picker;
use super::presets;
use super::jsonw::W;
use super::runs::{Run, Status, coltype_name, list_files};
use super::security;
use super::table::{self, CmpSpec, Filter, ViewSpec};
use crate::cli::json::{self, Json};
use crate::plugins::{Config, ConfigValue, Plugin, ReqKind};
use crate::renderers::pyfmt::parse_int0;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Long-lived streams (event stream, row streams) are capped so they can't take every
/// connection; a slot is released when the stream ends.
pub struct StreamSlot;
static STREAMS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_STREAMS: usize = 64;
impl StreamSlot {
    fn take() -> Option<StreamSlot> {
        if STREAMS.fetch_add(1, Ordering::Relaxed) >= MAX_STREAMS {
            STREAMS.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        Some(StreamSlot)
    }
}
impl Drop for StreamSlot {
    fn drop(&mut self) {
        STREAMS.fetch_sub(1, Ordering::Relaxed);
    }
}

fn err(status: u16, msg: &str) -> Response {
    let mut w = W::new();
    w.obj().ks("error", msg).end_obj();
    Response::json(status, w.done())
}

fn ok(w: W) -> Response {
    Response::json(200, w.done())
}

fn qint(req: &Request, name: &str) -> Option<i128> {
    req.param(name).and_then(parse_int0)
}

/// Security headers for every response.
pub fn common_headers(html: bool) -> Vec<(&'static str, &'static str)> {
    let mut h = vec![
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
        ("permissions-policy", "camera=(), microphone=(), geolocation=(), usb=(), serial=(), bluetooth=()"),
    ];
    if html {
        h.push((
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ));
    }
    h
}

pub fn handle(app: &Arc<App>, req: &Request) -> Response {
    let host = match req.header("host") {
        Some(h) if app.hosts.allows(h) => h.to_string(),
        Some(_) => return Response::text(421, "Misdirected request: this server only answers to its own address (DNS rebinding protection)."),
        None => return Response::text(400, "Host header required."),
    };
    let path = req.path.as_str();
    match (req.method, path) {
        // the page itself holds no secret: the token lives in the browser (URL fragment ->
        // localStorage of this exact origin) and goes out in a header on every API call
        (Method::Get | Method::Head, "/") => {
            let html = assets::index_html().replace("{{VERSION}}", crate::VERSION_BANNER);
            Response::new(200).header("cache-control", "no-store").bytes("text/html; charset=utf-8", html.into_bytes())
        }
        (Method::Get | Method::Head, "/favicon.svg") | (Method::Get | Method::Head, "/favicon.ico") => {
            assets::serve("favicon.svg", req)
        }
        (Method::Get | Method::Head, p) if p.starts_with("/assets/") => {
            assets::serve(&p["/assets/".len()..], req)
        }
        (_, p) if p.starts_with("/api/") => {
            if security::cross_site(req, &host) {
                return err(403, "cross-site requests are refused");
            }
            let ok = security::authenticate(req, &app.token) || (matches!(req.method, Method::Get | Method::Head) && app.tickets.redeem(req));
            if !ok {
                // slow down guessing
                let n = app.auth_failures.fetch_add(1, Ordering::Relaxed);
                if n > 20 {
                    std::thread::sleep(Duration::from_millis(250));
                }
                return err(401, "missing or wrong access token (open the URL printed by `fvol serve`)");
            }
            api(app, req, &p["/api/".len()..])
        }
        (Method::Options, _) => Response::text(405, "method not allowed"),
        _ => Response::text(404, "not found"),
    }
}

/// Reject JSON nested deeper than `max` (the parser recurses per level).
fn json_too_deep(text: &str, max: usize) -> bool {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for b in text.bytes() {
        if in_str {
            match (esc, b) {
                (true, _) => esc = false,
                (false, b'\\') => esc = true,
                (false, b'"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > max {
                    return true;
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    false
}

fn body_json(req: &Request) -> Result<Json, Response> {
    let text = std::str::from_utf8(&req.body).map_err(|_| err(400, "request body is not UTF-8"))?;
    if text.trim().is_empty() {
        return Ok(Json::Obj(Vec::new()));
    }
    if json_too_deep(text, 32) {
        return Err(err(400, "JSON nested too deeply"));
    }
    json::parse(text).map_err(|e| err(400, &format!("invalid JSON: {}", e.0)))
}

fn api(app: &Arc<App>, req: &Request, rest: &str) -> Response {
    let segs: Vec<&str> = rest.split('/').collect();
    let m = req.method;
    let get = matches!(m, Method::Get | Method::Head);
    match (segs.as_slice(), get, m) {
        (["session"], true, _) => {
            let mut w = W::new();
            app.session().json(&mut w);
            ok(w)
        }
        (["session"], _, Method::Post) => open_session(app, req),
        (["ticket"], _, Method::Post) => ticket(app, req),
        (["plugins"], true, _) => Response::json(200, app.plugins_json.clone()),
        (["events"], true, _) => events(app),
        (["stats"], true, _) => stats(app),
        (["runs"], true, _) => {
            let mut w = W::new();
            w.arr();
            for r in app.runs.all() {
                r.json(&mut w);
            }
            w.end_arr();
            ok(w)
        }
        (["runs"], _, Method::Post) => create_run(app, req),
        (["batches"], true, _) => {
            let mut w = W::new();
            w.arr();
            for b in app.runs.batches() {
                b.json(&mut w);
            }
            w.end_arr();
            ok(w)
        }
        (["batches"], _, Method::Post) => create_batch(app, req),
        (["batches", id, tail @ ..], _, _) => {
            let Some(b) = id.parse::<u64>().ok().and_then(|i| app.runs.batch(i)) else {
                return err(404, "no such run");
            };
            match (tail, m) {
                ([], Method::Delete) => {
                    app.runs.remove_batch(b.id);
                    err(200, "removed")
                }
                (["cancel"], Method::Post) => {
                    for id in &b.runs {
                        if let Some(r) = app.runs.get(*id) {
                            app.runs.cancel(&r);
                        }
                    }
                    err(200, "cancelled")
                }
                (["filter"], Method::Post) => {
                    let j = match body_json(req) {
                        Ok(j) => j,
                        Err(r) => return r,
                    };
                    match j.get("q").and_then(|q| q.as_str()).filter(|q| q.chars().count() <= 1000) {
                        Some(q) => {
                            app.runs.set_batch_filter(&b, q.to_string());
                            err(200, "saved")
                        }
                        None => err(422, "\"q\" is text of at most 1000 characters"),
                    }
                }
                (["name"], Method::Post) => {
                    let j = match body_json(req) {
                        Ok(j) => j,
                        Err(r) => return r,
                    };
                    match j.get("name").and_then(|n| n.as_str()).map(str::trim).filter(|n| !n.is_empty() && n.chars().count() <= 80) {
                        Some(n) => {
                            app.runs.rename_batch(&b, n.to_string());
                            let mut w = W::new();
                            b.json(&mut w);
                            ok(w)
                        }
                        None => err(422, "a run name is 1 to 80 characters"),
                    }
                }
                _ => err(404, "unknown API endpoint"),
            }
        }
        (["runs", id, tail @ ..], _, _) => {
            let Some(run) = id.parse::<u64>().ok().and_then(|i| app.runs.get(i)) else {
                return err(404, "no such run");
            };
            // a run of a reopened analysis reads its saved rows on first use
            if !tail.is_empty()
                && !matches!(tail, ["cancel"] | ["vol"] | ["files", ..] | ["files.zip"])
                && let Err(e) = app.runs.load_saved(&run)
            {
                return err(500, &e);
            }
            match (tail, m) {
                ([], Method::Get | Method::Head) => {
                    let mut w = W::new();
                    run.json(&mut w);
                    ok(w)
                }
                ([], Method::Delete) => {
                    app.runs.remove(run.id);
                    err(200, "removed")
                }
                (["cancel"], Method::Post) => {
                    app.runs.cancel(&run);
                    let mut w = W::new();
                    run.json(&mut w);
                    ok(w)
                }
                (["view"], Method::Post) => make_view(app, &run, req),
                (["rows"], Method::Get | Method::Head) => rows(&run, req),
                (["stream"], Method::Get) => stream_rows(app, run.clone(), req),
                (["hist"], Method::Get | Method::Head) => hist(&run, req),
                (["export"], Method::Get) => export(&run, req),
                (["vol"], Method::Get) => vol_export(app, run.clone(), req),
                (["files"], Method::Get | Method::Head) => {
                    let mut w = W::new();
                    w.arr();
                    for (n, s) in list_files(&run.out_dir) {
                        w.obj().ks("name", &n).ku("size", s).end_obj();
                    }
                    w.end_arr();
                    ok(w)
                }
                (["files.zip"], Method::Get) => super::zip::download(&run),
                (["files", name], Method::Get | Method::Head) => download(&run, name),
                _ => err(404, "unknown run endpoint"),
            }
        }
        (["mem"], true, _) => mem(app, req),
        (["disasm"], true, _) => disasm(app, req),
        (["fs"], true, _) => fs_list(req),
        (["pick-file"], true, _) => {
            let mut w = W::new();
            let t = picker_tool(app);
            w.obj().kb("available", t.is_some()).ks("tool", t.map_or("", |t| t.name())).end_obj();
            ok(w)
        }
        (["pick-file"], _, Method::Post) => pick_file(app, req),
        (["options"], true, _) => {
            let mut w = W::new();
            app.options.lock().unwrap_or_else(|e| e.into_inner()).write(&mut w);
            ok(w)
        }
        (["options"], _, Method::Post) => {
            let j = match body_json(req) {
                Ok(j) => j,
                Err(r) => return r,
            };
            let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
            let o = match super::options::Options::from_json(&j, &cwd) {
                Ok(o) => o,
                Err(e) => return err(422, &e),
            };
            match app.set_options(o) {
                Ok(reopened) => {
                    let mut w = W::new();
                    w.obj().kb("reopened", reopened).key("options");
                    app.options.lock().unwrap_or_else(|e| e.into_inner()).write(&mut w);
                    w.end_obj();
                    ok(w)
                }
                Err(e) => err(422, &e),
            }
        }
        (["rules"], true, _) => {
            let mut w = W::new();
            match &*app.rules.lock().unwrap_or_else(|e| e.into_inner()) {
                Some((file, text)) => w.obj().ks("file", file).ks("text", text).end_obj(),
                None => w.null(),
            };
            ok(w)
        }
        (["rules"], _, Method::Post) => {
            let j = match body_json(req) {
                Ok(j) => j,
                Err(r) => return r,
            };
            let (Some(file), Some(text)) = (j.get("file").and_then(|f| f.as_str()), j.get("text").and_then(|t| t.as_str())) else {
                return err(422, "{\"file\", \"text\"} are required");
            };
            if json::parse(text).is_err() {
                return err(422, "the rules file is not valid JSON");
            }
            *app.rules.lock().unwrap_or_else(|e| e.into_inner()) = Some((file.chars().take(200).collect(), text.to_string()));
            app.hub.bump();
            err(200, "saved")
        }
        (["rules"], _, Method::Delete) => {
            *app.rules.lock().unwrap_or_else(|e| e.into_inner()) = None;
            app.hub.bump();
            err(200, "removed")
        }
        (["analyses", id], _, Method::Delete) => match app.delete_analysis(id) {
            Ok(()) => err(200, "removed"),
            Err(e) => err(404, &e),
        },
        (["analyses"], true, _) => {
            let mut w = W::new();
            super::analysis::list(&mut w);
            ok(w)
        }
        (["presets"], true, _) => list_presets(app),
        (["presets"], _, Method::Post) => save_preset(app, req),
        (["presets", id], _, Method::Delete) => match presets::delete_in(&presets::presets_dir(), id) {
            Ok(()) => err(200, "removed"),
            Err(e) => err(404, &e),
        },
        _ => err(404, "unknown API endpoint"),
    }
}

// ------------------------------------------------------------------------------------------
// session

fn open_session(app: &Arc<App>, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let Some(file) = j.get("file").and_then(|f| f.as_str()) else { return err(422, "\"file\" (path of a memory image) is required") };
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let path = cwd.join(file);
    let path = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(e) => return err(422, &format!("{}: {e}", path.display())),
    };
    match std::fs::metadata(&path) {
        Ok(md) if md.is_file() => {}
        Ok(_) => return err(422, &format!("{} is not a regular file", path.display())),
        Err(e) => return err(422, &format!("{}: {e}", path.display())),
    }
    if let Err(e) = std::fs::File::open(&path) {
        return err(422, &format!("{} can't be read: {e}", path.display()));
    }
    let opened = app.open(path).and_then(|s| {
        // `symbol_dirs` in the request (kept for scripts) is set like the Options dialog's -s
        match j.get("symbol_dirs") {
            Some(Json::Arr(dirs)) => {
                let mut o = app.options.lock().unwrap_or_else(|e| e.into_inner()).clone();
                o.symbol_dirs = dirs.iter().filter_map(|d| d.as_str()).map(|d| cwd.join(d).to_string_lossy().into_owned()).collect();
                app.set_options(o)?;
                Ok(app.session())
            }
            _ => Ok(s),
        }
    });
    match opened {
        Ok(s) => {
            let mut w = W::new();
            s.json(&mut w);
            ok(w)
        }
        Err(e) => err(422, &e),
    }
}

fn stats(app: &Arc<App>) -> Response {
    let runs = app.runs.all();
    let bytes: usize = runs.iter().map(|r| r.read().table.bytes()).sum();
    let running = runs.iter().filter(|r| r.read().status == Status::Running).count();
    let mut w = W::new();
    w.obj().ku("runs", runs.len() as u64).ku("running", running as u64).ku("table_bytes", bytes as u64).ku("uptime_ms", app.started.elapsed().as_millis() as u64).ku("max_parallel", app.runs.max_parallel as u64).end_obj();
    ok(w)
}

// ------------------------------------------------------------------------------------------
// plugins

/// JSON description of every plugin and its options (drives the palette and option forms).
pub fn plugins_json(plugins: &[&'static dyn Plugin]) -> Vec<u8> {
    let mut w = W::new();
    w.arr();
    for p in plugins {
        let name = p.name();
        let os = match name.split('.').next() {
            Some(o @ ("windows" | "linux" | "mac")) => o,
            _ => "generic",
        };
        w.obj();
        w.ks("name", name).ks("os", os).ks("description", p.description().trim());
        w.key("epilog").opt_s(p.epilog());
        w.key("reqs").arr();
        for r in p.requirements() {
            w.obj();
            w.ks("name", r.name).ks("flag", &format!("--{}", r.name.replace('_', "-"))).ks("description", r.description);
            let kind = match &r.kind {
                ReqKind::Bool => "bool",
                ReqKind::Int => "int",
                ReqKind::Str => "str",
                ReqKind::Bytes => "bytes",
                ReqKind::Uri => "uri",
                ReqKind::ListInt => "list_int",
                ReqKind::ListStr => "list_str",
                ReqKind::Choice(_) => "choice",
            };
            w.ks("kind", kind).kb("optional", r.optional);
            if let ReqKind::Choice(c) = &r.kind {
                w.key("choices").arr();
                for x in c {
                    w.s(x);
                }
                w.end_arr();
            }
            w.key("default");
            match &r.default {
                None => {
                    w.null();
                }
                Some(v) => cv_json(&mut w, v),
            }
            w.end_obj();
        }
        w.end_arr();
        w.end_obj();
    }
    w.end_arr();
    w.done()
}

fn cv_json(w: &mut W, v: &ConfigValue) {
    match v {
        ConfigValue::Bool(b) => {
            w.b(*b);
        }
        ConfigValue::Int(i) => {
            w.i(*i);
        }
        ConfigValue::Str(s) => {
            w.s(s);
        }
        ConfigValue::Bytes(b) => {
            w.sb(b);
        }
        ConfigValue::List(l) => {
            w.arr();
            for x in l {
                cv_json(w, x);
            }
            w.end_arr();
        }
    }
}

fn py_repr_str(s: &str) -> String {
    crate::renderers::pyfmt::str_repr(s)
}

/// Validate browser-supplied options against the plugin's requirements (python `int(x, 0)`
/// for ints, choices, required options) and build the `Config` + the equivalent `fvol` args.
pub fn parse_config(plugin: &dyn Plugin, args: Option<&Json>) -> Result<(Config, Vec<String>), String> {
    let reqs = plugin.requirements();
    let mut cfg = Config::default();
    let mut argv = Vec::new();
    let empty = Vec::new();
    let items = match args {
        None | Some(Json::Null) => &empty,
        Some(Json::Obj(items)) => items,
        Some(_) => return Err("\"args\" must be an object".into()),
    };
    for (k, _) in items {
        if !reqs.iter().any(|r| r.name == k) {
            return Err(format!("{} has no option {}", plugin.name(), py_repr_str(k)));
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    for r in &reqs {
        let flag = format!("--{}", r.name.replace('_', "-"));
        let v = items.iter().rev().find(|(k, _)| k == r.name).map(|(_, v)| v);
        let is_unset = |v: &Json| match v {
            Json::Null => true,
            Json::Str(s) => s.trim().is_empty(),
            Json::Arr(a) => a.is_empty(),
            Json::Bool(b) => !*b && r.kind == ReqKind::Bool,
            _ => false,
        };
        let int = |x: &Json| -> Result<i128, String> {
            match x {
                Json::Int(i) => Ok(*i),
                Json::Str(s) => parse_int0(s).ok_or_else(|| format!("argument {flag}: invalid int value: {}", py_repr_str(s))),
                _ => Err(format!("argument {flag}: expected an integer")),
            }
        };
        let words = |x: &Json| -> Vec<Json> {
            match x {
                Json::Arr(a) => a.clone(),
                Json::Str(s) => s.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty()).map(|w| Json::Str(w.to_string())).collect(),
                other => vec![other.clone()],
            }
        };
        let Some(v) = v.filter(|v| !is_unset(v)) else {
            if !r.optional && r.default.is_none() {
                return Err(format!("the following arguments are required: {flag}"));
            }
            continue;
        };
        let cv = match &r.kind {
            ReqKind::Bool => match v {
                Json::Bool(true) => ConfigValue::Bool(true),
                Json::Str(s) if matches!(s.as_str(), "true" | "on" | "1") => ConfigValue::Bool(true),
                _ => return Err(format!("argument {flag}: expected true/false")),
            },
            ReqKind::Int => match v {
                // a one-element list is fine too (a PID picker hands over lists)
                Json::Arr(a) if a.len() == 1 => ConfigValue::Int(int(&a[0])?),
                v => ConfigValue::Int(int(v)?),
            },
            ReqKind::ListInt => ConfigValue::List(words(v).iter().map(int).collect::<Result<Vec<_>, _>>()?.into_iter().map(ConfigValue::Int).collect()),
            ReqKind::Str => ConfigValue::Str(v.as_str().ok_or_else(|| format!("argument {flag}: expected text"))?.to_string()),
            ReqKind::ListStr => ConfigValue::List(
                words(v).iter().map(|x| x.as_str().map(|s| ConfigValue::Str(s.to_string())).ok_or_else(|| format!("argument {flag}: expected text"))).collect::<Result<Vec<_>, _>>()?,
            ),
            ReqKind::Choice(c) => {
                let s = v.as_str().ok_or_else(|| format!("argument {flag}: expected text"))?;
                if !c.contains(&s) {
                    let list: Vec<String> = c.iter().map(|x| py_repr_str(x)).collect();
                    return Err(format!("argument {flag}: invalid choice: {} (choose from {})", py_repr_str(s), list.join(", ")));
                }
                ConfigValue::Str(s.to_string())
            }
            ReqKind::Uri => {
                let s = v.as_str().ok_or_else(|| format!("argument {flag}: expected a path or URL"))?;
                let has_scheme = s.find(':').is_some_and(|i| i > 1 && s[..i].bytes().all(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c)));
                if has_scheme && !s.to_ascii_lowercase().starts_with("file:") {
                    return Err(format!("argument {flag}: remote URLs are disabled in fvol serve (the server would fetch them); download the file and give its local path"));
                } else if has_scheme {
                    ConfigValue::Str(s.to_string())
                } else {
                    let p = cwd.join(s);
                    if !p.exists() {
                        return Err(format!("argument {flag}: file {} does not exist", p.display()));
                    }
                    let abs = crate::cli::abspath(&p.to_string_lossy(), "/");
                    ConfigValue::Str(format!("file://{}", super::session::quote_path(&abs)))
                }
            }
            ReqKind::Bytes => return Err(format!("argument {flag}: bytes options can't be given on the command line (volatility3 rejects them too)")),
        };
        // the equivalent command line
        match &cv {
            ConfigValue::Bool(_) => argv.push(flag.clone()),
            ConfigValue::Int(i) => {
                argv.push(flag.clone());
                argv.push(i.to_string());
            }
            ConfigValue::Str(s) => {
                argv.push(flag.clone());
                argv.push(s.clone());
            }
            ConfigValue::List(l) => {
                argv.push(flag.clone());
                for x in l {
                    match x {
                        ConfigValue::Int(i) => argv.push(i.to_string()),
                        ConfigValue::Str(s) => argv.push(s.clone()),
                        _ => {}
                    }
                }
            }
            ConfigValue::Bytes(_) => {}
        }
        cfg.set(r.name, cv);
    }
    for r in &reqs {
        if cfg.get(r.name).is_none()
            && let Some(d) = &r.default
        {
            cfg.set(r.name, d.clone());
        }
    }
    Ok((cfg, argv))
}

pub fn config_key(plugin: &dyn Plugin, cfg: &Config) -> String {
    let mut items: Vec<(&String, String)> = cfg.values.iter().map(|(k, v)| (k, format!("{v:?}"))).collect();
    items.sort();
    let mut s = plugin.name().to_string();
    for (k, v) in items {
        s.push('|');
        s.push_str(k);
        s.push('=');
        s.push_str(&v);
    }
    s
}

fn create_run(app: &Arc<App>, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let Some(name) = j.get("plugin").and_then(|p| p.as_str()) else { return err(422, "\"plugin\" is required") };
    let Some(plugin) = app.plugins.iter().copied().find(|p| p.name() == name) else { return err(404, &format!("unknown plugin {name}")) };
    let args = match app.options.lock().unwrap_or_else(|e| e.into_inner()).run_args(plugin, j.get("args")) {
        Ok(a) => a,
        Err(e) => return err(422, &e),
    };
    let (cfg, argv) = match parse_config(plugin, Some(&args)) {
        Ok(x) => x,
        Err(e) => return err(422, &e),
    };
    let session = app.session();
    if session.image.is_none() {
        return err(409, "open a memory image first");
    }
    let origin = j.get("origin").and_then(|o| o.as_str()).filter(|o| o.len() <= 24 && o.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')).unwrap_or("user");
    let key = config_key(plugin, &cfg);
    let reuse = matches!(j.get("reuse"), Some(Json::Bool(true)));
    let (run, reused) = match app.runs.find_reusable(session.id, &key).filter(|_| reuse) {
        Some(r) => (r, true),
        None => {
            let r = app.runs.create(session, plugin, cfg, argv, key, origin);
            *r.req_args.lock().unwrap_or_else(|e| e.into_inner()) = args;
            app.runs.submit(r.clone());
            (r, false)
        }
    };
    let mut w = W::new();
    w.obj().ku("id", run.id).kb("reused", reused).key("run");
    run.json(&mut w);
    w.end_obj();
    ok(w)
}

/// Start a run of several plugins: `{"name": "...", "entries": [{"plugin", "args"}]}`. Every
/// entry's options are checked first, so a mistake starts nothing.
fn create_batch(app: &Arc<App>, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let session = app.session();
    if session.image.is_none() {
        return err(409, "open a memory image first");
    }
    let entries = match j.get("entries") {
        Some(Json::Arr(a)) if !a.is_empty() && a.len() <= 200 => a,
        _ => return err(422, "\"entries\" must list 1 to 200 plugins"),
    };
    let mut parsed = Vec::with_capacity(entries.len());
    for e in entries {
        let Some(name) = e.get("plugin").and_then(|p| p.as_str()) else { return err(422, "every entry needs a \"plugin\"") };
        let Some(plugin) = app.plugins.iter().copied().find(|p| p.name() == name) else { return err(404, &format!("unknown plugin {name}")) };
        let args = match app.options.lock().unwrap_or_else(|e| e.into_inner()).run_args(plugin, e.get("args")) {
            Ok(a) => a,
            Err(x) => return err(422, &format!("{name}: {x}")),
        };
        match parse_config(plugin, Some(&args)) {
            Ok((cfg, argv)) => parsed.push((plugin, cfg, argv, args)),
            Err(x) => return err(422, &format!("{name}: {x}")),
        }
    }
    let n = app.runs.batches().iter().filter(|b| b.session == session.id).count() + 1;
    let name = j.get("name").and_then(|n| n.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(80).collect()).unwrap_or_else(|| format!("Run {n}"));
    let runs = parsed.into_iter().map(|(plugin, cfg, argv, args)| {
        let key = config_key(plugin, &cfg);
        let r = app.runs.create(session.clone(), plugin, cfg, argv, key, "user");
        *r.req_args.lock().unwrap_or_else(|e| e.into_inner()) = args;
        r
    }).collect();
    let b = app.runs.submit_batch(session.id, name, runs);
    let mut w = W::new();
    b.json(&mut w);
    ok(w)
}

// ------------------------------------------------------------------------------------------
// views and rows

fn parse_spec(j: &Json, run: &Run, types: &[crate::renderers::ColType], app: &Arc<App>) -> Result<ViewSpec, String> {
    let mut spec = ViewSpec { tree: true, ..Default::default() };
    let ncols = types.len();
    if let Some(q) = j.get("q").and_then(|q| q.as_str()) {
        spec.q = q.trim().to_ascii_lowercase().into_bytes();
    }
    let col = |x: &Json| -> Result<usize, String> {
        match x {
            Json::Int(i) if *i >= 0 && (*i as usize) < ncols => Ok(*i as usize),
            Json::Str(s) => s.parse::<usize>().ok().filter(|&c| c < ncols).ok_or_else(|| format!("bad column {s}")),
            _ => Err("bad column".into()),
        }
    };
    if let Some(Json::Arr(v)) = j.get("visible") {
        spec.visible = v.iter().map(col).collect::<Result<_, _>>()?;
    }
    if let Some(Json::Obj(cols)) = j.get("cols") {
        for (k, v) in cols {
            let c = col(&Json::Str(k.clone()))?;
            if let Some(e) = v.as_str().filter(|e| !e.trim().is_empty()) {
                let f = Filter::parse(e, types[c]).map_err(|m| format!("filter on {}: {m}", run.read().columns.get(c).map(|c| c.name.clone()).unwrap_or_default()))?;
                spec.filters.push((c, f));
            }
        }
    }
    if let Some(Json::Arr(s)) = j.get("sort") {
        for item in s {
            let a = item.as_arr();
            if a.is_empty() {
                continue;
            }
            let c = col(&a[0])?;
            let desc = a.get(1).and_then(|d| d.as_str()) == Some("desc");
            spec.sort.push((c, desc));
        }
    }
    if let Some(r) = j.get("range")
        && !matches!(r, Json::Null)
    {
        let cols: Vec<usize> = match (r.get("cols"), r.get("col")) {
            (Some(Json::Arr(a)), _) => a.iter().map(col).collect::<Result<_, _>>()?,
            (_, Some(c)) => vec![col(c)?],
            _ => return Err("range needs col or cols".into()),
        };
        let from = r.get("from").and_then(|x| x.as_str()).unwrap_or("").as_bytes().to_vec();
        let to = r.get("to").and_then(|x| x.as_str()).unwrap_or("").as_bytes().to_vec();
        spec.range = Some((cols, from, to));
    }
    if let Some(Json::Bool(t)) = j.get("tree") {
        spec.tree = *t;
    }
    if let Some(c) = j.get("cmp")
        && !matches!(c, Json::Null)
    {
        let other_id = match c.get("run") {
            Some(Json::Int(i)) => *i as u64,
            _ => return Err("cmp.run is required".into()),
        };
        let other = app.runs.get(other_id).ok_or("the run to compare with no longer exists")?;
        let keys: Vec<usize> = c.get("keys").map(|k| k.as_arr().iter().map(col).collect::<Result<_, _>>()).transpose()?.unwrap_or_default();
        let od = other.read();
        let on = od.table.ncols;
        let okeys: Vec<usize> = c
            .get("other_keys")
            .map(|k| k.as_arr().iter().filter_map(|x| if let Json::Int(i) = x { Some(*i as usize) } else { None }).filter(|&i| i < on).collect())
            .unwrap_or_default();
        if keys.is_empty() || keys.len() != okeys.len() {
            return Err("compare needs the same number of key columns on both sides".into());
        }
        let set = table::key_set(&od.table, &okeys);
        drop(od);
        let mode = match c.get("mode").and_then(|m| m.as_str()) {
            Some("only") => 1,
            Some("common") => 2,
            _ => 0,
        };
        spec.cmp = Some(CmpSpec { keys, other: set, mode });
    }
    Ok(spec)
}

fn make_view(app: &Arc<App>, run: &Arc<Run>, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let t0 = Instant::now();
    run.last_access.store(super::runs::now_ms(), Ordering::Relaxed);
    if run.read().evicted {
        return err(410, "this result was dropped from memory to make room for newer ones; run it again");
    }
    let types = run.read().types.clone();
    let spec = match parse_spec(&j, run, &types, app) {
        Ok(s) => s,
        Err(e) => return err(422, &e),
    };
    let key = String::from_utf8_lossy(&req.body).into_owned();
    if spec.is_identity() {
        let n = run.read().table.rows() as u64;
        let mut w = W::new();
        w.obj().ku("view", 0).ku("total", n).ku("matched", n).ku("built", n).ku("ms", 0).end_obj();
        return ok(w);
    }
    let d = run.read();
    let rows_now = d.table.rows();
    // reuse an identical view built over the same rows
    let cached = run.views.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(_, k, v)| *k == key && v.built == rows_now).map(|(vid, _, v)| (*vid, v.clone()));
    let (vid, view) = match cached {
        Some(x) => x,
        None => {
            let v = Arc::new(table::build_view(&d.table, &d.types, &spec));
            drop(d);
            let vid = run.next_view.fetch_add(1, Ordering::Relaxed);
            let mut views = run.views.lock().unwrap_or_else(|e| e.into_inner());
            views.push((vid, key, v.clone()));
            if views.len() > 12 {
                views.remove(0);
            }
            (vid, v)
        }
    };
    let mut w = W::new();
    w.obj().ku("view", vid).ku("total", view.total as u64).ku("matched", view.matched as u64).ku("built", view.built as u64).ku("ms", t0.elapsed().as_millis() as u64).end_obj();
    ok(w)
}

fn find_view(run: &Run, vid: u64) -> Option<Arc<table::View>> {
    run.views.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|(v, _, _)| *v == vid).map(|(_, _, v)| v.clone())
}

fn rows(run: &Arc<Run>, req: &Request) -> Response {
    run.last_access.store(super::runs::now_ms(), Ordering::Relaxed);
    if run.read().evicted {
        return err(410, "this result was dropped from memory to make room for newer ones; run it again");
    }
    let vid = qint(req, "view").unwrap_or(0).max(0) as u64;
    let from = qint(req, "from").unwrap_or(0).max(0) as usize;
    let count = qint(req, "count").unwrap_or(200).clamp(0, 5000) as usize;
    let view = if vid == 0 {
        None
    } else {
        match find_view(run, vid) {
            Some(v) => Some(v),
            None => return err(410, "view expired; create it again"),
        }
    };
    let d = run.read();
    let t = &d.table;
    let total = view.as_ref().map(|v| v.total).unwrap_or(t.rows());
    let end = (from + count).min(total);
    let mut out = Vec::with_capacity(64 + (end.saturating_sub(from)) * 16 * t.ncols.max(1));
    out.extend_from_slice(b"{\"total\":");
    out.extend_from_slice(total.to_string().as_bytes());
    out.extend_from_slice(b",\"stored\":");
    out.extend_from_slice(t.rows().to_string().as_bytes());
    out.extend_from_slice(b",\"from\":");
    out.extend_from_slice(from.to_string().as_bytes());
    out.extend_from_slice(b",\"rows\":[");
    for i in from..end {
        let (r, mark) = match &view {
            Some(v) => (v.row(i), v.mark(i)),
            None => (i, 0),
        };
        if r >= t.rows() {
            break;
        }
        if i > from {
            out.push(b',');
        }
        table::row_json(&mut out, t, r, mark);
    }
    out.extend_from_slice(b"]}");
    Response::json(200, out)
}

/// NDJSON stream of every row as it is produced (for scripts and tests; the UI pages through
/// `/rows`). Lines: {"t":"cols"}, {"t":"rows"}, {"t":"hb"}, finally {"t":"end"}.
fn stream_rows(app: &Arc<App>, run: Arc<Run>, req: &Request) -> Response {
    let mut sent = qint(req, "from").unwrap_or(0).max(0) as usize;
    let hub = app.hub.clone();
    let Some(slot) = StreamSlot::take() else { return err(503, "too many open streams") };
    Response::new(200).header("cache-control", "no-store").stream(
        "application/x-ndjson; charset=utf-8",
        Box::new(move |w: &mut dyn Write| {
            let _slot = slot;
            let mut cols_sent = false;
            let mut seen = 0u64;
            loop {
                let mut out = Vec::new();
                let finished;
                {
                    let d = run.read();
                    if !cols_sent && (!d.columns.is_empty() || d.status.finished()) {
                        let mut j = W::new();
                        j.obj().ks("t", "cols").key("cols").arr();
                        for c in &d.columns {
                            j.obj().ks("name", &c.name).ks("type", coltype_name(c.ty)).end_obj();
                        }
                        j.end_arr().end_obj();
                        out.extend_from_slice(&j.done());
                        out.push(b'\n');
                        cols_sent = true;
                    }
                    let n = d.table.rows();
                    while sent < n {
                        let end = (sent + 2000).min(n);
                        out.extend_from_slice(b"{\"t\":\"rows\",\"from\":");
                        out.extend_from_slice(sent.to_string().as_bytes());
                        out.extend_from_slice(b",\"rows\":[");
                        for r in sent..end {
                            if r > sent {
                                out.push(b',');
                            }
                            table::row_json(&mut out, &d.table, r, 0);
                        }
                        out.extend_from_slice(b"]}\n");
                        sent = end;
                    }
                    finished = d.status.finished() && !d.busy;
                }
                if finished {
                    let mut j = W::new();
                    j.obj().ks("t", "end").key("run");
                    run.json(&mut j);
                    j.end_obj();
                    out.extend_from_slice(&j.done());
                    out.push(b'\n');
                    w.write_all(&out)?;
                    return w.flush();
                }
                if !out.is_empty() {
                    w.write_all(&out)?;
                    w.flush()?;
                }
                let s = hub.wait(seen, Duration::from_secs(10));
                if s == seen {
                    w.write_all(b"{\"t\":\"hb\"}\n")?;
                    w.flush()?;
                }
                seen = s;
            }
        }),
    )
}

fn hist(run: &Arc<Run>, req: &Request) -> Response {
    let vid = qint(req, "view").unwrap_or(0).max(0) as u64;
    let buckets = qint(req, "buckets").unwrap_or(120).clamp(1, 2000) as usize;
    let view = if vid == 0 { None } else { find_view(run, vid) };
    let d = run.read();
    // col=N, col=2,3,4 or col=all (every DateTime column)
    let cols: Vec<usize> = match req.param("col").unwrap_or("") {
        "all" => d.types.iter().enumerate().filter(|(_, t)| **t == crate::renderers::ColType::DateTime).map(|(i, _)| i).collect(),
        s => s.split(',').filter_map(|x| x.trim().parse::<usize>().ok()).filter(|&c| c < d.table.ncols).collect(),
    };
    if cols.is_empty() {
        return err(422, "bad column");
    }
    let by = qint(req, "by").filter(|b| *b >= 0 && (*b as usize) < d.table.ncols).map(|b| b as usize);
    let ident;
    let v = match &view {
        Some(v) => v.as_ref(),
        None => {
            ident = table::View { rows: None, marks: Vec::new(), total: d.table.rows(), built: d.table.rows(), matched: d.table.rows() };
            &ident
        }
    };
    let mut w = W::new();
    let window = match (req.param("from").and_then(|x| x.parse::<f64>().ok()), req.param("to").and_then(|x| x.parse::<f64>().ok())) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    match table::histogram(&d.table, v, &cols, buckets, by, 6, window) {
        None => {
            w.obj().kb("empty", true).end_obj();
        }
        Some(h) => {
            w.obj().key("min").f(h.lo).key("max").f(h.hi).ku("below", h.below).ku("above", h.above).kb("focused", h.focused).key("counts").arr();
            for c in &h.counts {
                w.u(*c as u64);
            }
            w.end_arr();
            w.key("cats").arr();
            for c in &h.cats {
                w.sb(c);
            }
            w.end_arr();
            w.key("stacks").arr();
            for s in &h.stacks {
                w.arr();
                for c in s {
                    w.u(*c as u64);
                }
                w.end_arr();
            }
            w.end_arr().end_obj();
        }
    }
    ok(w)
}

fn attachment_name(run: &Run, ext: &str) -> String {
    let short = run.plugin.name().rsplit('.').nth(1).unwrap_or("result");
    format!("{short}-run{}.{ext}", run.id)
}

/// Download the current view as csv / tsv / json / jsonl / md (visible columns only).
fn export(run: &Arc<Run>, req: &Request) -> Response {
    let fmt = req.param("format").unwrap_or("csv").to_string();
    if !matches!(fmt.as_str(), "csv" | "tsv" | "json" | "jsonl" | "md") {
        return err(422, "format must be csv, tsv, json, jsonl or md");
    }
    let vid = qint(req, "view").unwrap_or(0).max(0) as u64;
    let view = if vid == 0 {
        None
    } else {
        match find_view(run, vid) {
            Some(v) => Some(v),
            None => return err(410, "view expired; create it again"),
        }
    };
    let ncols = run.read().table.ncols;
    let cols: Vec<usize> = match req.param("cols") {
        Some(c) if !c.is_empty() => c.split(',').filter_map(|x| x.parse::<usize>().ok()).filter(|&x| x < ncols).collect(),
        _ => (0..ncols).collect(),
    };
    let name = attachment_name(run, &fmt);
    let ctype = match fmt.as_str() {
        "csv" => "text/csv; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "jsonl" => "application/x-ndjson; charset=utf-8",
        "md" => "text/markdown; charset=utf-8",
        _ => "text/tab-separated-values; charset=utf-8",
    };
    let run = run.clone();
    Response::new(200).header("content-disposition", format!("attachment; filename=\"{name}\"")).header("cache-control", "no-store").stream(
        ctype,
        Box::new(move |w: &mut dyn Write| {
            let d = run.read();
            let t = &d.table;
            let tree = t.depth.iter().any(|&x| x > 0);
            let total = view.as_ref().map(|v| v.total).unwrap_or(t.rows());
            let names: Vec<&str> = cols.iter().map(|&c| d.columns[c].name.as_str()).collect();
            let mut out = Vec::with_capacity(1 << 16);
            let sep = if fmt == "tsv" { b'\t' } else { b',' };
            let mut first = true;
            match fmt.as_str() {
                "csv" | "tsv" => {
                    if tree {
                        out.extend_from_slice(b"TreeDepth");
                        out.push(sep);
                    }
                    for (i, n) in names.iter().enumerate() {
                        if i > 0 {
                            out.push(sep);
                        }
                        table::csv_field(&mut out, n.as_bytes(), sep);
                    }
                    out.extend_from_slice(b"\r\n");
                }
                "json" => out.extend_from_slice(b"[\n"),
                "md" => {
                    out.push(b'|');
                    for n in &names {
                        out.push(b' ');
                        md_cell(&mut out, n.as_bytes());
                        out.extend_from_slice(b" |");
                    }
                    out.extend_from_slice(b"\n|");
                    for _ in &names {
                        out.extend_from_slice(b"---|");
                    }
                    out.push(b'\n');
                }
                _ => {}
            }
            for i in 0..total {
                let (r, mark) = match &view {
                    Some(v) => (v.row(i), v.mark(i)),
                    None => (i, 0),
                };
                if r >= t.rows() || mark & table::M_CONTEXT != 0 {
                    continue;
                }
                match fmt.as_str() {
                    "csv" | "tsv" => {
                        if tree {
                            out.extend_from_slice(t.depth[r].to_string().as_bytes());
                            out.push(sep);
                        }
                        for (k, &c) in cols.iter().enumerate() {
                            if k > 0 {
                                out.push(sep);
                            }
                            let mut nb = table::NumBuf::default();
                            table::csv_field(&mut out, t.text(r, c, &mut nb), sep);
                        }
                        out.extend_from_slice(b"\r\n");
                    }
                    "json" | "jsonl" => {
                        if fmt == "json" {
                            if !first {
                                out.extend_from_slice(b",\n");
                            }
                            out.extend_from_slice(b"  ");
                        }
                        first = false;
                        out.push(b'{');
                        if tree {
                            out.extend_from_slice(b"\"__depth\": ");
                            out.extend_from_slice(t.depth[r].to_string().as_bytes());
                            out.extend_from_slice(b", ");
                        }
                        for (k, &c) in cols.iter().enumerate() {
                            if k > 0 {
                                out.extend_from_slice(b", ");
                            }
                            super::jsonw::str(&mut out, names[k]);
                            out.extend_from_slice(b": ");
                            table::cell_json(&mut out, t, r, c, d.types[c]);
                        }
                        out.push(b'}');
                        if fmt == "jsonl" {
                            out.push(b'\n');
                        }
                    }
                    _ => {
                        out.push(b'|');
                        for (k, &c) in cols.iter().enumerate() {
                            out.push(b' ');
                            if k == 0 && tree {
                                for _ in 0..t.depth[r] {
                                    out.extend_from_slice("\u{2003}".as_bytes());
                                }
                            }
                            let mut nb = table::NumBuf::default();
                            md_cell(&mut out, t.text(r, c, &mut nb));
                            out.extend_from_slice(b" |");
                        }
                        out.push(b'\n');
                    }
                }
                if out.len() >= 1 << 16 {
                    w.write_all(&out)?;
                    out.clear();
                }
            }
            if fmt == "json" {
                out.extend_from_slice(b"\n]\n");
            }
            w.write_all(&out)
        }),
    )
}

fn md_cell(out: &mut Vec<u8>, s: &[u8]) {
    for &c in s {
        match c {
            b'|' => out.extend_from_slice(b"\\|"),
            b'\n' => out.extend_from_slice(b"<br>"),
            b'\r' => {}
            _ => out.push(c),
        }
    }
}

/// Re-run the plugin with a CLI renderer: byte-identical to `fvol -r <renderer> <plugin> ...`.
fn vol_export(app: &Arc<App>, run: Arc<Run>, req: &Request) -> Response {
    let opts = app.options.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let renderer = req.param("renderer").map(str::to_string).or_else(|| opts.renderer.clone()).unwrap_or_else(|| "quick".into());
    let ropts = opts.render_options();
    if !crate::renderers::text::RENDERER_NAMES.contains(&renderer.as_str()) {
        return err(422, "unknown renderer");
    }
    let ext = match renderer.as_str() {
        "csv" => "csv",
        "json" => "json",
        "jsonl" => "jsonl",
        _ => "txt",
    };
    let name = attachment_name(&run, ext).replace("-run", "-vol-run");
    let n = app.export_seq.fetch_add(1, Ordering::Relaxed);
    let dir = run.session.out_root.join(format!("export-{:04}-{n}", run.id)).to_string_lossy().into_owned();
    let disposition = if req.param("inline").is_some() { "inline".to_string() } else { format!("attachment; filename=\"{name}\"") };
    Response::new(200).header("content-disposition", disposition).header("cache-control", "no-store").stream(
        "text/plain; charset=utf-8",
        Box::new(move |w: &mut dyn Write| {
            if !crate::renderers::text::is_structured(&renderer) {
                w.write_all(format!("{}\n", crate::VERSION_BANNER).as_bytes())?;
            }
            // like the run itself: a plugin that writes files gets a Context whose output_dir is
            // this export's own directory
            let ctx = if super::runs::writes_files(run.plugin, &run.cfg) {
                let mut o = run.session.ctx.opts.clone();
                o.output_dir = dir.clone();
                o.clear_cache = false;
                crate::context::Context::new(o).map(Arc::new).unwrap_or_else(|_| run.session.ctx.clone())
            } else {
                run.session.ctx.clone()
            };
            let mut r = crate::renderers::text::create(&renderer, w, ropts).ok_or_else(|| std::io::Error::other("renderer"))?;
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| crate::context::with_output_dir(&dir, || run.plugin.run(&ctx, &run.cfg, &mut *r))));
            match res {
                Ok(Ok(())) => r.finish().map_err(|e| std::io::Error::other(e.to_string())),
                Ok(Err(e)) => {
                    let _ = r.abort(matches!(e, crate::error::Error::Unsatisfied(_)));
                    Err(std::io::Error::other(e.to_string()))
                }
                Err(_) => {
                    let _ = r.abort(false);
                    Err(std::io::Error::other("plugin panicked"))
                }
            }
        }),
    )
}

/// Download one file the run wrote. The name must match an entry of a fresh listing of the
/// run's own output directory: no paths, no traversal, no symlinks.
fn download(run: &Arc<Run>, name: &str) -> Response {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.starts_with('.') || name.contains('\0') {
        return err(400, "bad file name");
    }
    let listing = list_files(&run.out_dir);
    let Some((n, _)) = listing.iter().find(|(n, _)| n == name) else { return err(404, "no such file in this run's output") };
    let p = run.out_dir.join(n);
    // refuse symlinks (list_files follows them via metadata)
    match std::fs::symlink_metadata(&p) {
        Ok(m) if m.file_type().is_file() => {}
        _ => return err(404, "no such file"),
    }
    let f = match std::fs::File::open(&p) {
        Ok(f) => f,
        Err(e) => return err(500, &e.to_string()),
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let safe: String = n.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' { c } else { '_' }).collect();
    let mut r = Response::new(200).header("content-disposition", format!("attachment; filename=\"{safe}\"")).header("content-type", "application/octet-stream").header("cache-control", "no-store");
    r.body = Body::Reader(Box::new(f), len);
    r
}

// ------------------------------------------------------------------------------------------
// memory

fn mem(app: &Arc<App>, req: &Request) -> Response {
    let s = app.session();
    let layer_id = req.param("layer").unwrap_or("kernel");
    let Some(addr) = qint(req, "addr").filter(|a| *a >= 0 && *a <= u64::MAX as i128) else { return err(422, "addr must be an integer (e.g. 0x1000)") };
    let len = qint(req, "len").unwrap_or(4096).clamp(1, super::mem::MAX_READ as i128) as u64;
    let layer = match s.layer(layer_id) {
        Ok(l) => l,
        Err(e) => return err(422, &e),
    };
    let (data, bad) = super::mem::read_range(layer, addr as u64, len);
    let mut w = W::new();
    w.obj().ks("layer", layer_id).ks("name", layer.name()).ks("addr", &format!("{addr:#x}")).ku("len", data.len() as u64).ks("max", &format!("{:#x}", layer.max_address()));
    let mut hex = String::with_capacity(data.len() * 2);
    for b in &data {
        hex.push_str(&format!("{b:02x}"));
    }
    w.ks("hex", &hex);
    w.key("bad").arr();
    for (o, l) in bad {
        w.arr().u(o).u(l).end_arr();
    }
    w.end_arr();
    w.end_obj();
    ok(w)
}

fn disasm(app: &Arc<App>, req: &Request) -> Response {
    let s = app.session();
    let layer_id = req.param("layer").unwrap_or("kernel");
    let Some(addr) = qint(req, "addr").filter(|a| *a >= 0 && *a <= u64::MAX as i128) else { return err(422, "addr must be an integer") };
    let len = qint(req, "len").unwrap_or(256).clamp(1, 16384) as u64;
    let arch = match req.param("arch") {
        Some(a @ ("intel" | "intel64" | "arm" | "arm64")) => a.to_string(),
        _ => s.warm.lock().unwrap_or_else(|e| e.into_inner()).1.arch.unwrap_or("intel64").to_string(),
    };
    let layer = match s.layer(layer_id) {
        Ok(l) => l,
        Err(e) => return err(422, &e),
    };
    let (data, bad) = super::mem::read_range(layer, addr as u64, len);
    let text = crate::disasm::format_capstone(&data, addr as u64, &arch);
    let mut w = W::new();
    w.obj().ks("arch", &arch).ks("addr", &format!("{addr:#x}")).ks("text", &text).kb("partial", !bad.is_empty()).end_obj();
    ok(w)
}

/// The desktop file dialog, when the page's user sits at this machine (loopback only).
fn picker_tool(app: &App) -> Option<picker::Tool> {
    if app.hosts.wildcard || !app.hosts.bind.is_some_and(|ip| ip.is_loopback()) {
        return None;
    }
    picker::tool()
}

/// Show the desktop's "open file" dialog and return the chosen path: `{"dir": "<start>"}`.
fn pick_file(app: &App, req: &Request) -> Response {
    let Some(tool) = picker_tool(app) else { return err(501, "no desktop file dialog here (the server needs a display and a loopback address)") };
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let dir = j.get("dir").and_then(|d| d.as_str()).map(|d| cwd.join(d)).filter(|d| d.is_dir()).unwrap_or(cwd);
    let mut w = W::new();
    match picker::pick(tool, &dir) {
        Ok(picker::Picked::Path(p)) => w.obj().ks("path", &p.to_string_lossy()).end_obj(),
        Ok(picker::Picked::Cancelled) => w.obj().kb("cancelled", true).end_obj(),
        Err(e) => return err(409, &e),
    };
    ok(w)
}

/// The user's presets in `~/.fvol/presets` (plus files that could not be read, with the reason).
fn list_presets(app: &App) -> Response {
    let known = |n: &str| app.plugins.iter().any(|p| p.name() == n);
    let dir = presets::presets_dir();
    let (ok_list, bad) = presets::list_in(&dir, &known);
    let mut w = W::new();
    w.obj().ks("dir", &dir.to_string_lossy()).key("presets").arr();
    for p in &ok_list {
        presets::write(&mut w, p, true);
    }
    w.end_arr().key("errors").arr();
    for (file, e) in &bad {
        w.obj().ks("file", file).ks("error", e).end_obj();
    }
    w.end_arr().end_obj();
    ok(w)
}

/// Save a preset: `{"name", "os", "plugins": [{"plugin", "args"}], "overwrite"}`.
fn save_preset(app: &App, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let Some(id) = j.get("name").and_then(|n| n.as_str()).and_then(presets::slug) else {
        return err(422, "the name needs at least one letter or digit");
    };
    let known = |n: &str| app.plugins.iter().any(|p| p.name() == n);
    let mut p = match presets::from_json(&id, &j, &known) {
        Ok(p) => p,
        Err(e) => return err(422, &e),
    };
    p.created = presets::now();
    let overwrite = matches!(j.get("overwrite"), Some(Json::Bool(true)));
    match presets::save_in(&presets::presets_dir(), &p, overwrite) {
        Ok(_) => {
            let mut w = W::new();
            presets::write(&mut w, &p, true);
            ok(w)
        }
        Err(presets::SaveError::Exists) => err(409, &format!("a preset named \"{}\" already exists", p.name)),
        Err(presets::SaveError::Io(e)) => err(500, &e),
    }
}

/// Directory listing for the "open image" dialog (names, sizes; no contents).
fn fs_list(req: &Request) -> Response {
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let p = req.param("path").filter(|p| !p.is_empty()).map(|p| cwd.join(p)).unwrap_or(cwd);
    let dir = if p.is_dir() { p.clone() } else { p.parent().map(|x| x.to_path_buf()).unwrap_or(p.clone()) };
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => return err(422, &format!("{}: {e}", dir.display())),
    };
    let mut entries: Vec<(String, bool, u64)> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let md = std::fs::metadata(e.path()).ok()?;
            Some((name, md.is_dir(), md.len()))
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    entries.truncate(2000);
    let mut w = W::new();
    w.obj().ks("dir", &dir.to_string_lossy()).key("entries").arr();
    for (n, d, s) in entries {
        w.obj().ks("name", &n).kb("dir", d).ku("size", s).end_obj();
    }
    w.end_arr().end_obj();
    ok(w)
}

// ------------------------------------------------------------------------------------------
// events

/// One long-lived NDJSON stream per browser tab: session state and run summaries whenever
/// they change (throttled to ~12 updates/s), heartbeats every 15 s.
fn events(app: &Arc<App>) -> Response {
    let app = app.clone();
    let Some(slot) = StreamSlot::take() else { return err(503, "too many open streams") };
    Response::new(200).header("cache-control", "no-store").stream(
        "application/x-ndjson; charset=utf-8",
        Box::new(move |w: &mut dyn Write| {
            let _slot = slot;
            let mut last_session: Vec<u8> = Vec::new();
            let mut last_batches: Vec<u8> = Vec::new();
            let mut sent_seq: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
            let mut seen = 0u64;
            let mut first = true;
            loop {
                let mut out = Vec::new();
                let now = app.hub.current();
                let mut sj = W::new();
                app.session().json(&mut sj);
                let sj = sj.done();
                if sj != last_session {
                    out.extend_from_slice(b"{\"t\":\"session\",\"session\":");
                    out.extend_from_slice(&sj);
                    out.extend_from_slice(b"}\n");
                    last_session = sj;
                }
                let mut bj = W::new();
                bj.arr();
                for b in app.runs.batches() {
                    b.json(&mut bj);
                }
                bj.end_arr();
                let bj = bj.done();
                if bj != last_batches {
                    out.extend_from_slice(b"{\"t\":\"batches\",\"batches\":");
                    out.extend_from_slice(&bj);
                    out.extend_from_slice(b"}\n");
                    last_batches = bj;
                }
                let runs = app.runs.all();
                let mut changed = W::new();
                changed.obj().ks("t", "runs").kb("full", first).key("runs").arr();
                let mut nchanged = 0;
                for r in &runs {
                    let seq = r.read().seq;
                    let running = r.read().status == Status::Running;
                    if first || sent_seq.get(&r.id) != Some(&seq) || running {
                        r.json(&mut changed);
                        sent_seq.insert(r.id, seq);
                        nchanged += 1;
                    }
                }
                changed.end_arr();
                let removed: Vec<u64> = sent_seq.keys().filter(|id| !runs.iter().any(|r| r.id == **id)).copied().collect();
                changed.key("removed").arr();
                for id in &removed {
                    changed.u(*id);
                    sent_seq.remove(id);
                }
                changed.end_arr().end_obj();
                if nchanged > 0 || !removed.is_empty() || first {
                    out.extend_from_slice(&changed.done());
                    out.push(b'\n');
                }
                first = false;
                if out.is_empty() {
                    out.extend_from_slice(b"{\"t\":\"hb\"}\n");
                }
                w.write_all(&out)?;
                w.flush()?;
                seen = seen.max(now);
                let any_running = runs.iter().any(|r| r.read().status == Status::Running);
                // running runs tick (elapsed time) twice a second even without new rows
                let wait = if any_running { Duration::from_millis(500) } else { Duration::from_secs(15) };
                seen = app.hub.wait(seen, wait);
                std::thread::sleep(Duration::from_millis(80));
            }
        }),
    )
}

/// A single-use, 60-second ticket for one GET URL (downloads: a plain link can't carry the
/// token header).
fn ticket(app: &Arc<App>, req: &Request) -> Response {
    let j = match body_json(req) {
        Ok(j) => j,
        Err(r) => return r,
    };
    let Some(path) = j.get("path").and_then(|p| p.as_str()) else { return err(422, "\"path\" is required") };
    if !path.starts_with("/api/") || path.contains("ticket=") || path.contains('#') || path.len() > 2048 {
        return err(422, "bad path");
    }
    match app.tickets.issue(path) {
        Some(t) => {
            let sep = if path.contains('?') { '&' } else { '?' };
            let mut w = W::new();
            w.obj().ks("url", &format!("{path}{sep}ticket={t}")).end_obj();
            ok(w)
        }
        None => err(503, "too many pending downloads"),
    }
}

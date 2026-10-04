//! The UI: hand-written HTML/CSS/vanilla JS (ES modules), embedded in the binary. No build step,
//! no external requests.

use super::http::{Body, Request, Response};

pub const INDEX_HTML: &str = include_str!("assets/index.html");

pub struct Asset {
    pub name: &'static str,
    pub ctype: &'static str,
    pub data: &'static [u8],
}

macro_rules! asset {
    ($name:literal, $ctype:literal) => {
        Asset { name: $name, ctype: $ctype, data: include_bytes!(concat!("assets/", $name)) }
    };
}

pub static ASSETS: &[Asset] = &[
    asset!("app.js", "text/javascript; charset=utf-8"),
    asset!("theme.js", "text/javascript; charset=utf-8"),
    asset!("core.js", "text/javascript; charset=utf-8"),
    asset!("table.js", "text/javascript; charset=utf-8"),
    asset!("catalog.js", "text/javascript; charset=utf-8"),
    asset!("ui.css", "text/css; charset=utf-8"),
    asset!("quickstart.js", "text/javascript; charset=utf-8"),
    asset!("workspace.js", "text/javascript; charset=utf-8"),
    asset!("procdata.js", "text/javascript; charset=utf-8"),
    asset!("rules.js", "text/javascript; charset=utf-8"),
    asset!("presets.js", "text/javascript; charset=utf-8"),
    asset!("results.js", "text/javascript; charset=utf-8"),
    asset!("layout.js", "text/javascript; charset=utf-8"),
    asset!("options.js", "text/javascript; charset=utf-8"),
    asset!("favicon.svg", "image/svg+xml"),
];

pub fn get(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|a| a.name == name)
}

/// `FASTVOL_WEB_DEV=<dir>`: serve the page and its assets from `<dir>` (read on every request)
/// instead of the copies compiled in, so UI work shows up on a browser refresh. Development only.
pub fn dev_dir() -> Option<&'static std::path::Path> {
    static DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| std::env::var_os("FASTVOL_WEB_DEV").filter(|d| !d.is_empty()).map(Into::into)).as_deref()
}

/// The page, from the dev directory when there is one.
pub fn index_html() -> String {
    match dev_dir().and_then(|d| std::fs::read_to_string(d.join("index.html")).ok()) {
        Some(s) => s,
        None => INDEX_HTML.to_string(),
    }
}

/// An asset read from the dev directory: plain file names only (no paths, no dot files).
fn dev_asset(dir: &std::path::Path, name: &str) -> Option<Response> {
    if name.is_empty() || name.starts_with('.') || !name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_')) {
        return None;
    }
    let data = std::fs::read(dir.join(name)).ok()?;
    let ctype = match name.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "svg" => "image/svg+xml",
        "woff2" => "font/woff2",
        "png" => "image/png",
        _ => "text/plain; charset=utf-8",
    };
    Some(Response::new(200).header("cache-control", "no-store").bytes(ctype, data))
}

/// Serve an asset by name: from the dev directory when set, else the compiled-in copy.
pub fn serve(name: &str, req: &Request) -> Response {
    if let Some(dir) = dev_dir() {
        return dev_asset(dir, name).unwrap_or_else(|| Response::text(404, "not found"));
    }
    get(name).map(|a| respond(a, req)).unwrap_or_else(|| Response::text(404, "not found"))
}

/// FNV-1a of the content: a strong-enough ETag for embedded, immutable-per-binary files.
fn etag(a: &Asset) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in a.data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("\"{h:016x}\"")
}

pub fn respond(a: &'static Asset, req: &Request) -> Response {
    let tag = etag(a);
    if req.header("if-none-match") == Some(tag.as_str()) {
        return Response::new(304).header("etag", tag).header("cache-control", "no-cache");
    }
    let mut r = Response::new(200).header("content-type", a.ctype).header("etag", tag).header("cache-control", "no-cache");
    r.body = Body::Static(a.data);
    r
}

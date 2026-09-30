//! Unit tests: HTTP parsing and limits, routing and every security check, option validation,
//! and the run engine (streaming sink, views, cancel, crash handling) with fake plugins.

use super::http::{self, Conn, HttpError, Limits, Method, Request, Response};
use super::runs::{Hub, Runs, Status};
use super::security::HostPolicy;
use super::session::{Session, SessionOpts};
use super::*;
use crate::cli::json::Json;
use crate::context::Context;
use crate::error::Result;
use crate::plugins::{Config, ConfigValue, Plugin, ReqKind, Requirement};
use crate::renderers::{DateTime, RowSink, Value};
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

// ------------------------------------------------------------------------------------------
// fake plugins

struct Fake;
impl Plugin for Fake {
    fn name(&self) -> &'static str {
        "test.fake.Fake"
    }
    fn description(&self) -> &'static str {
        "A fake plugin for tests."
    }
    fn requirements(&self) -> Vec<Requirement> {
        vec![
            Requirement::new("pid", "Process IDs", ReqKind::ListInt).optional(),
            Requirement::flag("dump", "Dump things"),
            Requirement::new("mode", "Mode", ReqKind::Choice(vec!["fast", "slow"])).optional().default(ConfigValue::Str("fast".into())),
            Requirement::new("count", "Rows", ReqKind::Int).optional(),
        ]
    }
    fn run(&self, ctx: &Context, cfg: &Config, out: &mut dyn RowSink) -> Result<()> {
        out.begin(crate::cols![("PID", Int), ("Name", Str), ("Offset", Hex), ("When", DateTime)])?;
        let n = cfg.get_int("count").unwrap_or(10);
        for i in 0..n {
            let depth = if i % 3 == 0 { 0 } else { 1 };
            let when = if i % 4 == 0 { Value::NotApplicable } else { Value::DateTime(DateTime { secs: 1_700_000_000 + i as i64 * 60, micros: 0, utc: true }) };
            out.row(depth, vec![Value::Int(i), Value::Str(format!("proc{i}.exe")), if i == 5 { Value::Unreadable } else { Value::Int(0xffff_8000_0000_0000u64 as i128 + i * 16) }, when])?;
        }
        if cfg.get_bool("dump") {
            let (mut f, _) = ctx.create_output_file("dumped.bin")?;
            use std::io::Write;
            f.write_all(b"evidence")?;
        }
        Ok(())
    }
}
static FAKE: Fake = Fake;

struct Slow;
impl Plugin for Slow {
    fn name(&self) -> &'static str {
        "test.slow.Slow"
    }
    fn description(&self) -> &'static str {
        ""
    }
    fn run(&self, _ctx: &Context, _cfg: &Config, out: &mut dyn RowSink) -> Result<()> {
        out.begin(crate::cols![("N", Int)])?;
        for i in 0..100_000 {
            std::thread::sleep(Duration::from_millis(2));
            out.row(0, vec![Value::Int(i)])?;
        }
        Ok(())
    }
}
static SLOW: Slow = Slow;

struct Crash;
impl Plugin for Crash {
    fn name(&self) -> &'static str {
        "test.crash.Crash"
    }
    fn description(&self) -> &'static str {
        ""
    }
    fn run(&self, _ctx: &Context, _cfg: &Config, out: &mut dyn RowSink) -> Result<()> {
        out.begin(crate::cols![("N", Int)])?;
        out.row(0, vec![Value::Int(1)])?;
        panic!("boom");
    }
}
static CRASH: Crash = Crash;

fn test_app() -> Arc<App> {
    let out = std::env::temp_dir().join(format!("fastvol-web-test-{}-{}", std::process::id(), runs::now_ms()));
    // a small file stands in for the image: fake plugins never read it
    let img = out.with_extension("img");
    std::fs::write(&img, vec![0u8; 4096]).unwrap();
    let opts = SessionOpts { image: Some(img), symbol_dirs: vec![], out_root: out, offline: true, remote_isf_url: None, cache_path: None, ..Default::default() };
    let hub = Arc::new(Hub::default());
    let session = Arc::new(Session::new(1, &opts).unwrap());
    let plugins: Vec<&'static dyn Plugin> = vec![&FAKE, &SLOW, &CRASH];
    Arc::new(App {
        token: "0123456789abcdef0123456789abcdef".into(),
        port: 8765,
        hosts: HostPolicy { port: 8765, wildcard: false, bind: Some("127.0.0.1".parse().unwrap()), names: vec![] },
        plugins_json: api::plugins_json(&plugins),
        plugins,
        session: std::sync::RwLock::new(session),
        base_opts: std::sync::Mutex::new(opts),
        runs: Arc::new(Runs::new(hub.clone(), 2, 1 << 30)),
        hub,
        next_session: std::sync::atomic::AtomicU64::new(2),
        auth_failures: std::sync::atomic::AtomicU64::new(0),
        tickets: super::security::Tickets::default(),
        export_seq: std::sync::atomic::AtomicU64::new(1),
        started: std::time::Instant::now(),
        options: std::sync::Mutex::new(Default::default()),
        cli_options: Default::default(),
        default_out: std::env::temp_dir(),
        rules: std::sync::Mutex::new(None),
        // no saved analysis: tests never write to the real ~/.fvol
        analysis: std::sync::Mutex::new(None),
    })
}

fn req(method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> Request {
    let mut raw = format!("{method} {target} HTTP/1.1\r\n");
    for (n, v) in headers {
        raw.push_str(&format!("{n}: {v}\r\n"));
    }
    if !body.is_empty() {
        raw.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    raw.push_str("\r\n");
    raw.push_str(body);
    let mut c = Conn::new(Cursor::new(raw.into_bytes()));
    c.read_request(&Limits::default()).unwrap()
}

const TOK: &str = "0123456789abcdef0123456789abcdef";
const HOST: (&str, &str) = ("host", "127.0.0.1:8765");
const AUTH: (&str, &str) = ("x-vol-token", TOK);

fn call(app: &Arc<App>, method: &str, target: &str, headers: &[(&str, &str)], body: &str) -> (u16, String, Response) {
    let r = api::handle(app, &req(method, target, headers, body));
    let status = r.status;
    let text = match &r.body {
        http::Body::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        http::Body::Static(b) => String::from_utf8_lossy(b).into_owned(),
        _ => String::new(),
    };
    (status, text, r)
}

fn j(s: &str) -> Json {
    crate::cli::json::parse(s).unwrap()
}

fn wait_done(app: &Arc<App>, id: u64) {
    for _ in 0..500 {
        let r = app.runs.get(id).unwrap();
        let d = r.read();
        if d.status.finished() && !d.busy {
            return;
        }
        drop(d);
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("run {id} did not finish");
}

// ------------------------------------------------------------------------------------------
// HTTP parsing

fn parse(raw: &[u8]) -> std::result::Result<Request, HttpError> {
    Conn::new(Cursor::new(raw.to_vec())).read_request(&Limits::default())
}

#[test]
fn http_parses_requests() {
    let r = parse(b"GET /api/rows?view=3&q=a%20b+c HTTP/1.1\r\nHost: x\r\nX-Thing:  v \r\n\r\n").unwrap();
    assert_eq!(r.method, Method::Get);
    assert_eq!(r.path, "/api/rows");
    assert_eq!(r.param("view"), Some("3"));
    assert_eq!(r.param("q"), Some("a b c"));
    assert_eq!(r.header("x-thing"), Some("v"));
    assert!(r.keep_alive);
    let r = parse(b"POST /x HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello").unwrap();
    assert_eq!(r.body, b"hello");
    assert!(!r.keep_alive);
    let r = parse(b"GET / HTTP/1.0\r\n\r\n").unwrap();
    assert!(!r.keep_alive);
    let r = parse(b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").unwrap();
    assert!(r.keep_alive);
}

#[test]
fn http_pipelining() {
    let raw = b"GET /a HTTP/1.1\r\nHost: x\r\n\r\n\r\nPOST /b HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nokGET /c HTTP/1.1\r\nHost: x\r\n\r\n";
    let mut c = Conn::new(Cursor::new(raw.to_vec()));
    let l = Limits::default();
    assert_eq!(c.read_request(&l).unwrap().path, "/a");
    let b = c.read_request(&l).unwrap();
    assert_eq!((b.path.as_str(), b.body.as_slice()), ("/b", &b"ok"[..]));
    assert_eq!(c.read_request(&l).unwrap().path, "/c");
    assert_eq!(c.read_request(&l).unwrap_err(), HttpError::Closed);
}

#[test]
fn http_rejects_malformed_and_oversized() {
    let cases: &[(&[u8], u16)] = &[
        (b"GET / HTTP/1.1\r\n\r\n", 400),                                               // no Host
        (b"GET / HTTP/1.1\r\nHost: a\r\nHost: b\r\n\r\n", 400),                         // two Hosts
        (b"GET http://evil/ HTTP/1.1\r\nHost: a\r\n\r\n", 400),                         // absolute-form
        (b"GET //x HTTP/1.1\r\nHost: a\r\n\r\n", 400),                                  // scheme-relative
        (b"GET /%zz HTTP/1.1\r\nHost: a\r\n\r\n", 400),                                 // bad escape
        (b"GET /%00 HTTP/1.1\r\nHost: a\r\n\r\n", 400),                                 // NUL
        (b"GET / HTTP/2.0\r\nHost: a\r\n\r\n", 505),                                    // version
        (b"GET  / HTTP/1.1\r\nHost: a\r\n\r\n", 400),                                   // double space
        (b"GET / HTTP/1.1\r\nHost: a\r\n folded\r\n\r\n", 400),                         // obs-fold
        (b"GET / HTTP/1.1\r\nHo st: a\r\n\r\n", 400),                                   // bad name
        (b"POST / HTTP/1.1\r\nHost: a\r\nTransfer-Encoding: chunked\r\n\r\n", 501),     // TE body
        (b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: -1\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: +5\r\n\r\n", 400),
        (b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: 99999999\r\n\r\n", 413),
        (b"POST / HTTP/1.1\r\nHost: a\r\nContent-Length: 10\r\n\r\nshort", 400),      // truncated
        (b"GET / HTTP/1.1\r\nHost: a\r\nX: a\x01b\r\n\r\n", 400),                       // control char
    ];
    for (raw, want) in cases {
        let e = parse(raw).unwrap_err();
        assert_eq!(e.status(), Some(*want), "{:?} -> {e:?}", String::from_utf8_lossy(raw));
    }
    // a head larger than the limit
    let mut big = b"GET / HTTP/1.1\r\nHost: a\r\n".to_vec();
    for i in 0..400 {
        big.extend_from_slice(format!("X-{i}: {}\r\n", "a".repeat(40)).as_bytes());
    }
    big.extend_from_slice(b"\r\n");
    assert_eq!(parse(&big).unwrap_err().status(), Some(431));
    // too many headers
    let mut many = b"GET / HTTP/1.1\r\nHost: a\r\n".to_vec();
    for i in 0..100 {
        many.extend_from_slice(format!("X-{i}: 1\r\n").as_bytes());
    }
    many.extend_from_slice(b"\r\n");
    assert_eq!(parse(&many).unwrap_err().status(), Some(431));
    // empty connection
    assert_eq!(parse(b"").unwrap_err(), HttpError::Closed);
}

#[test]
fn http_writes_fixed_and_chunked() {
    let mut out = Vec::new();
    http::write_response(&mut out, Response::text(200, "hi"), true, true, false, &[]).unwrap();
    let s = String::from_utf8(out).unwrap();
    assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(s.contains("content-length: 3\r\n"));
    assert!(s.ends_with("\r\n\r\nhi\n"));
    let mut out = Vec::new();
    let r = Response::new(200).stream(
        "text/plain",
        Box::new(|w: &mut dyn std::io::Write| {
            w.write_all(b"abc")?;
            w.flush()?;
            w.write_all(b"defg")
        }),
    );
    let keep = http::write_response(&mut out, r, true, true, false, &[]).unwrap();
    assert!(keep);
    let s = String::from_utf8(out).unwrap();
    assert!(s.contains("transfer-encoding: chunked"));
    assert!(s.ends_with("\r\n\r\n3\r\nabc\r\n4\r\ndefg\r\n0\r\n\r\n"), "{s:?}");
    // header values can't inject headers
    let mut out = Vec::new();
    http::write_response(&mut out, Response::new(204).header("x", "a\r\nset-cookie: evil=1"), true, true, false, &[]).unwrap();
    assert!(!String::from_utf8(out).unwrap().contains("\r\nset-cookie"));
}

// ------------------------------------------------------------------------------------------
// security & routing

#[test]
fn api_requires_token_host_and_same_origin() {
    let app = test_app();
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST], "").0, 401);
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, ("x-vol-token", "nope")], "").0, 401);
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, AUTH], "").0, 200);
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, ("authorization", &format!("Bearer {TOK}"))], "").0, 200);
    // DNS rebinding: a foreign Host is refused even with the token
    assert_eq!(call(&app, "GET", "/api/plugins", &[("host", "evil.example:8765"), AUTH], "").0, 421);
    assert_eq!(call(&app, "GET", "/api/plugins", &[("host", "127.0.0.1:9999"), AUTH], "").0, 421);
    assert_eq!(call(&app, "GET", "/api/plugins", &[("host", "localhost:8765"), AUTH], "").0, 200);
    // cross-site browser requests
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, AUTH, ("sec-fetch-site", "cross-site")], "").0, 403);
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, AUTH, ("origin", "http://evil.example")], "").0, 403);
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, AUTH, ("origin", "http://127.0.0.1:8765"), ("sec-fetch-site", "same-origin")], "").0, 200);
    // cookies are never credentials (they are not port-isolated)
    let cookie = format!("fastvol_8765={TOK}");
    assert_eq!(call(&app, "GET", "/api/plugins", &[HOST, ("cookie", &cookie)], "").0, 401);
    let body = r#"{"plugin":"test.fake.Fake"}"#;
    assert_eq!(call(&app, "POST", "/api/runs", &[HOST, ("cookie", &cookie)], body).0, 401);
    // the token is not accepted in the query string either
    assert_eq!(call(&app, "GET", &format!("/api/plugins?token={TOK}"), &[HOST], "").0, 401);
    // deeply nested JSON is refused before parsing
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    assert_eq!(call(&app, "POST", "/api/runs", &[HOST, AUTH], &deep).0, 400);
    // no CORS preflight support
    assert_eq!(call(&app, "OPTIONS", "/api/runs", &[HOST], "").0, 401);
    assert_eq!(call(&app, "OPTIONS", "/", &[HOST], "").0, 405);
}

#[test]
fn page_download_tickets_and_static_assets() {
    let app = test_app();
    // the page holds no secret and sets no cookie
    let (st, body, r) = call(&app, "GET", "/", &[HOST], "");
    assert_eq!(st, 200);
    assert!(!body.contains(TOK));
    assert!(!r.headers.iter().any(|(n, _)| *n == "set-cookie"));
    // download tickets: single use, bound to one exact URL, issued only with the token
    let target = "/api/plugins?x=1";
    assert_eq!(call(&app, "POST", "/api/ticket", &[HOST], &format!(r#"{{"path":"{target}"}}"#)).0, 401);
    let (st, text, _) = call(&app, "POST", "/api/ticket", &[HOST, AUTH], &format!(r#"{{"path":"{target}"}}"#));
    assert_eq!(st, 200, "{text}");
    let url = match j(&text).get("url") {
        Some(Json::Str(u)) => u.clone(),
        _ => panic!("{text}"),
    };
    let t = url.rsplit_once("ticket=").unwrap().1.to_string();
    assert_eq!(call(&app, "GET", &format!("/api/plugins?x=2&ticket={t}"), &[HOST], "").0, 401); // other URL
    assert_eq!(call(&app, "POST", &url, &[HOST], "").0, 401); // GET only
    assert_eq!(call(&app, "GET", &url, &[HOST], "").0, 200);
    assert_eq!(call(&app, "GET", &url, &[HOST], "").0, 401); // used up
    assert_eq!(call(&app, "POST", "/api/ticket", &[HOST, AUTH], r#"{"path":"/etc/passwd"}"#).0, 422);
    // assets are public, ETag'd
    let (st, _, r) = call(&app, "GET", "/assets/ui.css", &[HOST], "");
    assert_eq!(st, 200);
    let tag = r.headers.iter().find(|(n, _)| *n == "etag").unwrap().1.clone();
    assert_eq!(call(&app, "GET", "/assets/ui.css", &[HOST, ("if-none-match", &tag)], "").0, 304);
    assert_eq!(call(&app, "GET", "/assets/../mod.rs", &[HOST], "").0, 404);
    assert_eq!(call(&app, "GET", "/assets/%2e%2e%2fmod.rs", &[HOST], "").0, 404);
    // a foreign Host can't even load the page
    assert_eq!(call(&app, "GET", "/", &[("host", "rebind.example:8765")], "").0, 421);
}

#[test]
fn csp_and_security_headers() {
    let h = api::common_headers(true);
    let csp = h.iter().find(|(n, _)| *n == "content-security-policy").unwrap().1;
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("unsafe"));
    assert!(h.iter().any(|(n, v)| *n == "x-content-type-options" && *v == "nosniff"));
}

// ------------------------------------------------------------------------------------------
// option validation

#[test]
fn config_validation_mirrors_python() {
    let p: &dyn Plugin = &FAKE;
    let (cfg, argv) = api::parse_config(p, Some(&j(r#"{"pid":["0x10", 20, "0o7", "1_000"], "dump": true, "count": "0b11"}"#))).unwrap();
    assert_eq!(cfg.get_ints("pid"), vec![16, 20, 7, 1000]);
    assert!(cfg.get_bool("dump"));
    assert_eq!(cfg.get_int("count"), Some(3));
    assert_eq!(cfg.get_str("mode"), Some("fast")); // default applied
    assert_eq!(argv, vec!["--pid", "16", "20", "7", "1000", "--dump", "--count", "3"]);
    let (cfg, _) = api::parse_config(p, Some(&j(r#"{"pid":"4, 8 12"}"#))).unwrap();
    assert_eq!(cfg.get_ints("pid"), vec![4, 8, 12]);
    // python int(x, 0) rejects leading zeros, stray underscores and junk
    for bad in ["010", "1__0", "_1", "0x", "12a", "1.5", "--1"] {
        let e = api::parse_config(p, Some(&j(&format!(r#"{{"count":"{bad}"}}"#)))).unwrap_err();
        assert!(e.contains("invalid int value"), "{bad}: {e}");
    }
    assert!(api::parse_config(p, Some(&j(r#"{"mode":"medium"}"#))).unwrap_err().contains("invalid choice"));
    assert!(api::parse_config(p, Some(&j(r#"{"nope":1}"#))).unwrap_err().contains("no option"));
    // empty values are "not given"
    let (cfg, argv) = api::parse_config(p, Some(&j(r#"{"pid":[], "count":"", "dump": false}"#))).unwrap();
    assert!(cfg.get("pid").is_none() && cfg.get("count").is_none() && argv.is_empty());
}

// ------------------------------------------------------------------------------------------
// runs

fn start(app: &Arc<App>, body: &str) -> u64 {
    let (st, text, _) = call(app, "POST", "/api/runs", &[HOST, AUTH], body);
    assert_eq!(st, 200, "{text}");
    match j(&text).get("id") {
        Some(Json::Int(i)) => *i as u64,
        _ => panic!("{text}"),
    }
}

#[test]
fn run_streams_rows_views_and_files() {
    let app = test_app();
    let id = start(&app, r#"{"plugin":"test.fake.Fake","args":{"count":50,"dump":true}}"#);
    wait_done(&app, id);
    let run = app.runs.get(id).unwrap();
    {
        let d = run.read();
        assert_eq!(d.status, Status::Done);
        assert_eq!(d.table.rows(), 50);
        // cells hold exactly what the quick renderer prints
        let txt = |r, c| {
            let mut nb = super::table::NumBuf::default();
            d.table.text(r, c, &mut nb).to_vec()
        };
        assert_eq!(txt(1, 2), b"0xffff800000000010");
        assert_eq!(d.table.kind(1, 2), super::table::K_HEX);
        assert_eq!(d.table.kind(1, 0), super::table::K_DEC);
        assert_eq!(txt(0, 3), b"N/A");
        assert_eq!(txt(5, 2), b"-");
        assert_eq!(txt(1, 3), b"2023-11-14 22:14:20.000000 UTC");
        assert_eq!(d.files, vec![("dumped.bin".to_string(), 8)]);
    }
    // window of rows
    let (st, text, _) = call(&app, "GET", &format!("/api/runs/{id}/rows?from=4&count=2"), &[HOST, AUTH], "");
    assert_eq!(st, 200);
    let v = j(&text);
    assert_eq!(v.get("total"), Some(&Json::Int(50)));
    let rows = v.get("rows").unwrap().as_arr();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].as_arr()[0], Json::Int(5));
    assert_eq!(rows[1].as_arr()[4], Json::Null); // unreadable
    // filtered + sorted view
    let (st, text, _) = call(&app, "POST", &format!("/api/runs/{id}/view"), &[HOST, AUTH], r#"{"cols":{"0":">=40"},"sort":[[0,"desc"]]}"#);
    assert_eq!(st, 200, "{text}");
    let v = j(&text);
    let vid = match v.get("view") {
        Some(Json::Int(i)) => *i,
        _ => panic!(),
    };
    assert_eq!(v.get("total"), Some(&Json::Int(10)));
    let (_, text, _) = call(&app, "GET", &format!("/api/runs/{id}/rows?view={vid}&from=0&count=3"), &[HOST, AUTH], "");
    let rows = j(&text);
    let first: Vec<i128> = rows.get("rows").unwrap().as_arr().iter().map(|r| if let Json::Int(i) = r.as_arr()[0] { i } else { -1 }).collect();
    assert_eq!(first, vec![49, 48, 47]);
    // bad filter expression is explained
    let (st, text, _) = call(&app, "POST", &format!("/api/runs/{id}/view"), &[HOST, AUTH], r#"{"cols":{"0":">abc"}}"#);
    assert_eq!(st, 422);
    assert!(text.contains("not a number"));
    // histogram over the DateTime column
    let (st, text, _) = call(&app, "GET", &format!("/api/runs/{id}/hist?col=3&buckets=10"), &[HOST, AUTH], "");
    assert_eq!(st, 200);
    let counts: i128 = j(&text).get("counts").unwrap().as_arr().iter().map(|c| if let Json::Int(i) = c { *i } else { 0 }).sum();
    assert_eq!(counts, 37); // 50 rows minus 13 N/A
    // downloads: listed file ok, traversal refused
    let (st, _, r) = call(&app, "GET", &format!("/api/runs/{id}/files/dumped.bin"), &[HOST, AUTH], "");
    assert_eq!(st, 200);
    assert!(matches!(r.body, http::Body::Reader(_, 8)));
    for bad in ["..%2f..%2fetc%2fpasswd", "%2e%2e", ".hidden", "nope.bin", "a%5cb"] {
        let st = call(&app, "GET", &format!("/api/runs/{id}/files/{bad}"), &[HOST, AUTH], "").0;
        assert!(st == 400 || st == 404, "{bad}: {st}");
    }
    // the same request with reuse returns the finished run
    let (_, text, _) = call(&app, "POST", "/api/runs", &[HOST, AUTH], r#"{"plugin":"test.fake.Fake","args":{"count":"50","dump":true},"reuse":true}"#);
    assert_eq!(j(&text).get("reused"), Some(&Json::Bool(true)));
    assert_eq!(j(&text).get("id"), Some(&Json::Int(id as i128)));
    // validation errors come back as 422 with argparse-like text
    let (st, text, _) = call(&app, "POST", "/api/runs", &[HOST, AUTH], r#"{"plugin":"test.fake.Fake","args":{"count":"12x"}}"#);
    assert_eq!(st, 422);
    assert!(text.contains("invalid int value: '12x'"));
    assert_eq!(call(&app, "POST", "/api/runs", &[HOST, AUTH], r#"{"plugin":"windows.nothing.X"}"#).0, 404);
}

#[test]
fn run_cancel_and_crash() {
    let app = test_app();
    let id = start(&app, r#"{"plugin":"test.slow.Slow"}"#);
    std::thread::sleep(Duration::from_millis(60));
    let (st, _, _) = call(&app, "POST", &format!("/api/runs/{id}/cancel"), &[HOST, AUTH], "");
    assert_eq!(st, 200);
    wait_done(&app, id);
    let r = app.runs.get(id).unwrap();
    assert_eq!(r.read().status, Status::Cancelled);
    assert!(r.read().table.rows() < 100_000);

    let id = start(&app, r#"{"plugin":"test.crash.Crash"}"#);
    wait_done(&app, id);
    let r = app.runs.get(id).unwrap();
    let d = r.read();
    assert_eq!(d.status, Status::Failed);
    assert_eq!(d.table.rows(), 1);
    let e = d.error.as_ref().unwrap();
    assert_eq!(e.kind, "crash");
    assert!(e.detail.contains("boom"));
}

#[test]
fn queue_respects_parallel_limit() {
    let app = test_app(); // max 2 in parallel
    let ids: Vec<u64> = (0..3).map(|i| start(&app, &format!(r#"{{"plugin":"test.slow.Slow","origin":"t{}"}}"#, "x".repeat(i)))).collect();
    std::thread::sleep(Duration::from_millis(50));
    let st: Vec<Status> = ids.iter().map(|i| app.runs.get(*i).unwrap().read().status).collect();
    assert_eq!(st.iter().filter(|s| **s == Status::Running).count(), 2);
    assert_eq!(st[2], Status::Queued);
    for i in &ids {
        app.runs.cancel(&app.runs.get(*i).unwrap());
    }
    for i in &ids {
        wait_done(&app, *i);
    }
}

#[test]
fn exports_csv_json_md() {
    let app = test_app();
    let id = start(&app, r#"{"plugin":"test.fake.Fake","args":{"count":4}}"#);
    wait_done(&app, id);
    let get = |fmt: &str| -> String {
        let (st, _, r) = call(&app, "GET", &format!("/api/runs/{id}/export?format={fmt}&cols=0,1,3"), &[HOST, AUTH], "");
        assert_eq!(st, 200);
        let mut out = Vec::new();
        if let http::Body::Stream(f) = r.body {
            f(&mut out).unwrap();
        }
        String::from_utf8(out).unwrap()
    };
    let csv = get("csv");
    assert!(csv.starts_with("TreeDepth,PID,Name,When\r\n0,0,proc0.exe,N/A\r\n1,1,proc1.exe,2023-11-14 22:14:20.000000 UTC\r\n"), "{csv}");
    let json = get("json");
    let parsed = j(&json);
    assert_eq!(parsed.as_arr().len(), 4);
    assert_eq!(parsed.as_arr()[1].get("PID"), Some(&Json::Int(1)));
    assert_eq!(parsed.as_arr()[0].get("When"), Some(&Json::Null));
    let jl = get("jsonl");
    assert_eq!(jl.lines().count(), 4);
    let md = get("md");
    assert!(md.starts_with("| PID | Name | When |\n|---|---|---|\n"));
}

#[test]
fn memory_budget_caps_runs_and_evicts_least_recently_viewed() {
    let app = test_app();
    // a registry with a 64 KiB budget (32 KiB per run)
    let runs = Arc::new(Runs::new(app.hub.clone(), 2, 64 << 10));
    let session = app.session();
    let start = |n: i128| {
        let (cfg, argv) = api::parse_config(&FAKE, Some(&j(&format!(r#"{{"count":{n}}}"#)))).unwrap();
        let r = runs.create(session.clone(), &FAKE, cfg, argv, format!("k{n}"), "user");
        runs.submit(r.clone());
        for _ in 0..500 {
            if r.read().status.finished() && !r.read().busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        r
    };
    let a = start(400);
    let b = start(401);
    assert!(!a.read().truncated && !b.read().truncated);
    b.last_access.store(runs::now_ms(), std::sync::atomic::Ordering::Relaxed); // b was looked at
    let c = start(402);
    // a (least recently viewed) made room for c
    assert!(a.read().evicted, "a should have been evicted");
    assert!(!b.read().evicted);
    assert_eq!(c.read().table.rows(), 402);
    // one run can't take more than half the budget
    let big = start(5000);
    assert!(big.read().truncated);
    assert_eq!(big.read().produced, 5000);
    assert!(big.read().table.bytes() <= 32 << 10);
    assert!(runs.used.load(std::sync::atomic::Ordering::Relaxed) <= 64 << 10);
}

#[test]
fn slow_clients_hit_a_hard_deadline() {
    /// yields one byte per read, 20 ms apart: never finishes a request
    struct Drip(usize);
    impl std::io::Read for Drip {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            let src = b"GET / HTTP/1.1\r\nHost: x\r\nX: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
            if self.0 >= src.len() {
                b[0] = b'a';
            } else {
                b[0] = src[self.0];
            }
            self.0 += 1;
            Ok(1)
        }
    }
    impl http::ReadTimeout for Drip {}
    let lim = Limits { request_time: Duration::from_millis(200), ..Limits::default() };
    let t = std::time::Instant::now();
    let e = Conn::new(Drip(0)).read_request(&lim).unwrap_err();
    assert_eq!(e, HttpError::Timeout);
    assert!(t.elapsed() < Duration::from_millis(600), "{:?}", t.elapsed());
}

#[test]
fn landing_facts_are_unique() {
    use super::session::add_fact;
    let mut f = vec![("Kernel".to_string(), "ntkrnlmp.pdb X-1".to_string())];
    // windows.info rows first, then the automagic's own values for the same facts
    add_fact(&mut f, "Kernel Base".into(), "0xf80000000000".into());
    add_fact(&mut f, "DTB".into(), "0x1aa000".into());
    add_fact(&mut f, "Symbols".into(), "file:///x.json.xz".into());
    add_fact(&mut f, "Kernel Base".into(), "0xf80000000000".into());
    add_fact(&mut f, "kernel base".into(), "0x1".into());
    add_fact(&mut f, "DTB".into(), "0x2".into());
    let labels: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(labels, ["Kernel", "Kernel Base", "DTB", "Symbols"]);
    assert_eq!(f[2].1, "0x1aa000");
}

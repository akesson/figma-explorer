//! A local stand-in for `api.figma.com`, routed by path so concurrent
//! requests (`ensure_fresh`, prefetch, asset downloads) get the right body
//! regardless of arrival order.
//!
//! Routes come from `tests/fixtures/api/routes.json` (written by
//! `scripts/record_fixtures.py`) and can be overridden per test with
//! [`FakeFigma::set_route`]. The query string is ignored when matching.
//! Two routes are synthesized instead of recorded: `/v1/images/{key}` answers
//! with URLs pointing back at this server, and `/img/*` serves a canned PNG
//! or SVG — render bytes aren't what the suite tests. Pagination tests use
//! [`FakeFigma::set_route_exact`], which matches path *and* query and wins
//! over a path-only route.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A 1×1 transparent PNG.
pub const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
pub const SVG: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>"#;

/// One request the server saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Req {
    /// Path including the query string.
    pub path: String,
    /// `X-Figma-Token` header, if sent.
    pub token: Option<String>,
}

#[derive(Clone)]
struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

#[derive(Default)]
struct State {
    routes: HashMap<String, Response>,
    /// Keyed by path including the query string; checked first.
    exact: HashMap<String, Response>,
    log: Vec<Req>,
}

pub struct FakeFigma {
    base_url: String,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeFigma {
    /// Serve the routes in `<fixtures>/routes.json`, then the hand-authored
    /// sibling `overlay/` on top. A missing `routes.json` starts an empty
    /// server (every path 404s until `set_route`).
    pub fn start(fixtures: &Path) -> Self {
        let mut state = State::default();
        let index = fixtures.join("routes.json");
        if index.exists() {
            let routes: HashMap<String, serde_json::Value> =
                serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap();
            for (path, r) in routes {
                let body_path = fixtures.join(r["body"].as_str().unwrap());
                let body = std::fs::read(&body_path)
                    .unwrap_or_else(|e| panic!("fixture {}: {e}", body_path.display()));
                let status = r["status"].as_u64().unwrap() as u16;
                state.routes.insert(path, json(status, body));
            }
        }
        let overlay = fixtures.with_file_name("overlay");
        if overlay.is_dir() {
            load_overlay(&overlay, &overlay, &mut state.routes);
        }

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(state));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let (state, stop, base_url) = (Arc::clone(&state), Arc::clone(&stop), base_url.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        handle(stream, &state, &base_url);
                    }
                }
            })
        };
        FakeFigma {
            base_url,
            state,
            stop,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Add or replace a route (JSON body).
    pub fn set_route(&self, path: &str, status: u16, body: &str) {
        self.state
            .lock()
            .unwrap()
            .routes
            .insert(path.to_owned(), json(status, body.as_bytes().to_vec()));
    }

    /// Add or replace a route matched on path and query string exactly
    /// (e.g. `/v1/teams/2002/components?page_size=1000&after=5`).
    pub fn set_route_exact(&self, path_and_query: &str, status: u16, body: &str) {
        self.state.lock().unwrap().exact.insert(
            path_and_query.to_owned(),
            json(status, body.as_bytes().to_vec()),
        );
    }

    /// Current body of a route, for tests that patch a recorded fixture.
    pub fn route_body(&self, path: &str) -> String {
        let state = self.state.lock().unwrap();
        let r = state
            .routes
            .get(path)
            .unwrap_or_else(|| panic!("no route {path}"));
        String::from_utf8(r.body.clone()).unwrap()
    }

    /// Take every request logged since the last call.
    pub fn drain(&self) -> Vec<Req> {
        std::mem::take(&mut self.state.lock().unwrap().log)
    }
}

impl Drop for FakeFigma {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock `incoming()` so the thread sees the flag.
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Hand-authored responses in the sibling `overlay/` dir (see its README):
/// `overlay/v1/files/K/comments.json` serves `/v1/files/K/comments` with
/// status 200, replacing any recorded route for the same path.
fn load_overlay(root: &Path, dir: &Path, routes: &mut HashMap<String, Response>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            load_overlay(root, &path, routes);
        } else if path.extension().is_some_and(|e| e == "json") {
            let rel = path.strip_prefix(root).unwrap().with_extension("");
            let route = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
            routes.insert(route, json(200, std::fs::read(&path).unwrap()));
        }
    }
}

fn json(status: u16, body: Vec<u8>) -> Response {
    Response {
        status,
        content_type: "application/json",
        body,
    }
}

fn handle(mut stream: TcpStream, state: &Mutex<State>, base_url: &str) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let path = lines
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .unwrap_or("")
        .to_owned();
    let token = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("x-figma-token"))
        .map(|(_, v)| v.trim().to_owned());

    let (route, query) = path.split_once('?').unwrap_or((&path, ""));
    let resp = {
        let mut state = state.lock().unwrap();
        state.log.push(Req {
            path: path.clone(),
            token,
        });
        state
            .exact
            .get(&path)
            .or_else(|| state.routes.get(route))
            .cloned()
    };
    let resp = resp.unwrap_or_else(|| synthesized(route, query, base_url));

    let reason = match resp.status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let header = format!(
        "HTTP/1.1 {} {reason}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        resp.status,
        resp.content_type,
        resp.body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&resp.body);
    let _ = stream.flush();
}

/// `/v1/images/{key}` and `/img/*`, or a loud 404 for anything unrecorded.
fn synthesized(route: &str, query: &str, base_url: &str) -> Response {
    if route.starts_with("/v1/images/") {
        let param = |name: &str| {
            query
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == name)
                .map(|(_, v)| decode(v))
        };
        let format = param("format").unwrap_or_else(|| "png".into());
        let images: serde_json::Map<String, serde_json::Value> = param("ids")
            .unwrap_or_default()
            .split(',')
            .filter(|id| !id.is_empty())
            .map(|id| {
                let file = id.replace([':', ';'], "-");
                (
                    id.to_owned(),
                    format!("{base_url}/img/{file}.{format}").into(),
                )
            })
            .collect();
        let body = serde_json::json!({ "err": null, "images": images });
        return json(200, body.to_string().into_bytes());
    }
    if let Some(name) = route.strip_prefix("/img/") {
        return if name.ends_with(".svg") {
            Response {
                status: 200,
                content_type: "image/svg+xml",
                body: SVG.as_bytes().to_vec(),
            }
        } else {
            Response {
                status: 200,
                content_type: "image/png",
                body: PNG.to_vec(),
            }
        };
    }
    json(
        404,
        format!(r#"{{"status":404,"err":"no fixture for {route}"}}"#).into_bytes(),
    )
}

/// Percent-decode a query value (`1%3A2` → `1:2`).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

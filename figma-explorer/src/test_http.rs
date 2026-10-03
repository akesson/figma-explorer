//! Test-only one-shot HTTP server: serves a queue of canned responses in
//! order and records each request path, so network-touching code (listings,
//! version probes, refetches) can be tested against `cfg.base_path`.
//! A queued response nobody requests makes `Server::paths()` block — queue
//! exactly what the code under test should ask for.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

pub struct Server {
    pub base_url: String,
    pub paths: Arc<Mutex<Vec<String>>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

pub fn serve(responses: Vec<(u16, String)>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let paths = Arc::new(Mutex::new(Vec::new()));
    let paths_t = Arc::clone(&paths);
    let handle = std::thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&buf);
            let path = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            paths_t.lock().unwrap().push(path);
            let reason = match status {
                200 => "OK",
                403 => "Forbidden",
                _ => "Other",
            };
            let resp = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
        }
    });
    Server {
        base_url,
        paths,
        handle: Some(handle),
    }
}

impl Server {
    pub fn paths(mut self) -> Vec<String> {
        self.handle.take().unwrap().join().unwrap();
        self.paths.lock().unwrap().clone()
    }
}

/// A `Configuration` pointed at `server`.
pub fn cfg_for(server: &Server) -> figma_api::apis::configuration::Configuration {
    let mut cfg = figma_api::apis::configuration::Configuration::new();
    cfg.base_path = server.base_url.clone();
    cfg
}

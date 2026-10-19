//! The plugin's loopback HTTP server, mounted by the POM under
//! `/api/ui/plugins/ghostly/proxy/` (`ui.upstream`). Only the node reaches
//! it: every request carries `x-pom-plugin-token`.
//!
//! - `GET /web/…`: Ghostly's own web app, built for this path
//!   (`runtime::WEB_BASE`). It keeps its profile in the browser.
//! - `GET /state`: the node's member and the network's group (`Chat::snapshot`).
//! - `POST /sign` `{"statement": "…"}`: the node key's signature over the
//!   browser profile's `did:key` identity statement (`Chat::sign`).

use crate::chat::Chat;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_REQUEST: usize = 64 * 1024;

pub struct Upstream {
    port: u16,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Upstream {
    pub fn start(chat: Arc<Chat>) -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| error.to_string())?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        let token = token(port);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_token = token.clone();
        let thread = thread::Builder::new()
            .name("pom-plugin-ghostly-upstream".into())
            .spawn(move || serve(listener, chat, thread_token, thread_stop))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            port,
            token,
            stop,
            thread: Some(thread),
        })
    }

    pub fn response(&self) -> Value {
        json!({"status": "ready", "port": self.port, "token": self.token})
    }
}

impl Drop for Upstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A secret the POM sends back on every proxied request.
fn token(port: u16) -> String {
    let mut seed = [0_u8; 32];
    if let Ok(mut random) = std::fs::File::open("/dev/urandom") {
        let _ = random.read_exact(&mut seed);
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut token: String = seed.iter().map(|byte| format!("{byte:02x}")).collect();
    token.push_str(&format!("{nanos:x}{port:04x}"));
    token
}

fn serve(listener: TcpListener, chat: Arc<Chat>, token: String, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                let chat = Arc::clone(&chat);
                let token = token.clone();
                // A send waits on the CLI; reading the state must not wait behind it.
                let _ = thread::Builder::new()
                    .name("pom-plugin-ghostly-request".into())
                    .spawn(move || handle(stream, &chat, &token));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
}

struct Request {
    method: String,
    path: String,
    token: Option<String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    let header_end = loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        if buffer.len() > MAX_REQUEST {
            return None;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = head.lines();
    let mut parts = lines.next()?.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.split('?').next()?.to_owned();
    let mut token = None;
    let mut length = 0_usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("x-pom-plugin-token") {
            token = Some(value.trim().to_owned());
        } else if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
    }
    if length > MAX_REQUEST {
        return None;
    }
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(length);
    Some(Request {
        method,
        path,
        token,
        body,
    })
}

fn handle(mut stream: TcpStream, chat: &Chat, token: &str) {
    let Some(request) = read_request(&mut stream) else {
        write_json(
            &mut stream,
            "400 Bad Request",
            &json!({"error": "bad request"}),
        );
        return;
    };
    if request.token.as_deref() != Some(token) {
        write_json(
            &mut stream,
            "401 Unauthorized",
            &json!({"error": "unauthorized"}),
        );
        return;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/state") => write_json(&mut stream, "200 OK", &chat.snapshot()),
        ("POST", "/sign") => {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let Some(statement) = body["statement"].as_str() else {
                write_json(
                    &mut stream,
                    "400 Bad Request",
                    &json!({"error": "statement is required"}),
                );
                return;
            };
            match chat.sign(statement.trim_end_matches(['\r', '\n'])) {
                Ok(signed) => write_json(&mut stream, "200 OK", &signed),
                Err(error) => write_json(
                    &mut stream,
                    "422 Unprocessable Entity",
                    &json!({"error": error}),
                ),
            }
        }
        ("GET" | "HEAD", path) if path == "/web" || path.starts_with("/web/") => {
            let head = request.method == "HEAD";
            match chat.web_root() {
                Some(root) => serve_web(&mut stream, &root, &path[4..], head),
                None => write_json(
                    &mut stream,
                    "503 Service Unavailable",
                    &json!({"error": "Ghostly is still being installed"}),
                ),
            }
        }
        _ => write_json(&mut stream, "404 Not Found", &json!({"error": "not found"})),
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("ttf") => "font/ttf",
        Some("mp3") => "audio/mpeg",
        Some("ogg") => "audio/ogg",
        Some("wav") => "audio/wav",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file a web path names, inside `root`. `None` for a path that leaves it.
pub fn web_file(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_start_matches('/'));
    if path.contains('%')
        || path.contains('\\')
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let file = root.join(relative);
    if file.is_file() {
        return Some(file);
    }
    // The app routes in the hash; anything without an extension is the app.
    (relative.extension().is_none()).then(|| root.join("index.html"))
}

fn serve_web(stream: &mut TcpStream, root: &Path, path: &str, head: bool) {
    let Some(file) = web_file(root, path) else {
        write_json(stream, "404 Not Found", &json!({"error": "not found"}));
        return;
    };
    let Ok(body) = std::fs::read(&file) else {
        write_json(stream, "404 Not Found", &json!({"error": "not found"}));
        return;
    };
    // Built assets carry their hash in the name; the rest is checked each time.
    let cache = if file
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "assets")
    {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nCache-Control: {cache}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        content_type(&file),
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    if !head {
        let _ = stream.write_all(&body);
    }
}

fn write_json(stream: &mut TcpStream, status: &str, body: &Value) {
    let Ok(body) = serde_json::to_vec(body) else {
        return;
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_paths_stay_inside_the_app() {
        let root =
            std::env::temp_dir().join(format!("pom-plugin-ghostly-web-{}", std::process::id()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), "app").unwrap();
        std::fs::write(root.join("assets").join("main.js"), "js").unwrap();
        assert_eq!(web_file(&root, "/"), Some(root.join("index.html")));
        assert_eq!(web_file(&root, ""), Some(root.join("index.html")));
        assert_eq!(
            web_file(&root, "/assets/main.js"),
            Some(root.join("assets").join("main.js"))
        );
        assert_eq!(web_file(&root, "/assets/missing.js"), None);
        assert_eq!(web_file(&root, "/../secret"), None);
        assert_eq!(web_file(&root, "/assets/%2e%2e/x"), None);
        assert_eq!(content_type(Path::new("a.wasm")), "application/wasm");
        std::fs::remove_dir_all(root).unwrap();
    }
}

//! The local web page: a tiny HTTP server on 127.0.0.1 that the browser polls.

use crate::media::{self, lock, Shared};
use crate::platform;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const PAGE: &str = include_str!("ui.html");
const PING_REPLY: &str = "shrinker";
pub const DEFAULT_PORT: u16 = 4790;

fn header(k: &str, v: &str) -> Header {
    Header::from_bytes(k.as_bytes(), v.as_bytes()).expect("valid header")
}

fn send_json(rq: Request, v: Value) {
    let _ = rq.respond(Response::from_string(v.to_string()).with_header(header("Content-Type", "application/json")));
}

fn ok_or_err(rq: Request, r: Result<(), String>) {
    match r {
        Ok(()) => send_json(rq, json!({ "ok": true })),
        Err(e) => send_json(rq, json!({ "error": e })),
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| pct_decode(v))
    })
}

fn not_while_running(sh: &Shared) -> Result<(), String> {
    if lock(sh).running { Err("Stop first.".into()) } else { Ok(()) }
}

fn current_dir_or_default(sh: &Shared) -> PathBuf {
    let d = lock(sh).dir_path.clone();
    if d.as_os_str().is_empty() { default_dir() } else { d }
}

/// Videos (Movies on macOS) or Pictures, whichever exists, else home.
pub fn default_dir() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_default();
    let videos = home.join(if cfg!(target_os = "macos") { "Movies" } else { "Videos" });
    [videos, home.join("Pictures")].into_iter().find(|p| p.is_dir()).unwrap_or(home)
}

fn handle(sh: Shared, mut rq: Request) {
    let url = rq.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let (path, query) = (path.to_string(), query.to_string());

    if *rq.method() == Method::Get {
        let resp = match path.as_str() {
            "/" => Response::from_string(PAGE).with_header(header("Content-Type", "text/html; charset=utf-8")),
            "/api/ping" => Response::from_string(PING_REPLY),
            "/api/state" => Response::from_string(serde_json::to_string(&*lock(&sh)).unwrap_or_default())
                .with_header(header("Content-Type", "application/json"))
                .with_header(header("Cache-Control", "no-store")),
            "/api/thumb" => match media::thumbnail(&sh, &query_param(&query, "name").unwrap_or_default()) {
                Some(img) => Response::from_data(img)
                    .with_header(header("Content-Type", "image/webp"))
                    .with_header(header("Cache-Control", "max-age=3600")),
                None => Response::from_string("").with_status_code(404),
            },
            _ => Response::from_string("").with_status_code(404),
        };
        let _ = rq.respond(resp);
        return;
    }

    let mut raw = String::new();
    let _ = rq.as_reader().read_to_string(&mut raw);
    let body: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);

    match path.as_str() {
        "/api/open-folder" => {
            let r = not_while_running(&sh).and_then(|_| {
                let p = match body["path"].as_str().map(str::trim).filter(|s| !s.is_empty()) {
                    Some(p) => Some(PathBuf::from(p.trim_matches('"'))),
                    None => platform::pick_folder(&current_dir_or_default(&sh))?,
                };
                match p {
                    Some(p) => media::scan(&sh, &p, None),
                    None => Ok(()),
                }
            });
            ok_or_err(rq, r)
        }
        "/api/pick-files" => {
            let r = not_while_running(&sh).and_then(|_| {
                let picked = platform::pick_files(&current_dir_or_default(&sh))?;
                let Some(first) = picked.first() else { return Ok(()) };
                let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
                let names = picked.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).collect();
                media::scan(&sh, &dir, Some(names))
            });
            ok_or_err(rq, r)
        }
        "/api/refresh" => {
            let r = not_while_running(&sh).and_then(|_| {
                let (dir, keep) = {
                    let app = lock(&sh);
                    (app.dir_path.clone(), app.files.iter().filter(|f| f.selected).map(|f| f.name.clone()).collect::<Vec<_>>())
                };
                if dir.as_os_str().is_empty() || !dir.is_dir() {
                    return Err("That folder isn't there any more.".into());
                }
                media::scan(&sh, &dir, Some(keep))
            });
            ok_or_err(rq, r)
        }
        "/api/select" => {
            let names: Vec<String> =
                body["names"].as_array().map(|a| a.iter().filter_map(|n| n.as_str().map(String::from)).collect()).unwrap_or_default();
            let value = body["value"].as_bool().unwrap_or(false);
            let mut app = lock(&sh);
            if !app.running {
                for f in app.files.iter_mut().filter(|f| f.ok && names.contains(&f.name)) {
                    f.selected = value;
                }
            }
            drop(app);
            ok_or_err(rq, Ok(()))
        }
        "/api/settings" => {
            let r = not_while_running(&sh).and_then(|_| {
                let mut app = lock(&sh);
                for (k, v) in body.as_object().into_iter().flatten() {
                    app.settings.set(k, v.as_str().unwrap_or_default())?;
                }
                Ok(())
            });
            ok_or_err(rq, r)
        }
        "/api/start" => ok_or_err(rq, media::start(&sh)),
        "/api/stop" => {
            media::stop(&sh);
            ok_or_err(rq, Ok(()))
        }
        "/api/discard" => {
            let name = body["name"].as_str().unwrap_or_default().to_string();
            ok_or_err(rq, media::discard(&sh, &name))
        }
        "/api/reveal" => {
            let app = lock(&sh);
            let dir = app.dir_path.clone();
            let output = body["name"].as_str().and_then(|n| app.files.iter().find(|f| f.name == n)).and_then(|f| f.output.clone());
            drop(app);
            let target = output.map(PathBuf::from).unwrap_or_else(|| media::out_dir(&dir));
            platform::reveal(if target.exists() { &target } else { &dir });
            ok_or_err(rq, Ok(()))
        }
        "/api/quit" => {
            // Stop the encode (finished chunks are kept for next time) and exit.
            media::stop(&sh);
            ok_or_err(rq, Ok(()));
            std::thread::spawn(|| {
                std::thread::sleep(Duration::from_millis(600));
                std::process::exit(0);
            });
        }
        _ => {
            let _ = rq.respond(Response::from_string("").with_status_code(404));
        }
    }
}

/// Plain HTTP/1.0 to an already-running Shrinker on this machine.
fn talk(port: u16, method: &str, path: &str, body: &str) -> Option<String> {
    let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_millis(400)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(20))).ok()?;
    write!(s, "{method} {path} HTTP/1.0\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).ok()?;
    let mut resp = String::new();
    s.read_to_string(&mut resp).ok()?;
    resp.split_once("\r\n\r\n").map(|(_, b)| b.to_string())
}

/// If Shrinker is already running, hand it the folder/file and show its page
/// instead of starting a second copy. Returns true when it did.
pub fn hand_to_running(port: u16, start: Option<&Path>, open_browser: bool) -> bool {
    for p in port..port.saturating_add(20) {
        if talk(p, "GET", "/api/ping", "").as_deref() == Some(PING_REPLY) {
            if let Some(path) = start {
                let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
                let _ = talk(p, "POST", "/api/open-folder", &json!({ "path": abs.display().to_string() }).to_string());
            }
            println!("  Shrinker is already running: http://localhost:{p}");
            if open_browser {
                platform::open_url(&format!("http://localhost:{p}"));
            }
            return true;
        }
    }
    false
}

pub fn run(sh: Shared, start: Option<PathBuf>, port: u16, open_browser: bool) -> Result<(), String> {
    let mut bound = None;
    for p in port..port.saturating_add(20) {
        if let Ok(s) = Server::http(("127.0.0.1", p)) {
            bound = Some((s, p));
            break;
        }
    }
    let (server, port) = bound.ok_or(format!("No free port between {port} and {}.", port.saturating_add(19)))?;
    let url = format!("http://localhost:{port}");
    println!();
    println!("  Shrinker is running at {url}");
    println!("  Keep this window open while it works (or use Quit on the page).");
    println!("  Stopping keeps finished work; the next Start carries on from there.");
    println!();
    platform::hide_own_console();

    if lock(&sh).problem.is_none() {
        let sh = sh.clone();
        let target = start.unwrap_or_else(default_dir);
        std::thread::spawn(move || {
            if let Err(e) = media::scan(&sh, &target, None) {
                lock(&sh).message = e;
            }
        });
    }
    if open_browser {
        platform::open_url(&url);
    }
    for rq in server.incoming_requests() {
        let sh = sh.clone();
        std::thread::spawn(move || handle(sh, rq));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_decoding() {
        assert_eq!(query_param("name=My%20clip%20'test'%20100%25%20%E2%80%93%20%C3%A9.mp4", "name").unwrap(), "My clip 'test' 100% – é.mp4");
        assert_eq!(query_param("a=1&name=x+y", "name").unwrap(), "x y");
        assert_eq!(query_param("name=50%", "name").unwrap(), "50%");
        assert!(query_param("other=1", "name").is_none());
    }
}

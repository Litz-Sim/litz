//! `litz serve`: a static file server for a generated site.
//!
//! A page that loads a wasm module or a firmware image cannot do so over
//! `file://`, so this answers GET requests out of one directory with the
//! right content types. It binds to loopback only: this is a development
//! tool, and a directory server reachable from the network is a different
//! product.
//!
//! Nothing here is asynchronous. A visitor makes a handful of requests and
//! then works inside the page, so a thread per connection costs less than
//! the complexity of a runtime.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};

pub const DEFAULT_PORT: u16 = 8000;

/// Serve `root` until the process is interrupted. `port` 0 asks the system to
/// pick one, which is what the tests do.
pub fn serve(root: &Path, port: u16) -> Result<(), String> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    println!(
        "litz: serving {} at http://127.0.0.1:{port}/",
        root.display()
    );
    println!("litz: press Ctrl-C to stop");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let root = root.to_path_buf();
                std::thread::spawn(move || {
                    // A browser dropping a connection mid-request is routine,
                    // so a failed write is not worth a message.
                    let _ = answer(stream, &root);
                });
            }
            Err(e) => eprintln!("litz: {e}"),
        }
    }
    Ok(())
}

fn answer(mut stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]);
    let Some(line) = request.lines().next() else {
        return respond(&mut stream, 400, "text/plain", b"bad request");
    };
    let mut parts = line.split(' ');
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return respond(&mut stream, 405, "text/plain", b"only GET is served");
    };

    let Some(path) = resolve(root, target) else {
        return respond(
            &mut stream,
            403,
            "text/plain",
            b"outside the served directory",
        );
    };
    match std::fs::read(&path) {
        Ok(body) => respond(&mut stream, 200, mime(&path), &body),
        Err(_) => respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

/// A request target to a path under `root`, or `None` if it escapes.
///
/// `..` is rejected rather than normalised away: a path that climbs is a
/// mistake or an attack, and neither deserves a guess at what was meant.
fn resolve(root: &Path, target: &str) -> Option<PathBuf> {
    let target = target.split(['?', '#']).next().unwrap_or("");
    let mut relative = PathBuf::new();
    for piece in target.split('/') {
        if piece.is_empty() || piece == "." {
            continue;
        }
        if piece == ".." || piece.contains('\\') || piece.contains('\0') {
            return None;
        }
        relative.push(piece);
    }
    if relative.as_os_str().is_empty() || relative.extension().is_none() {
        relative.push("index.html");
    }
    // Belt and braces: whatever the join produced must still start at root.
    if !relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(relative))
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn respond(stream: &mut TcpStream, status: u16, mime: &str, body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Unknown",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_path_serves_the_index() {
        assert_eq!(
            resolve(Path::new("site"), "/"),
            Some(PathBuf::from("site/index.html"))
        );
        assert_eq!(
            resolve(Path::new("site"), "/demo"),
            Some(PathBuf::from("site/demo/index.html"))
        );
    }

    #[test]
    fn a_file_path_is_kept_whole() {
        assert_eq!(
            resolve(Path::new("site"), "/litz_wasm.wasm?v=2"),
            Some(PathBuf::from("site/litz_wasm.wasm"))
        );
    }

    #[test]
    fn climbing_out_of_the_root_is_refused() {
        assert_eq!(resolve(Path::new("site"), "/../Cargo.toml"), None);
        assert_eq!(resolve(Path::new("site"), "/demo/../../Cargo.toml"), None);
    }

    #[test]
    fn content_types_are_the_ones_a_browser_needs() {
        assert_eq!(mime(Path::new("a.wasm")), "application/wasm");
        assert_eq!(mime(Path::new("a.js")), "text/javascript; charset=utf-8");
        assert_eq!(mime(Path::new("a.bin")), "application/octet-stream");
    }
}

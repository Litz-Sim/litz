//! `litz connect-editor`: the link between a build and the editor page.
//!
//! The editor cannot run a firmware toolchain, and `litz build` cannot draw a
//! circuit. This module is the smallest bridge between the two: one process,
//! one loopback port, a pairing token, WebSocket text messages for commands
//! and build output, and an in-memory artifact store the editor fetches over
//! plain HTTP.
//!
//! Nothing leaves the machine on its own. The token is the only way in: a page
//! that was not handed the URL printed by `connect-editor` gets 403 on every
//! route. The socket accepts five commands — build this project's own recipe,
//! accept bytes a `litz build --push` already produced, read or write the
//! editor's project library, and make one HTTP request from this machine — and
//! never a command string, so the link is not a shell.
//!
//! The library is the only place the link touches the filesystem for the page:
//! flat `.litzproject.json` files under `.litz/projects`, each named by a stem
//! that cannot leave that directory. A forwarded request leaves the machine
//! because the browser is not allowed to make it (a bucket's control API has
//! no CORS): the page names the method, URL, headers and body, and the link
//! answers with the status, headers and body it got back.
//!
//! The code is a credential the user may have bookmarked, so a link that
//! starts where one ran before keeps the code `.litz/editor.json` remembers
//! unless `--token` names another.

use crate::build;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 4317;

/// A connection that sends nothing for this long is dropped. The editor pings
/// every 25 seconds, so this only ends links whose page is gone.
const IDLE: Duration = Duration::from_secs(90);
/// JSON messages are tiny; only a pushed image is large.
const MAX_FRAME: usize = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 8;
const LOCK_FILE: &str = ".litz/editor.json";
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Where the editor keeps the projects it saves through the link. A project is
/// one file, so the library is a directory the user can list, copy or commit.
const LIBRARY: &str = ".litz/projects";
const PROJECT_SUFFIX: &str = ".litzproject.json";
/// A project carries its parts, code and wires; 8 MB is generous for one and
/// still small enough that a save cannot stall the socket.
const MAX_PROJECT: usize = 8 * 1024 * 1024;
/// A forwarded request is a form or a JSON document; an answer is usually one
/// too, and anything larger is a file the page has its own way to fetch.
const MAX_FORWARD: usize = 1024 * 1024;
const MAX_REPLY: u64 = 4 * 1024 * 1024;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

/// One image the editor can fetch, keyed by its SHA-256.
struct Artifact {
    board: Option<String>,
    name: String,
    bytes: Arc<Vec<u8>>,
}

/// A connected page (or a `--push` client) and the channel its writer drains.
struct Session {
    id: u64,
    out: mpsc::Sender<Message>,
}

enum Message {
    Text(String),
    /// A WebSocket control frame. The server never sends a binary data frame:
    /// images leave through the artifact route, which the page fetches itself.
    Pong(Vec<u8>),
    Close,
}

/// What a link serves and who may reach it.
pub struct Options {
    pub directory: PathBuf,
    pub port: u16,
    /// Interface to bind. Loopback by default; a server sets this on purpose.
    pub host: String,
    /// Pages allowed to open the socket. Empty accepts any page that holds the
    /// code, which is the loopback case.
    pub origins: Vec<String>,
    /// An editor build to serve beside the link, so page and socket share one
    /// origin and the browser has nothing to negotiate.
    pub assets: Option<PathBuf>,
    /// The pairing code to use. Without one the link keeps the code its
    /// previous run left in `.litz/editor.json`, so a bookmark or a page that
    /// remembered the URL survives a restart; a first run makes one.
    pub token: Option<String>,
}

struct State {
    dir: PathBuf,
    token: String,
    origins: Vec<String>,
    assets: Option<PathBuf>,
    sessions: RwLock<Vec<Session>>,
    artifacts: Mutex<Vec<(String, Artifact)>>,
    next_id: AtomicU64,
    /// One build at a time: two recipes writing the same output directory
    /// would interleave their files and produce an image neither intended.
    building: AtomicBool,
}

impl State {
    fn keep(&self, sha: String, artifact: Artifact) {
        let mut artifacts = self.artifacts.lock().unwrap();
        artifacts.retain(|(key, _)| key != &sha);
        artifacts.push((sha, artifact));
        if artifacts.len() > MAX_ARTIFACTS {
            artifacts.remove(0);
        }
    }

    fn artifact(&self, sha: &str) -> Option<(Option<String>, String, Arc<Vec<u8>>)> {
        let artifacts = self.artifacts.lock().unwrap();
        artifacts
            .iter()
            .find(|(key, _)| key == sha)
            .map(|(_, artifact)| {
                (
                    artifact.board.clone(),
                    artifact.name.clone(),
                    artifact.bytes.clone(),
                )
            })
    }
}

/// Serve a project to an editor page until the process is interrupted.
///
/// `port` 0 asks the system for one, which is what the tests use.
pub fn serve(options: &Options) -> Result<(), String> {
    let (listener, token) = listen(options)?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("editor link: {e}"))?
        .port();
    // A wildcard bind is not an address a browser can use, so the printed URL
    // names loopback and says what to replace it with.
    let wildcard = options.host == "0.0.0.0" || options.host == "::";
    let host = if wildcard { "127.0.0.1" } else { &options.host };
    // One line to copy: the page reads the pairing code from the fragment, so
    // the code never reaches the server in a request line or a log.
    match &options.assets {
        Some(assets) => {
            println!("litz: editor at http://{host}:{port}/#{token}");
            println!("litz: serving {} as well; Ctrl-C to stop", assets.display());
        }
        None => {
            println!("litz: editor link ws://{host}:{port}/ws#{token}");
            println!("litz: paste that into the editor's Connect dialog; Ctrl-C to stop");
        }
    }
    if wildcard {
        println!(
            "litz: listening on every interface; replace 127.0.0.1 with the name the editor reaches"
        );
    }
    if wildcard || !loopback(&options.host) {
        if options.origins.is_empty() {
            eprintln!(
                "litz: warning: bound to {} without --origin; any page holding the code may attach",
                options.host
            );
        }
        eprintln!("litz: warning: this listener speaks plain HTTP; terminate TLS in front of it");
    }
    println!("litz: `litz build --push` from another terminal sends an image here");
    accept_loop(listener, Arc::new(state(options, token)));
    Ok(())
}

fn loopback(host: &str) -> bool {
    host == "127.0.0.1" || host == "::1" || host == "localhost"
}

/// Bind the listener and claim the project's lock file.
fn listen(options: &Options) -> Result<(TcpListener, String), String> {
    let dir = options
        .directory
        .canonicalize()
        .map_err(|e| format!("{}: {e}", options.directory.display()))?;
    if let Ok((running, _)) = read_lock(&dir)
        && TcpStream::connect(("127.0.0.1", running)).is_ok()
    {
        return Err(format!(
            "an editor link is already running on port {running}; close it first"
        ));
    }
    let listener = TcpListener::bind((options.host.as_str(), options.port))
        .map_err(|e| format!("cannot listen on {}:{}: {e}", options.host, options.port))?;
    let actual = listener
        .local_addr()
        .map_err(|e| format!("editor link: {e}"))?
        .port();
    // A code is a credential the user may have put in a bookmark or left in a
    // page's storage, so the link keeps the one it used last unless the caller
    // asked for a new one. The liveness check above already showed that the
    // file names no running link.
    let token = match &options.token {
        Some(code) => code_of(code).ok_or_else(|| {
            format!(
                "{code:?} is not a pairing code: 4 to 64 letters, digits, dots, dashes or tildes"
            )
        })?,
        None => read_lock(&dir)
            .map(|(_, remembered)| remembered)
            .unwrap_or_else(|_| token()),
    };
    write_lock(&dir, actual, &token)?;
    Ok((listener, token))
}

fn state(options: &Options, token: String) -> State {
    State {
        dir: options.directory.to_path_buf(),
        token,
        origins: options.origins.clone(),
        assets: options.assets.clone(),
        sessions: RwLock::new(Vec::new()),
        artifacts: Mutex::new(Vec::new()),
        next_id: AtomicU64::new(1),
        building: AtomicBool::new(false),
    }
}

fn accept_loop(listener: TcpListener, state: Arc<State>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = state.clone();
                std::thread::spawn(move || handle(stream, state));
            }
            Err(e) => eprintln!("litz: editor link: {e}"),
        }
    }
}

/// A project's built image, ready to hand to an editor.
///
/// `board` names a `[[board]]` entry; without one the recipe's `[build]
/// firmware` is the answer, which is what a single-board project builds.
pub fn push(dir: &Path, board: Option<&str>) -> Result<String, String> {
    let (port, token) = read_lock(&dir.canonicalize().map_err(|e| e.to_string())?)?;
    let (name, path) = build::image_for(dir, board)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sha = hex(&Sha256::digest(&bytes));
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "firmware.bin".to_string());

    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("editor link on port {port}: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    client_handshake(&mut stream, port, &token)?;
    let published = publish_message(board.or(name.as_deref()), &file, &sha, bytes.len());
    write_frame(&mut stream, 0x1, published.as_bytes(), true)?;
    let ready = read_text(&mut stream, false)?;
    if ready.get("type").and_then(|kind| kind.as_str()) != Some("publish.ready") {
        return Err(format!("editor link refused the image: {ready}"));
    }
    write_frame(&mut stream, 0x2, &bytes, true)?;
    let done = read_text(&mut stream, false)?;
    match done.get("type").and_then(|kind| kind.as_str()) {
        Some("pushed") => Ok(format!("pushed {file} to the editor ({sha})")),
        _ => Err(format!("editor link did not accept the image: {done}")),
    }
}

fn publish_message(board: Option<&str>, file: &str, sha: &str, size: usize) -> String {
    serde_json::json!({
        "type": "publish",
        "board": board,
        "name": file,
        "sha256": sha,
        "size": size,
    })
    .to_string()
}

// ---------------------------------------------------------------------------
// HTTP side

fn handle(mut stream: TcpStream, state: Arc<State>) {
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(IDLE));
    let Ok(request) = read_request(&mut stream) else {
        return;
    };
    let origin = request.headers.get("origin").map(String::as_str);
    if !allowed(&state.origins, origin) {
        let _ = respond(
            &mut stream,
            403,
            "text/plain",
            b"this page is not allowed on this link",
            origin,
            &[],
        );
        return;
    }
    let (path, query) = split_target(&request.target);
    // The code arrives in the query for a plain link, in the socket protocol or
    // an Authorization header once a page wants it out of request lines.
    let bearer = request
        .headers
        .get("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    let offered = offered_token(
        request
            .headers
            .get("sec-websocket-protocol")
            .map(String::as_str),
    );
    let provided = query.get("token").map(String::as_str).unwrap_or("");
    let authorized =
        same(&state.token, provided) || same(&state.token, bearer) || same(&state.token, offered);
    let protocol = if request.headers.contains_key("sec-websocket-protocol") {
        "Sec-WebSocket-Protocol: litz\r\n"
    } else {
        ""
    };

    if request.method == "OPTIONS" {
        let _ = respond(&mut stream, 204, "text/plain", b"", origin, &[]);
        return;
    }
    match path.as_str() {
        "/health" => {
            let body = serde_json::json!({
                "ok": true,
                "version": env!("CARGO_PKG_VERSION"),
                "project": state.dir.file_name().and_then(|name| name.to_str()).unwrap_or(""),
            })
            .to_string();
            let _ = respond(
                &mut stream,
                200,
                "application/json",
                body.as_bytes(),
                origin,
                &[],
            );
        }
        "/ws" => {
            if !authorized {
                let _ = respond(
                    &mut stream,
                    403,
                    "text/plain",
                    b"the pairing code does not match",
                    origin,
                    &[],
                );
                return;
            }
            let Some(key) = request.headers.get("sec-websocket-key") else {
                let _ = respond(
                    &mut stream,
                    400,
                    "text/plain",
                    b"not a websocket handshake",
                    origin,
                    &[],
                );
                return;
            };
            let accept = base64(&sha1(format!("{key}{GUID}").as_bytes()));
            let _ = write!(
                stream,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{protocol}\r\n"
            );
            run_socket(stream, state);
        }
        target if target.starts_with("/artifact/") => {
            if !authorized {
                let _ = respond(
                    &mut stream,
                    403,
                    "text/plain",
                    b"the pairing code does not match",
                    origin,
                    &[],
                );
                return;
            }
            let sha = &target["/artifact/".len()..];
            match state.artifact(sha) {
                Some((_, _, bytes)) => {
                    let _ = respond(
                        &mut stream,
                        200,
                        "application/octet-stream",
                        &bytes,
                        origin,
                        &["Content-Disposition", "attachment"],
                    );
                }
                None => {
                    let _ = respond(
                        &mut stream,
                        404,
                        "text/plain",
                        b"no such image",
                        origin,
                        &[],
                    );
                }
            }
        }
        _ => match &state.assets {
            Some(root) if request.method == "GET" => {
                let _ = serve_asset(&mut stream, root, &request.target, origin);
            }
            Some(_) => {
                let _ = respond(
                    &mut stream,
                    405,
                    "text/plain",
                    b"only GET is served",
                    origin,
                    &[],
                );
            }
            None => {
                let _ = respond(&mut stream, 404, "text/plain", b"not found", origin, &[]);
            }
        },
    }
}

/// Serve the editor build. A path with no file behind it goes to `index.html`:
/// the page owns its routes, not the file tree.
fn serve_asset(
    stream: &mut TcpStream,
    root: &Path,
    target: &str,
    origin: Option<&str>,
) -> std::io::Result<()> {
    let Some(path) = crate::serve::resolve(root, target) else {
        return respond(
            stream,
            403,
            "text/plain",
            b"outside the served directory",
            origin,
            &[],
        );
    };
    if let Ok(body) = std::fs::read(&path) {
        return respond(stream, 200, crate::serve::mime(&path), &body, origin, &[]);
    }
    match std::fs::read(root.join("index.html")) {
        Ok(body) => respond(stream, 200, "text/html; charset=utf-8", &body, origin, &[]),
        Err(_) => respond(stream, 404, "text/plain", b"not found", origin, &[]),
    }
}

/// A client that names no Origin (curl, `litz build --push`) is fine. A page
/// that names one must be on the list, when a list was given.
fn allowed(origins: &[String], origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(origin) => origins.is_empty() || origins.iter().any(|entry| entry == origin),
    }
}

/// The code a page offered as `Sec-WebSocket-Protocol: litz, CODE`.
fn offered_token(header: Option<&str>) -> &str {
    header
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|part| *part != "litz")
                .unwrap_or("")
        })
        .unwrap_or("")
}

struct Request {
    method: String,
    target: String,
    headers: HashMap<String, String>,
}

/// Read a request head without swallowing the bytes that follow it: the first
/// WebSocket frame may already be in flight when the handshake is answered.
fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) => return Err("connection closed during the request".to_string()),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(e.to_string()),
        }
        if head.len() > 16 * 1024 {
            return Err("request head too large".to_string());
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Ok(Request {
        method,
        target,
        headers,
    })
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut values = HashMap::new();
    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            values.insert(key.to_string(), value.replace("%2D", "-"));
        }
    }
    (path.to_string(), values)
}

#[allow(clippy::too_many_arguments)]
fn respond(
    stream: &mut TcpStream,
    status: u16,
    mime: &str,
    body: &[u8],
    origin: Option<&str>,
    extra: &[&str],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Unknown",
    };
    let allow_origin = origin.unwrap_or("*");
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: {allow_origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Expose-Headers: Content-Length\r\nVary: Origin\r\n",
        body.len()
    )?;
    let mut index = 0;
    while index + 1 < extra.len() {
        write!(stream, "{}: {}\r\n", extra[index], extra[index + 1])?;
        index += 2;
    }
    write!(stream, "\r\n")?;
    stream.write_all(body)
}

// ---------------------------------------------------------------------------
// WebSocket side

fn run_socket(stream: TcpStream, state: Arc<State>) {
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let (out, incoming) = mpsc::channel();
    std::thread::spawn(move || write_loop(writer_stream, incoming));

    let id = state.next_id.fetch_add(1, Ordering::SeqCst);
    state.sessions.write().unwrap().push(Session {
        id,
        out: out.clone(),
    });

    let hello = serde_json::json!({
        "type": "hello",
        "version": env!("CARGO_PKG_VERSION"),
        "project": state.dir.file_name().and_then(|name| name.to_str()).unwrap_or(""),
        "library": library(&state.dir).display().to_string(),
        "boards": boards(&state.dir),
    });
    let _ = out.send(Message::Text(hello.to_string()));

    let mut stream = stream;
    let mut upload: Option<Upload> = None;
    while let Ok(frame) = read_frame(&mut stream, true) {
        match frame.opcode {
            0x8 => break,
            0x9 => {
                let _ = out.send(Message::Pong(frame.payload));
            }
            0xA => {}
            0x2 => {
                let Some(pending) = upload.take() else {
                    let _ = out.send(Message::Text(error("an image arrived without a publish")));
                    continue;
                };
                accept_image(&state, id, &out, pending, &frame.payload);
            }
            0x1 => {
                let Ok(message) = serde_json::from_slice::<serde_json::Value>(&frame.payload)
                else {
                    let _ = out.send(Message::Text(error("each message is a JSON object")));
                    continue;
                };
                // Only requests that reach the filesystem carry an id: the
                // page awaits one answer and must not mistake it for another.
                let id = message.get("id").and_then(|value| value.as_u64());
                match message.get("type").and_then(|kind| kind.as_str()) {
                    Some("ping") => {
                        let _ = out.send(Message::Text(r#"{"type":"pong"}"#.to_string()));
                    }
                    Some("projects.list") => reply(
                        &out,
                        id,
                        serde_json::json!({
                            "type": "projects.listed",
                            "items": list_projects(&state.dir),
                        }),
                    ),
                    Some("project.read") => {
                        let name = message.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        match project_path(&state.dir, name) {
                            Some(path) => match read_project(&path) {
                                Ok(body) => reply(
                                    &out,
                                    id,
                                    serde_json::json!({
                                        "type": "project.text",
                                        "key": name.trim(),
                                        "text": body,
                                    }),
                                ),
                                Err(failure) => reply(&out, id, failure_message(&failure)),
                            },
                            None => reply(&out, id, failure_message(&bad_name(name))),
                        }
                    }
                    Some("project.write") => {
                        let name = message.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let body = message.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        match project_path(&state.dir, name) {
                            Some(path) if body.len() <= MAX_PROJECT => {
                                match write_project(&path, body) {
                                    Ok(()) => reply(
                                        &out,
                                        id,
                                        serde_json::json!({
                                            "type": "project.written",
                                            "key": name.trim(),
                                        }),
                                    ),
                                    Err(failure) => reply(&out, id, failure_message(&failure)),
                                }
                            }
                            Some(_) => {
                                reply(&out, id, failure_message("the project is larger than 8 MB"))
                            }
                            None => reply(&out, id, failure_message(&bad_name(name))),
                        }
                    }
                    Some("project.remove") => {
                        let name = message.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        match project_path(&state.dir, name) {
                            Some(path) => match std::fs::remove_file(&path) {
                                Ok(()) => reply(
                                    &out,
                                    id,
                                    serde_json::json!({
                                        "type": "project.removed",
                                        "key": name.trim(),
                                    }),
                                ),
                                Err(failure) => reply(
                                    &out,
                                    id,
                                    failure_message(&format!("{}: {failure}", path.display())),
                                ),
                            },
                            None => reply(&out, id, failure_message(&bad_name(name))),
                        }
                    }
                    Some("http.request") => match Forward::from_message(&message) {
                        Some(request) => {
                            // A slow host must not stall the page's saves, so
                            // the answer comes back through the writer channel
                            // from its own thread.
                            let out = out.clone();
                            std::thread::spawn(move || {
                                let _ = out.send(Message::Text(forward(id, &request).to_string()));
                            });
                        }
                        None => reply(
                            &out,
                            id,
                            failure_message(
                                "http.request needs an http(s) url, a method and at most 1 MB",
                            ),
                        ),
                    },
                    Some("build") => {
                        let board = message
                            .get("board")
                            .and_then(|value| value.as_str())
                            .map(str::to_string);
                        start_build(state.clone(), out.clone(), board);
                    }
                    Some("publish") => match Upload::from_message(&message) {
                        Some(pending) => {
                            upload = Some(pending);
                            let _ =
                                out.send(Message::Text(r#"{"type":"publish.ready"}"#.to_string()));
                        }
                        None => {
                            let _ = out
                                .send(Message::Text(error("publish needs name, sha256 and size")));
                        }
                    },
                    Some(other) => {
                        let _ =
                            out.send(Message::Text(error(&format!("unknown message {other:?}"))));
                    }
                    None => {
                        let _ = out.send(Message::Text(error("a message needs a type")));
                    }
                }
            }
            _ => {}
        }
    }

    state
        .sessions
        .write()
        .unwrap()
        .retain(|session| session.id != id);
    let _ = out.send(Message::Close);
}

fn boards(dir: &Path) -> Vec<String> {
    litz_engine::config::Config::load_project(dir)
        .map(|config| config.boards.into_iter().map(|entry| entry.id).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Project library

fn library(dir: &Path) -> PathBuf {
    dir.join(LIBRARY)
}

/// A name is a file stem, never a path: the library is flat, so nothing the
/// page sends can address a file outside it.
fn project_name(name: &str) -> Option<&str> {
    let name = name.trim();
    let plain = name
        .chars()
        .all(|c| !c.is_control() && c != '/' && c != '\\');
    (!name.is_empty() && name.len() <= 120 && !name.starts_with('.') && plain).then_some(name)
}

fn bad_name(name: &str) -> String {
    if name.trim().is_empty() {
        "a project needs a name".to_string()
    } else {
        "a project name cannot contain a path".to_string()
    }
}

fn project_path(dir: &Path, name: &str) -> Option<PathBuf> {
    Some(library(dir).join(format!("{}{PROJECT_SUFFIX}", project_name(name)?)))
}

fn list_projects(dir: &Path) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    let Ok(entries) = std::fs::read_dir(library(dir)) else {
        return items;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(PROJECT_SUFFIX))
        else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        // Milliseconds since the epoch: the page turns this into a local time,
        // which the CLI has no business deciding.
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|age| age.as_millis() as u64)
            .unwrap_or(0);
        items.push(serde_json::json!({
            "key": stem,
            "name": stem,
            "size": meta.len(),
            "modified": modified,
        }));
    }
    items.sort_by(|left, right| right["modified"].as_u64().cmp(&left["modified"].as_u64()));
    items
}

fn read_project(path: &Path) -> Result<String, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if size.len() > MAX_PROJECT as u64 {
        return Err("the project is larger than 8 MB".to_string());
    }
    std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_project(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    // Write beside the target and rename: a crash mid-save keeps the previous
    // project readable instead of leaving half a file.
    let staging = path.with_extension("json.tmp");
    std::fs::write(&staging, body).map_err(|e| format!("{}: {e}", staging.display()))?;
    std::fs::rename(&staging, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn error(message: &str) -> String {
    serde_json::json!({ "type": "error", "message": message }).to_string()
}

fn failure_message(message: &str) -> serde_json::Value {
    serde_json::json!({ "type": "error", "message": message })
}

/// Answer a request the page is waiting on. A message without an id was sent
/// fire-and-forget, and its answer needs nothing to match against.
fn reply(out: &mpsc::Sender<Message>, id: Option<u64>, mut value: serde_json::Value) {
    if let Some(id) = id {
        value["id"] = serde_json::json!(id);
    }
    let _ = out.send(Message::Text(value.to_string()));
}

struct Upload {
    board: Option<String>,
    name: String,
    sha: String,
    size: usize,
}

impl Upload {
    fn from_message(message: &serde_json::Value) -> Option<Self> {
        let text = |key: &str| {
            message
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        Some(Self {
            board: text("board"),
            name: text("name")?,
            sha: text("sha256")?,
            size: message.get("size")?.as_u64()? as usize,
        })
    }
}

fn accept_image(
    state: &State,
    id: u64,
    out: &mpsc::Sender<Message>,
    pending: Upload,
    bytes: &[u8],
) {
    if bytes.len() != pending.size {
        let _ = out.send(Message::Text(error(&format!(
            "expected {} bytes, received {}",
            pending.size,
            bytes.len()
        ))));
        return;
    }
    let sha = hex(&Sha256::digest(bytes));
    if !same(&sha, &pending.sha) {
        let _ = out.send(Message::Text(error("the image does not match its sha256")));
        return;
    }
    state.keep(
        sha.clone(),
        Artifact {
            board: pending.board.clone(),
            name: pending.name.clone(),
            bytes: Arc::new(bytes.to_vec()),
        },
    );
    // The publisher is answered with `pushed` instead: it asked for an
    // acknowledgement, not a second copy of what it just sent.
    broadcast_except(
        state,
        Some(id),
        &artifact_message(pending.board.as_deref(), &pending.name, &sha, bytes.len()),
    );
    let _ = out.send(Message::Text(
        serde_json::json!({ "type": "pushed", "sha256": sha }).to_string(),
    ));
}

fn start_build(state: Arc<State>, out: mpsc::Sender<Message>, board: Option<String>) {
    if state.building.swap(true, Ordering::SeqCst) {
        let _ = out.send(Message::Text(error("a build is already running")));
        return;
    }
    std::thread::spawn(move || {
        let started = serde_json::json!({ "type": "build.started", "board": board }).to_string();
        broadcast(&state, &started);
        let outcome = run_build(&state, &out, board.as_deref());
        if let Err(message) = outcome {
            broadcast(
                &state,
                &serde_json::json!({ "type": "build.failed", "message": message }).to_string(),
            );
        }
        state.building.store(false, Ordering::SeqCst);
    });
}

fn run_build(
    state: &State,
    out: &mpsc::Sender<Message>,
    board: Option<&str>,
) -> Result<(), String> {
    let mut emit = |chunk: &str| {
        let message = serde_json::json!({ "type": "log", "chunk": chunk }).to_string();
        let _ = out.send(Message::Text(message));
    };
    build::run_with(&state.dir, false, &mut emit)?;
    let (name, path) = build::image_for(&state.dir, board)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let size = bytes.len();
    let sha = hex(&Sha256::digest(&bytes));
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "firmware.bin".to_string());
    state.keep(
        sha.clone(),
        Artifact {
            board: name.clone(),
            name: file.clone(),
            bytes: Arc::new(bytes),
        },
    );
    broadcast(state, &artifact_message(name.as_deref(), &file, &sha, size));
    Ok(())
}

fn artifact_message(board: Option<&str>, name: &str, sha: &str, size: usize) -> String {
    serde_json::json!({
        "type": "artifact",
        "board": board,
        "name": name,
        "sha256": sha,
        "size": size,
        "url": format!("/artifact/{sha}"),
    })
    .to_string()
}

fn broadcast(state: &State, message: &str) {
    broadcast_except(state, None, message);
}

fn broadcast_except(state: &State, skip: Option<u64>, message: &str) {
    let sessions = state.sessions.read().unwrap();
    for session in sessions.iter() {
        if Some(session.id) == skip {
            continue;
        }
        let _ = session.out.send(Message::Text(message.to_string()));
    }
}

fn write_loop(mut stream: TcpStream, incoming: mpsc::Receiver<Message>) {
    while let Ok(message) = incoming.recv() {
        let written = match message {
            Message::Text(text) => write_frame(&mut stream, 0x1, text.as_bytes(), false),
            Message::Pong(payload) => write_frame(&mut stream, 0xA, &payload, false),
            Message::Close => {
                let _ = write_frame(&mut stream, 0x8, &[], false);
                return;
            }
        };
        if written.is_err() {
            return;
        }
    }
}

struct Frame {
    opcode: u8,
    payload: Vec<u8>,
}

fn read_frame(stream: &mut TcpStream, from_client: bool) -> Result<Frame, String> {
    let mut header = [0u8; 2];
    read_exact(stream, &mut header)?;
    let opcode = header[0] & 0x0f;
    let masked = header[1] & 0x80 != 0;
    let mut length = (header[1] & 0x7f) as u64;
    if length == 126 {
        let mut extended = [0u8; 2];
        read_exact(stream, &mut extended)?;
        length = u16::from_be_bytes(extended) as u64;
    } else if length == 127 {
        let mut extended = [0u8; 8];
        read_exact(stream, &mut extended)?;
        length = u64::from_be_bytes(extended);
    }
    if length as usize > MAX_FRAME {
        return Err("frame too large".to_string());
    }
    if from_client && !masked {
        // RFC 6455: every client frame is masked. Refusing the shape as well
        // as the token keeps a stray local sender from writing bytes verbatim.
        return Err("a client frame must be masked".to_string());
    }
    let mut payload = vec![0u8; length as usize];
    if masked {
        let mut mask = [0u8; 4];
        read_exact(stream, &mut mask)?;
        read_exact(stream, &mut payload)?;
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % 4];
        }
    } else {
        read_exact(stream, &mut payload)?;
    }
    Ok(Frame { opcode, payload })
}

fn write_frame(
    stream: &mut TcpStream,
    opcode: u8,
    payload: &[u8],
    mask: bool,
) -> Result<(), String> {
    let mut head = Vec::with_capacity(14);
    head.push(0x80 | opcode);
    let mask_bit = if mask { 0x80u8 } else { 0 };
    if payload.len() < 126 {
        head.push(mask_bit | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        head.push(mask_bit | 126);
        head.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        head.push(mask_bit | 127);
        head.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    if mask {
        let key = mask_key();
        head.extend_from_slice(&key);
        let mut body = payload.to_vec();
        for (index, byte) in body.iter_mut().enumerate() {
            *byte ^= key[index % 4];
        }
        stream.write_all(&head).map_err(|e| e.to_string())?;
        stream.write_all(&body).map_err(|e| e.to_string())?;
    } else {
        stream.write_all(&head).map_err(|e| e.to_string())?;
        stream.write_all(payload).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())
}

fn read_exact(stream: &mut TcpStream, buffer: &mut [u8]) -> Result<(), String> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return Err("connection closed".to_string()),
            Ok(n) => filled += n,
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn read_text(stream: &mut TcpStream, from_client: bool) -> Result<serde_json::Value, String> {
    let frame = read_frame(stream, from_client)?;
    if frame.opcode != 0x1 {
        return Err(format!(
            "expected a text frame, received opcode {}",
            frame.opcode
        ));
    }
    serde_json::from_slice(&frame.payload).map_err(|e| e.to_string())
}

fn client_handshake(stream: &mut TcpStream, port: u16, token: &str) -> Result<(), String> {
    let key = base64(&entropy_bytes(16));
    write!(
        stream,
        "GET /ws?token={token} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(0) => return Err("the link closed during the handshake".to_string()),
            Ok(_) => head.push(byte[0]),
            Err(e) => return Err(e.to_string()),
        }
        if head.len() > 16 * 1024 {
            return Err("handshake head too large".to_string());
        }
    }
    let text = String::from_utf8_lossy(&head);
    let status = text.lines().next().unwrap_or("");
    if !status.contains(" 101 ") {
        return Err(format!("the link refused the handshake: {status}"));
    }
    // Drive the first server frame (hello) out of the way.
    let hello = read_text(stream, false)?;
    if hello.get("type").and_then(|kind| kind.as_str()) != Some("hello") {
        return Err(format!("the link did not greet: {hello}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The project's lock file: how `build --push` finds a running link.

fn write_lock(dir: &Path, port: u16, token: &str) -> Result<(), String> {
    let path = dir.join(LOCK_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let body = serde_json::json!({ "port": port, "token": token }).to_string();
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_lock(dir: &Path) -> Result<(u16, String), String> {
    let path = dir.join(LOCK_FILE);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        format!(
            "no editor link is running here; start `litz connect-editor` first ({} missing)",
            path.display()
        )
    })?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let port = value
        .get("port")
        .and_then(|port| port.as_u64())
        .ok_or_else(|| format!("{}: no port", path.display()))?;
    let token = value
        .get("token")
        .and_then(|token| token.as_str())
        .ok_or_else(|| format!("{}: no token", path.display()))?;
    Ok((port as u16, token.to_string()))
}

// ---------------------------------------------------------------------------
// Requests the page cannot make itself.

/// A request the page hands to the link because the far side refuses browsers
/// (no CORS, or a header a page may not set) — a bucket's control API, for one.
struct Forward {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
}

impl Forward {
    fn from_message(message: &serde_json::Value) -> Option<Self> {
        let method = message.get("method")?.as_str()?.trim().to_ascii_uppercase();
        let url = message.get("url")?.as_str()?.trim().to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return None;
        }
        let headers = message
            .get("headers")
            .and_then(|value| value.as_object())
            .map(|map| {
                map.iter()
                    .filter_map(|(name, value)| Some((name.clone(), value.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let body = message.get("body").and_then(|value| value.as_str());
        if body.is_some_and(|body| body.len() > MAX_FORWARD) {
            return None;
        }
        Some(Self {
            method,
            url,
            headers,
            body: body.map(str::to_string),
        })
    }
}

/// Put the page's headers on a request builder; the body-wanted flag rides on
/// the builder's type and is carried through unchanged.
fn carrying<B>(
    mut call: ureq::RequestBuilder<B>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<B> {
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call
}

/// Carry out one forwarded request: the answer carries the same id back so the
/// page can settle the promise it made when it asked. Non-2xx replies are
/// answers, not failures — the caller wants the body the host sent.
fn forward(id: Option<u64>, request: &Forward) -> serde_json::Value {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(FORWARD_TIMEOUT))
            .max_redirects(8)
            .build(),
    );
    // The builder's type records whether a body is wanted, so the methods that
    // never take one and the ones that do are two arms rather than one.
    let sent = match request.method.as_str() {
        "GET" | "HEAD" | "DELETE" => {
            let call = match request.method.as_str() {
                "HEAD" => agent.head(&request.url),
                "GET" => agent.get(&request.url),
                _ => agent.delete(&request.url),
            };
            carrying(call, &request.headers).call()
        }
        "POST" | "PUT" | "PATCH" => {
            let call = match request.method.as_str() {
                "POST" => agent.post(&request.url),
                "PUT" => agent.put(&request.url),
                _ => agent.patch(&request.url),
            };
            let body = request.body.as_deref().unwrap_or("");
            carrying(call, &request.headers).send(body)
        }
        other => {
            return serde_json::json!({
                "type": "http.failed",
                "id": id,
                "error": format!("the link does not forward {other}"),
            });
        }
    };
    let mut response = match sent {
        Ok(response) => response,
        Err(failure) => {
            return serde_json::json!({
                "type": "http.failed",
                "id": id,
                "error": failure.to_string(),
            });
        }
    };
    let status = response.status().as_u16();
    let headers: serde_json::Map<String, serde_json::Value> = response
        .headers()
        .iter()
        .map(|(name, value)| {
            let value = String::from_utf8_lossy(value.as_bytes()).to_string();
            (name.as_str().to_string(), serde_json::Value::String(value))
        })
        .collect();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_REPLY)
        .read_to_vec()
        .unwrap_or_default();
    // JSON and form answers are text; an image or a downloaded file is not, and
    // base64 is the only shape a JSON message can carry it in.
    match String::from_utf8(bytes.clone()) {
        Ok(text) => serde_json::json!({
            "type": "http.replied",
            "id": id,
            "status": status,
            "headers": headers,
            "body": text,
        }),
        Err(_) => serde_json::json!({
            "type": "http.replied",
            "id": id,
            "status": status,
            "headers": headers,
            "body": base64(&bytes),
            "encoding": "base64",
        }),
    }
}

// ---------------------------------------------------------------------------
// Small primitives, kept here so the link adds no dependency.

/// A code the user supplies: printable, no separators that would end the URL
/// fragment or the `Sec-WebSocket-Protocol` list it travels in.
fn code_of(text: &str) -> Option<String> {
    let text = text.trim();
    let shaped = (4..=64).contains(&text.len())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'));
    shaped.then(|| text.to_string())
}

fn token() -> String {
    let mut text = String::new();
    for salt in 0..4u8 {
        if salt > 0 {
            text.push('-');
        }
        text.push_str(&format!("{:04X}", (entropy(salt) & 0xFFFF) as u16));
    }
    text
}

/// A value seeded from the OS through `RandomState`, mixed with the clock and
/// the process id. `RandomState`'s keys come from the platform's entropy, and
/// the salt keeps the four groups of a token from being slices of one value.
fn entropy(salt: u8) -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u8(salt);
    hasher.write_u32(std::process::id());
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0),
    );
    hasher.finish()
}

fn entropy_bytes(length: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(length);
    while bytes.len() < length {
        bytes.extend_from_slice(&entropy(bytes.len() as u8).to_be_bytes());
    }
    bytes.truncate(length);
    bytes
}

fn mask_key() -> [u8; 4] {
    let value = entropy(0xFF).to_be_bytes();
    [value[0], value[1], value[2], value[3]]
}

fn same(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut state: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut message = data.to_vec();
    let bits = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());

    for block in message.as_chunks::<64>().0 {
        let mut words = [0u32; 80];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            words[index] = u32::from_be_bytes(*word);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) =
            (state[0], state[1], state[2], state[3], state[4]);
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | (!b & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut digest = [0u8; 20];
    for (index, word) in state.iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        text.push(ALPHABET[(a >> 2) as usize] as char);
        text.push(ALPHABET[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            text.push(ALPHABET[(((b & 0x0F) << 2) | (c >> 6)) as usize] as char);
        } else {
            text.push('=');
        }
        if chunk.len() > 2 {
            text.push(ALPHABET[(c & 0x3F) as usize] as char);
        } else {
            text.push('=');
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn scratch(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("litz-link-{}-{n}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sha1_matches_the_rfc_vectors() {
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
            "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
        );
    }

    #[test]
    fn the_accept_key_matches_the_rfc_example() {
        let accept = base64(&sha1(
            b"dGhlIHNhbXBsZSBub25jZQ==258EAFA5-E914-47DA-95CA-C5AB0DC85B11",
        ));
        assert_eq!(accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn a_token_is_four_groups_of_hex() {
        let first = token();
        assert_eq!(first.len(), 19, "{first}");
        let groups: Vec<&str> = first.split('-').collect();
        assert_eq!(groups.len(), 4);
        for group in groups {
            assert_eq!(group.len(), 4);
            assert!(group.chars().all(|c| c.is_ascii_hexdigit()), "{first}");
        }
        assert_ne!(first, token());
    }

    #[test]
    fn frames_round_trip_a_masked_client_frame() {
        let dir = scratch("frames");
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut server, _) = listener.accept().unwrap();
            read_frame(&mut server, true).unwrap()
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write_frame(&mut client, 0x1, b"{\"type\":\"ping\"}", true).unwrap();
        let frame = handle.join().unwrap();
        assert_eq!(frame.opcode, 0x1);
        assert_eq!(frame.payload, b"{\"type\":\"ping\"}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_given_code_is_held_to_what_a_fragment_and_a_header_can_carry() {
        assert_eq!(code_of(" FIELD-TEST "), Some("FIELD-TEST".into()));
        assert_eq!(code_of("abcd"), Some("abcd".into()));
        assert!(code_of("has space").is_none());
        assert!(code_of("hash#tag").is_none());
        assert!(code_of("slash/end").is_none());
        assert!(code_of("abc").is_none());
        assert!(code_of("x".repeat(65).as_str()).is_none());
    }

    #[test]
    fn a_restart_keeps_the_code_a_bookmark_names() {
        let dir = scratch("keep-code");
        // The port a dead link used; nothing listens there any more.
        let dead = {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            port
        };
        write_lock(&dir, dead, "AAAA-BBBB-CCCC-DDDD").unwrap();
        let options = Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: Vec::new(),
            assets: None,
            token: None,
        };
        let (_listener, token) = listen(&options).unwrap();
        assert_eq!(token, "AAAA-BBBB-CCCC-DDDD");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_flag_sets_the_code_and_the_lock_file_records_it() {
        let dir = scratch("given-code");
        let options = Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: Vec::new(),
            assets: None,
            token: Some("FIELD-TEST-0001".into()),
        };
        let (_listener, token) = listen(&options).unwrap();
        assert_eq!(token, "FIELD-TEST-0001");
        assert_eq!(read_lock(&dir).unwrap().1, "FIELD-TEST-0001");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_forwarded_request_carries_its_method_headers_and_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nX-Probe: yes\r\nContent-Length: 7\r\nConnection: close\r\n\r\ncreated",
                )
                .unwrap();
            String::from_utf8_lossy(&request).to_string()
        });

        let request = Forward {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/accounts/x/r2/buckets"),
            headers: vec![("authorization".into(), "Bearer secret".into())],
            body: Some("{\"name\":\"litz\"}".into()),
        };
        let answer = forward(Some(7), &request);
        assert_eq!(answer["type"], "http.replied");
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["status"], 201);
        assert_eq!(answer["headers"]["x-probe"], "yes");
        assert_eq!(answer["body"], "created");

        let sent = seen.join().unwrap().to_lowercase();
        assert!(
            sent.starts_with("post /accounts/x/r2/buckets http/1.1"),
            "{sent}"
        );
        assert!(sent.contains("authorization: bearer secret"), "{sent}");
        assert!(sent.contains("{\"name\":\"litz\"}"), "{sent}");
    }

    #[test]
    fn a_lock_file_round_trips() {
        let dir = scratch("lock");
        write_lock(&dir, 4317, "ABCD-EF01-2345-6789").unwrap();
        let (port, token) = read_lock(&dir).unwrap();
        assert_eq!(port, 4317);
        assert_eq!(token, "ABCD-EF01-2345-6789");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_build_streams_output_and_publishes_the_image() {
        let dir = scratch("build");
        std::fs::write(
            dir.join("litz.build.toml"),
            "[build]\nfirmware = \"out/app.bin\"\n\n[[step]]\nrun = \"mkdir -p out && printf hello > out/app.bin\"\n",
        )
        .unwrap();

        let options = Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: Vec::new(),
            assets: None,
            token: None,
        };
        let (listener, token) = listen(&options).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(state(&options, token.clone()));
        let running = state.clone();
        std::thread::spawn(move || accept_loop(listener, running));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        client_handshake(&mut stream, port, &token).unwrap();
        write_frame(&mut stream, 0x1, br#"{"type":"build"}"#, true).unwrap();

        let mut artifact = None;
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            let message = read_text(&mut stream, false).unwrap();
            match message.get("type").and_then(|kind| kind.as_str()) {
                Some("artifact") => {
                    artifact = Some(message);
                    break;
                }
                Some("build.failed") => panic!("build failed: {message}"),
                _ => {}
            }
        }
        let artifact = artifact.expect("the build published no image");
        let sha = artifact
            .get("sha256")
            .and_then(|value| value.as_str())
            .unwrap();
        assert_eq!(artifact["size"], 5);
        let (_, _, bytes) = state.artifact(sha).expect("the image is kept");
        assert_eq!(&bytes[..], b"hello");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_library_keeps_one_file_per_project() {
        let dir = scratch("library");
        let path = project_path(&dir, " Blinky ").unwrap();
        assert_eq!(
            path,
            dir.join(".litz")
                .join("projects")
                .join("Blinky.litzproject.json")
        );
        let body = r#"{"format":"litz-project"}"#;
        write_project(&path, body).unwrap();
        assert_eq!(read_project(&path).unwrap(), body);
        // The staging file is renamed away, so a save leaves one file.
        assert!(!path.with_extension("json.tmp").exists());

        let items = list_projects(&dir);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["key"], "Blinky");
        assert_eq!(items[0]["size"].as_u64(), Some(body.len() as u64));
        assert!(items[0]["modified"].as_u64().unwrap() > 0);

        std::fs::write(dir.join(".litz/projects/notes.txt"), "kept").unwrap();
        assert_eq!(
            list_projects(&dir).len(),
            1,
            "only project files are listed"
        );

        std::fs::remove_file(&path).unwrap();
        assert!(list_projects(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_project_name_cannot_address_another_directory() {
        for name in ["", "  ", "../escape", "a/b", "a\\b", ".hidden", "x\ny"] {
            assert!(
                project_path(Path::new("."), name).is_none(),
                "{name:?} was accepted"
            );
        }
        assert!(project_name(&"long".repeat(31)).is_none());
        assert_eq!(project_name("Blinky 1"), Some("Blinky 1"));
    }

    #[test]
    fn the_code_can_arrive_in_a_header_instead_of_the_query() {
        let dir = scratch("header-token");
        let (port, token) = start(Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: Vec::new(),
            assets: None,
            token: None,
        });
        assert!(request(port, "/artifact/none", &[]).contains(" 403 "));
        let bearer = format!("Bearer {token}");
        assert!(
            request(port, "/artifact/none", &[("Authorization", &bearer)]).contains(" 404 "),
            "a header-carried code is the same credential"
        );
        assert!(
            request(
                port,
                "/artifact/none",
                &[("Authorization", "Bearer 0000-0000-0000-0000")]
            )
            .contains(" 403 ")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_origin_list_keeps_other_pages_out() {
        let dir = scratch("origins");
        let (port, _) = start(Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: vec!["https://litz.example.com".into()],
            assets: None,
            token: None,
        });
        assert!(
            request(port, "/health", &[("Origin", "https://elsewhere.example")]).contains(" 403 ")
        );
        assert!(
            request(port, "/health", &[("Origin", "https://litz.example.com")]).contains(" 200 ")
        );
        // A client with no Origin is still welcome: curl and `--push` have none.
        assert!(request(port, "/health", &[]).contains(" 200 "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_editor_build_is_served_beside_the_link() {
        let dir = scratch("assets");
        let assets = dir.join("site");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(assets.join("app.js"), "export {}").unwrap();
        let (port, _) = start(Options {
            directory: dir.clone(),
            port: 0,
            host: "127.0.0.1".into(),
            origins: Vec::new(),
            assets: Some(assets),
            token: None,
        });
        assert!(request(port, "/", &[]).contains(" 200 "));
        assert!(request(port, "/app.js", &[]).contains(" 200 "));
        // A client route has no file behind it; the page answers it.
        assert!(request(port, "/editor", &[]).contains(" 200 "));
        assert!(request(port, "/../Cargo.toml", &[]).contains(" 403 "));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Run a link on a system-chosen port and hand back what it bound.
    fn start(options: Options) -> (u16, String) {
        let dir = options.directory.clone();
        std::thread::spawn(move || {
            let _ = serve(&options);
        });
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if let Ok((port, token)) = read_lock(&dir)
                && TcpStream::connect(("127.0.0.1", port)).is_ok()
            {
                return (port, token);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the link did not claim its lock file");
    }

    fn request(port: u16, target: &str, headers: &[(&str, &str)]) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut head =
            format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        body.lines().next().unwrap_or("").to_string()
    }
}

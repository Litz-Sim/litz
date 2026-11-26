//! `litz connect-editor`: the link between a build and the editor page.
//!
//! The editor cannot run a firmware toolchain, and `litz build` cannot draw a
//! circuit. This module is the smallest bridge between the two: one process,
//! one loopback port, a pairing token, WebSocket text messages for commands
//! and build output, and an in-memory artifact store the editor fetches over
//! plain HTTP.
//!
//! Nothing leaves the machine. The token is the only way in: a page that was
//! not handed the URL printed by `connect-editor` gets 403 on every route.
//! The socket accepts two commands — build this project's own recipe, and
//! accept bytes a `litz build --push` already produced — and never a command
//! string, so the link is not a shell.

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

struct State {
    dir: PathBuf,
    token: String,
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
pub fn serve(dir: &Path, port: u16) -> Result<(), String> {
    let (listener, token) = listen(dir, port)?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("editor link: {e}"))?
        .port();
    // One line to copy: the page reads the pairing code from the fragment, so
    // the code never reaches the server in a request line or a log.
    println!("litz: editor link ws://127.0.0.1:{port}/ws#{token}");
    println!("litz: paste that into the editor's Connect dialog; Ctrl-C to stop");
    println!("litz: `litz build --push` from another terminal sends an image here");
    accept_loop(listener, Arc::new(state(dir, token)));
    Ok(())
}

/// Bind the loopback listener and claim the project's lock file.
fn listen(dir: &Path, port: u16) -> Result<(TcpListener, String), String> {
    let dir = dir
        .canonicalize()
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Ok((running, _)) = read_lock(&dir)
        && TcpStream::connect(("127.0.0.1", running)).is_ok()
    {
        return Err(format!(
            "an editor link is already running on port {running}; close it first"
        ));
    }
    let listener = TcpListener::bind(("127.0.0.1", port))
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let actual = listener
        .local_addr()
        .map_err(|e| format!("editor link: {e}"))?
        .port();
    let token = token();
    write_lock(&dir, actual, &token)?;
    Ok((listener, token))
}

fn state(dir: &Path, token: String) -> State {
    State {
        dir: dir.to_path_buf(),
        token,
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
    let (path, query) = split_target(&request.target);
    let provided = query.get("token").map(String::as_str).unwrap_or("");
    let authorized = same(&state.token, provided);

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
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
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
        _ => {
            let _ = respond(&mut stream, 404, "text/plain", b"not found", origin, &[]);
        }
    }
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
                match message.get("type").and_then(|kind| kind.as_str()) {
                    Some("ping") => {
                        let _ = out.send(Message::Text(r#"{"type":"pong"}"#.to_string()));
                    }
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

fn error(message: &str) -> String {
    serde_json::json!({ "type": "error", "message": message }).to_string()
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
// Small primitives, kept here so the link adds no dependency.

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

    for block in message.chunks_exact(64) {
        let mut words = [0u32; 80];
        for (index, word) in block.chunks_exact(4).enumerate() {
            words[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
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

        let (listener, token) = listen(&dir, 0).unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(state(&dir, token.clone()));
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
}

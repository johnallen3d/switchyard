//! Where switchyard-server listens and how a client asks it, so the server
//! and the pickers agree on both. The server (crates/server) keeps every
//! Rex server's rows and the task registry up to date from events; a client
//! asks it rather than fanning out to every host itself, and falls back to
//! the direct path when nothing answers.
//!
//! On the control-plane host, the protocol is newline-delimited JSON over a
//! Unix socket (`socket_path()`). On client hosts with `[control_plane] url`
//! configured, requests go over HTTP to that server (`GET /api/snapshot`,
//! `GET /api/subscribe`, and `POST /api/rpc`), keeping the single task registry
//! and control plane on one always-on host.
//!
//! `pastes` turns a snapshot into the tab picker's data pastes (see the
//! tab-picker's standalone.rs), rewriting `"local"` / `"-"` host markers when
//! viewing snapshots from a remote control plane.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::links::Unlinked;
use crate::rex::{local_label, runtime_dir};

/// Set by `switchyard-server` itself so it never forwards RPCs back out to a
/// remote `control_plane.url`, even if misconfigured.
static LOCAL_SERVER_MODE: AtomicBool = AtomicBool::new(false);

/// Mark this process as the local `switchyard-server`, disabling remote
/// control-plane forwarding.
pub fn set_local_server_mode() {
    LOCAL_SERVER_MODE.store(true, Ordering::Relaxed);
}

/// The remote control-plane base URL for this process, or `None` when running
/// on the control-plane host or inside `switchyard-server`.
#[must_use]
pub fn remote_url() -> Option<String> {
    if LOCAL_SERVER_MODE.load(Ordering::Relaxed) {
        return None;
    }
    crate::config::remote_control_url()
}

/// The server's socket, in this user's runtime directory.
#[must_use]
pub fn socket_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("server.sock"))
}

/// How long a client waits for a local memory-served reply before falling back.
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// TCP connect timeout to a remote control plane over the tailnet.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Fast remote read/cache timeout (`snapshot`, `folders`, `probes`, `refresh`).
const FAST_HTTP_TIMEOUT: Duration = Duration::from_secs(5);
/// Long remote timeout for actions that may SSH, clone, bundle, or sync Atuin.
const ACTION_HTTP_TIMEOUT: Duration = Duration::from_secs(180);
/// Idle read timeout on a remote `/api/subscribe` stream (server sends a
/// keepalive blank line every 30 s, so 90 s means the connection went dead,
/// e.g. across laptop sleep).
const SUBSCRIBE_HTTP_TIMEOUT: Duration = Duration::from_secs(90);
/// Maximum HTTP response headers size we accept from the control plane.
const MAX_HTTP_HEADERS: usize = 64 * 1024;

fn send(cmd: &str) -> Result<BufReader<UnixStream>, String> {
    request(&json!({ "cmd": cmd }))
}

fn request(req: &Value) -> Result<BufReader<UnixStream>, String> {
    let path = socket_path().ok_or("no runtime directory for the server's socket")?;
    let mut stream = UnixStream::connect(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(REPLY_TIMEOUT))
        .map_err(|e| e.to_string())?;
    writeln!(stream, "{req}").map_err(|e| format!("switchyard-server: {e}"))?;
    Ok(BufReader::new(stream))
}

fn read_reply(reader: &mut impl BufRead) -> Result<Value, String> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Err("switchyard-server hung up".into()),
        Ok(_) => {
            let reply: Value =
                serde_json::from_str(&line).map_err(|e| format!("switchyard-server: {e}"))?;
            match reply["error"].as_str() {
                Some(e) => Err(format!("switchyard-server: {e}")),
                None => Ok(reply),
            }
        }
        Err(e) => Err(format!("switchyard-server: {e}")),
    }
}

/// Parse an `http://host[:port][/prefix]` base URL into `(host, port, host_header, base_path)`.
fn parse_http_url(url: &str) -> Result<(String, u16, String, String), String> {
    let rest = url
        .trim()
        .strip_prefix("http://")
        .ok_or_else(|| format!("control_plane.url must start with http://: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
        None => (rest, ""),
    };
    if authority.is_empty() {
        return Err(format!("control_plane.url has no host: {url}"));
    }
    let (host, port) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let (addr, tail) = after_bracket
            .split_once(']')
            .ok_or_else(|| format!("invalid IPv6 host in {url}"))?;
        let port = match tail {
            "" => 80,
            s => s
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| format!("invalid port in {url}"))?,
        };
        (addr.to_string(), port)
    } else if let Some((h, p)) = authority.rsplit_once(':') {
        let port = p
            .parse::<u16>()
            .map_err(|_| format!("invalid port in {url}"))?;
        (h.to_string(), port)
    } else {
        (authority.to_string(), 80)
    };
    if host.is_empty() {
        return Err(format!("control_plane.url has no host: {url}"));
    }
    Ok((host, port, authority.to_string(), path.to_string()))
}

fn connect_http(url: &str, timeout: Duration) -> Result<(TcpStream, String, String), String> {
    let (host, port, host_header, base_path) = parse_http_url(url)?;
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| format!("can't resolve {host}:{port}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("no address for {host}:{port}"));
    }
    let mut last_err = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(timeout));
                let _ = stream.set_write_timeout(Some(FAST_HTTP_TIMEOUT));
                return Ok((stream, host_header, base_path));
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(format!(
        "can't reach control plane {host}:{port}: {}",
        last_err.map_or_else(|| "connection failed".into(), |e| e.to_string())
    ))
}

/// Read HTTP response status code and optional `Content-Length`, leaving
/// `reader` positioned at the first byte of the response body.
fn read_http_head(reader: &mut impl BufRead) -> Result<(u16, Option<usize>), String> {
    let mut status_line = String::new();
    if reader
        .read_line(&mut status_line)
        .map_err(|e| format!("control plane read failed: {e}"))?
        == 0
    {
        return Err("control plane closed the connection".into());
    }
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| {
            format!(
                "invalid HTTP status from control plane: {}",
                status_line.trim()
            )
        })?;
    let mut length = None;
    let mut total = status_line.len();
    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .map_err(|e| format!("control plane read failed: {e}"))?
            == 0
        {
            return Err("control plane closed headers early".into());
        }
        total += line.len();
        if total > MAX_HTTP_HEADERS {
            return Err("control plane response headers too large".into());
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, val)) = trimmed.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
            && let Ok(n) = val.trim().parse::<usize>()
        {
            length = Some(n);
        }
    }
    Ok((status, length))
}

fn http_json(
    url: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> Result<Value, String> {
    let (mut stream, host_header, base_path) = connect_http(url, timeout)?;
    let payload = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "{method} {base_path}{path} HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n"
    );
    if body.is_some() {
        let _ = write!(
            head,
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        );
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.write_all(payload.as_bytes()))
        .and_then(|()| stream.flush())
        .map_err(|e| format!("control plane write failed: {e}"))?;
    let mut reader = BufReader::new(stream);
    let (status, length) = read_http_head(&mut reader)?;
    let mut buf = Vec::new();
    if let Some(len) = length {
        buf.resize(len, 0);
        reader
            .read_exact(&mut buf)
            .map_err(|e| format!("control plane read failed: {e}"))?;
    } else {
        reader
            .read_to_end(&mut buf)
            .map_err(|e| format!("control plane read failed: {e}"))?;
    }
    let parsed: Result<Value, _> = serde_json::from_slice(&buf);
    if !(200..300).contains(&status) {
        if let Ok(v) = parsed
            && let Some(e) = v["error"].as_str()
        {
            return Err(e.to_string());
        }
        return Err(format!("control plane returned HTTP {status}"));
    }
    parsed.map_err(|e| format!("invalid JSON from control plane: {e}"))
}

/// Call `POST /api/rpc` at `url` with `body` and unwrap `error` if present.
///
/// # Errors
///
/// Fails when the control plane can't be reached, returns a non-2xx HTTP
/// status, or replies with an `error` field.
pub fn rpc_at(url: &str, body: &Value, timeout: Duration) -> Result<Value, String> {
    let reply = http_json(url, "POST", "/api/rpc", Some(body), timeout)?;
    if let Some(e) = reply["error"].as_str() {
        return Err(e.to_string());
    }
    Ok(reply)
}

/// Fetch `GET /api/snapshot` from `url`.
///
/// # Errors
///
/// Fails when the control plane can't be reached or returns invalid JSON.
pub fn snapshot_at(url: &str) -> Result<Value, String> {
    let reply = http_json(url, "GET", "/api/snapshot", None, FAST_HTTP_TIMEOUT)?;
    if let Some(e) = reply["error"].as_str() {
        return Err(format!("switchyard-server: {e}"));
    }
    Ok(reply)
}

/// Open a streaming `GET /api/subscribe` subscription at `url`.
///
/// # Errors
///
/// Fails when the control plane can't be reached, rejects the stream, or
/// fails to send the initial snapshot.
pub fn subscribe_at(url: &str) -> Result<Subscription, String> {
    let (mut stream, host_header, base_path) = connect_http(url, FAST_HTTP_TIMEOUT)?;
    let head = format!(
        "GET {base_path}/api/subscribe HTTP/1.1\r\nHost: {host_header}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(|e| format!("control plane write failed: {e}"))?;
    let mut reader = BufReader::new(stream);
    let (status, _) = read_http_head(&mut reader)?;
    if status != 200 {
        return Err(format!("control plane subscribe returned HTTP {status}"));
    }
    let first = read_http_stream_line(&mut reader)?;
    let _ = reader
        .get_ref()
        .set_read_timeout(Some(SUBSCRIBE_HTTP_TIMEOUT));
    Ok(Subscription {
        first: Some(first),
        reader: Some(SubReader::Http(reader)),
    })
}

fn read_http_stream_line(reader: &mut BufReader<TcpStream>) -> Result<Value, String> {
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return Err("switchyard-server hung up".into()),
            Ok(_) => {
                // Blank lines are periodic keepalives.
                if line.trim().is_empty() {
                    continue;
                }
                let reply: Value =
                    serde_json::from_str(&line).map_err(|e| format!("switchyard-server: {e}"))?;
                if let Some(e) = reply["error"].as_str() {
                    return Err(format!("switchyard-server: {e}"));
                }
                return Ok(reply);
            }
            Err(e) => return Err(format!("switchyard-server: {e}")),
        }
    }
}

/// The server's current snapshot, or why it couldn't be had (most often:
/// no server running).
///
/// # Errors
///
/// Fails when no server answers or its reply is an error or unreadable.
pub fn snapshot() -> Result<Value, String> {
    if let Some(url) = remote_url() {
        return snapshot_at(&url);
    }
    read_reply(&mut send("snapshot")?)
}

/// `host`'s zoxide folders from the server's cache, one per line, or why
/// not (no server, or it has no good listing for `host`).
///
/// # Errors
///
/// Fails when no server answers or it has no listing for `host`.
pub fn folders(host: &str) -> Result<String, String> {
    let req = json!({ "cmd": "folders", "host": host });
    let reply = if let Some(url) = remote_url() {
        rpc_at(&url, &req, FAST_HTTP_TIMEOUT).map_err(|e| format!("switchyard-server: {e}"))?
    } else {
        read_reply(&mut request(&req)?)?
    };
    reply["folders"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "switchyard-server: no folders".into())
}

/// The server's cached host probes for `project` (see the module docs), or
/// why not.
///
/// # Errors
///
/// Fails when no server answers or it has no probes for `project`.
pub fn probes(project: &str) -> Result<Vec<Value>, String> {
    let req = json!({ "cmd": "probes", "project": project });
    let reply = if let Some(url) = remote_url() {
        rpc_at(&url, &req, FAST_HTTP_TIMEOUT).map_err(|e| format!("switchyard-server: {e}"))?
    } else {
        read_reply(&mut request(&req)?)?
    };
    reply["probes"]
        .as_array()
        .cloned()
        .ok_or_else(|| "switchyard-server: no probes".into())
}

/// Ask the server to check the task registry now, after an action changed
/// it. Quietly does nothing without a server: the registry is read fresh
/// on the direct path anyway.
pub fn refresh() {
    if let Some(url) = remote_url() {
        let _ = rpc_at(&url, &json!({ "cmd": "refresh" }), FAST_HTTP_TIMEOUT);
        return;
    }
    if let Ok(mut reader) = send("refresh") {
        let _ = read_reply(&mut reader);
    }
}

/// Snapshots as they change, starting with the current one. Iteration ends
/// at the first error, such as the server stopping.
///
/// # Errors
///
/// Fails when no server answers or its first snapshot can't be read.
pub fn subscribe() -> Result<Subscription, String> {
    if let Some(url) = remote_url() {
        return subscribe_at(&url);
    }
    let mut reader = send("subscribe")?;
    // The first reply comes at once, so it gets the usual timeout ...
    let first = read_reply(&mut reader)?;
    // ... but updates come only on changes, and a quiet minute is normal.
    reader
        .get_ref()
        .set_read_timeout(None)
        .map_err(|e| e.to_string())?;
    Ok(Subscription {
        first: Some(first),
        reader: Some(SubReader::Unix(reader)),
    })
}

enum SubReader {
    Unix(BufReader<UnixStream>),
    Http(BufReader<TcpStream>),
}

pub struct Subscription {
    first: Option<Value>,
    reader: Option<SubReader>,
}

impl Iterator for Subscription {
    type Item = Result<Value, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(first) = self.first.take() {
            return Some(Ok(first));
        }
        let reply = match self.reader.as_mut()? {
            SubReader::Unix(r) => read_reply(r),
            SubReader::Http(r) => read_http_stream_line(r),
        };
        if reply.is_err() {
            self.reader = None;
        }
        Some(reply)
    }
}

fn remote_rpc(body: &Value) -> Result<Value, String> {
    let url = remote_url().ok_or("no remote control plane configured")?;
    rpc_at(&url, body, ACTION_HTTP_TIMEOUT)
}

fn reply_string(reply: &Value, key: &str) -> Result<String, String> {
    reply[key]
        .as_str()
        .map(String::from)
        .ok_or_else(|| format!("control plane reply missing {key}"))
}

/// Fetch reconciled or live tasks from the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or returns an error.
pub fn remote_tasks(live: bool) -> Result<Vec<Value>, String> {
    let reply = remote_rpc(&json!({ "cmd": "tasks", "live": live }))?;
    reply["tasks"]
        .as_array()
        .cloned()
        .ok_or_else(|| "control plane reply missing tasks".into())
}

/// Open a session for `dir` on `host` (or `auto`) through the remote control
/// plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or `open_on` fails.
pub fn remote_open(host: &str, dir: &str) -> Result<(String, String), String> {
    let reply = remote_rpc(&json!({ "cmd": "open", "host": host, "dir": dir }))?;
    Ok((
        reply_string(&reply, "host")?,
        reply_string(&reply, "session")?,
    ))
}

/// Parameters for launching a task through the remote control plane.
pub struct LaunchRequest<'a> {
    pub host: &'a str,
    pub path: &'a str,
    pub name: &'a str,
    pub harness: &'a str,
    pub prompt: Option<&'a str>,
    pub prompt_ref: Option<&'a str>,
    pub from_host: Option<&'a str>,
    pub work_item: Option<&'a str>,
}

/// Launch a task through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or launch fails.
pub fn remote_launch(req: &LaunchRequest<'_>) -> Result<(String, String, String), String> {
    let reply = remote_rpc(&json!({
        "cmd": "launch",
        "host": req.host,
        "path": req.path,
        "name": req.name,
        "harness": req.harness,
        "prompt": req.prompt,
        "prompt_ref": req.prompt_ref,
        "from_host": req.from_host,
        "work_item": req.work_item,
    }))?;
    Ok((
        reply_string(&reply, "host")?,
        reply_string(&reply, "session")?,
        reply_string(&reply, "window")?,
    ))
}

/// Run `actions::act` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or the action fails.
pub fn remote_act(action: &str, host: &str, session: &str, window: &str) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "act",
        "action": action,
        "host": host,
        "session": session,
        "window": window,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::archive_family` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or archiving fails.
pub fn remote_archive_family(
    action: &str,
    host: &str,
    session: &str,
    window: &str,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "archive_family",
        "action": action,
        "host": host,
        "session": session,
        "window": window,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::snooze` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or snoozing fails.
pub fn remote_snooze(window: &str, until: u64) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "snooze",
        "window": window,
        "until": until,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::message` (with optional source host for pasted uploads)
/// through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or messaging fails.
pub fn remote_message(
    host: &str,
    session: &str,
    window: &str,
    text: &str,
    from: Option<&str>,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "message",
        "host": host,
        "session": session,
        "window": window,
        "text": text,
        "from_host": from,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::message_ref` through the remote control plane (when the prompt
/// file lives on another host), carrying the caller host (`from`) so pasted
/// local image paths on the client machine can still be copied to the agent.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or messaging fails.
pub fn remote_message_ref(
    host: &str,
    session: &str,
    window: &str,
    prompt_ref: &str,
    from: Option<&str>,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "message",
        "host": host,
        "session": session,
        "window": window,
        "prompt_ref": prompt_ref,
        "from_host": from,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::interrupt` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or interrupting fails.
pub fn remote_interrupt(host: &str, session: &str, window: &str) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "interrupt",
        "host": host,
        "session": session,
        "window": window,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::answer_question` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or answering fails.
pub fn remote_answer(
    host: &str,
    session: &str,
    window: &str,
    response: &crate::actions::QuestionResponse,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "answer",
        "host": host,
        "session": session,
        "window": window,
        "response": response,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::switch_harness` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or switching fails.
pub fn remote_switch(
    host: &str,
    session: &str,
    window: &str,
    target: &str,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "switch",
        "host": host,
        "session": session,
        "window": window,
        "harness": target,
    }))?;
    reply_string(&reply, "result")
}

/// Run `actions::move_host` through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or moving fails.
pub fn remote_move(
    host: &str,
    session: &str,
    window: &str,
    target: &str,
) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "move",
        "host": host,
        "session": session,
        "window": window,
        "target": target,
    }))?;
    reply_string(&reply, "result")
}

/// Remove `refs` from open tasks for `worktree` on `host` through the remote
/// control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or unlinking fails.
pub fn remote_unlink(host: &str, worktree: &str, refs: &[&str]) -> Result<Vec<Unlinked>, String> {
    let reply = remote_rpc(&json!({
        "cmd": "unlink",
        "host": host,
        "worktree": worktree,
        "refs": refs,
    }))?;
    let outcomes = reply["outcomes"]
        .as_array()
        .ok_or("control plane reply missing outcomes")?;
    Ok(outcomes
        .iter()
        .map(|v| match v.as_str() {
            Some("removed") => Unlinked::Removed,
            Some("derived") => Unlinked::Derived,
            _ => Unlinked::Absent,
        })
        .collect())
}

/// Fetch a tab's transcript through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached.
pub fn remote_transcript(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let reply = remote_rpc(&json!({
        "cmd": "transcript",
        "host": host,
        "session": session,
        "window": window,
    }))?;
    Ok(reply["result"].clone())
}

/// Fetch a tab's git changes through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached.
pub fn remote_changes(host: &str, session: &str, window: &str) -> Result<Value, String> {
    let reply = remote_rpc(&json!({
        "cmd": "changes",
        "host": host,
        "session": session,
        "window": window,
    }))?;
    Ok(reply["result"].clone())
}

/// Resolve a tab's checkout directory through the remote control plane.
///
/// # Errors
///
/// Fails when the remote control plane can't be reached or the directory can't
/// be found.
pub fn remote_task_dir(host: &str, session: &str, window: &str) -> Result<String, String> {
    let reply = remote_rpc(&json!({
        "cmd": "task_dir",
        "host": host,
        "session": session,
        "window": window,
    }))?;
    reply_string(&reply, "dir")
}

/// Hex-encode a host label as `host_rows.lua` does.
fn hex_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Rewrite a host's `S` header lines when viewing a snapshot produced by a
/// remote control plane (`server_host` != `viewer_local`).
fn rewrite_rows(rows: &str, host: &str, server_host: &str, viewer_local: &str) -> String {
    if host == "local" {
        let replacement = format!("S\t{}\t{server_host}\t", hex_encode(server_host));
        let mut out = String::with_capacity(rows.len());
        for line in rows.split_inclusive('\n') {
            if let Some(rest) = line.strip_prefix("S\t-\tlocal\t") {
                out.push_str(&replacement);
                out.push_str(rest);
            } else {
                out.push_str(line);
            }
        }
        return out;
    }
    if host == viewer_local {
        let prefix = format!("S\t{}\t{viewer_local}\t", hex_encode(viewer_local));
        let mut out = String::with_capacity(rows.len());
        for line in rows.split_inclusive('\n') {
            if let Some(rest) = line.strip_prefix(&prefix) {
                out.push_str("S\t-\tlocal\t");
                out.push_str(rest);
            } else {
                out.push_str(line);
            }
        }
        return out;
    }
    rows.to_string()
}

/// The tab picker's data pastes for a snapshot, as (kind, body) in delivery
/// order, leaving out what the server doesn't know yet.
#[must_use]
pub fn pastes(snap: &Value) -> Vec<(&'static str, String)> {
    pastes_for(snap, &local_label())
}

/// `pastes` viewed from `viewer_local`: when `snap["server_host"]` is set and
/// differs from `viewer_local`, the server's `"local"` host is rewritten to
/// its real label and `viewer_local`'s host (if present) becomes `"local"` /
/// `"-"` and is placed first.
#[must_use]
pub fn pastes_for(snap: &Value, viewer_local: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let hosts = snap["hosts"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let server_host = snap["server_host"]
        .as_str()
        .filter(|s| !s.is_empty() && *s != viewer_local);
    if !hosts.is_empty() {
        let mut ordered: Vec<&Value> = hosts.iter().collect();
        if server_host.is_some()
            && let Some(pos) = ordered
                .iter()
                .position(|h| h["host"].as_str() == Some(viewer_local))
        {
            let local_entry = ordered.remove(pos);
            ordered.insert(0, local_entry);
        }
        let (mut rows, mut failed, mut first_error) = (String::new(), String::new(), None);
        let mut answered = false;
        for h in ordered {
            let raw_name = h["host"].as_str().unwrap_or("?");
            let display_name = match server_host {
                Some(sh) if raw_name == "local" => sh,
                _ => raw_name,
            };
            let raw_rows = h["rows"].as_str().unwrap_or("");
            match server_host {
                Some(sh) => rows.push_str(&rewrite_rows(raw_rows, raw_name, sh, viewer_local)),
                None => rows.push_str(raw_rows),
            }
            answered |= !h["age_ms"].is_null();
            let why = match h["error"].as_str() {
                Some(e) => {
                    first_error.get_or_insert_with(|| format!("{display_name}: {e}"));
                    e
                }
                None if h["age_ms"].is_null() => "not listed yet",
                None => continue,
            };
            failed.push_str(&failed_row(display_name, why));
        }
        match first_error {
            Some(e) if !answered => out.push(("error", e)),
            _ if !answered => {}
            _ => out.push(("rows", rows + &failed)),
        }
    }
    let checked = snap["tasks_checked"].as_bool() == Some(true);
    let error = snap["tasks_error"].as_str();
    match &snap["tasks"] {
        Value::Array(_) => {
            out.push((
                if checked { "tasks" } else { "live" },
                snap["tasks"].to_string(),
            ));
            if let (false, Some(e)) = (checked, error) {
                out.push(("tasks-error", e.to_string()));
            }
        }
        _ => {
            if let Some(e) = error {
                out.push(("tasks-error", e.to_string()));
            }
        }
    }
    out
}

/// The rows' note that a host's tabs are missing or stale, kept to one
/// row: tabs and newlines in the message become spaces.
fn failed_row(host: &str, why: &str) -> String {
    let why: String = why
        .trim()
        .chars()
        .map(|c| if c == '\t' || c == '\n' { ' ' } else { c })
        .collect();
    format!("E\t{host}\t{why}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(name: &str, rows: &str, age: Option<u64>, error: Option<&str>) -> Value {
        json!({ "host": name, "rows": rows, "age_ms": age, "error": error, "watching": true })
    }

    #[test]
    fn pastes_carry_every_host_and_checked_tasks() {
        let snap = json!({
            "hosts": [host("local", "S\t-\tlocal\ts1\ta\n", Some(5), None), host("miller", "S\tx\tmiller\ts2\tb\n", Some(9), None)],
            "tasks": [{"window_id": "w"}], "tasks_checked": true, "tasks_error": null,
        });
        assert_eq!(
            pastes(&snap),
            vec![
                (
                    "rows",
                    "S\t-\tlocal\ts1\ta\nS\tx\tmiller\ts2\tb\n".to_string()
                ),
                ("tasks", r#"[{"window_id":"w"}]"#.to_string()),
            ]
        );
    }

    #[test]
    fn pastes_for_remote_viewer_rewrites_local_and_viewer_hosts() {
        let mac_hex = hex_encode("m4-mbp");
        let omarchy_hex = hex_encode("omarchy");
        let snap = json!({
            "server_host": "omarchy",
            "hosts": [
                host("local", "S\t-\tlocal\ts1\tmarvin\nT\tw1\tworking\tchat\t~\n", Some(5), None),
                host("miller", "S\t6d696c6c6572\tmiller\ts2\tcalc\n", Some(9), None),
                host("m4-mbp", &format!("S\t{mac_hex}\tm4-mbp\ts3\tagents\nT\tw3\t\tswitchyard\t~\n"), Some(4), None),
            ],
            "tasks": [], "tasks_checked": true, "tasks_error": null,
        });
        let got = pastes_for(&snap, "m4-mbp");
        assert_eq!(
            got[0],
            (
                "rows",
                format!(
                    "S\t-\tlocal\ts3\tagents\nT\tw3\t\tswitchyard\t~\n\
                     S\t{omarchy_hex}\tomarchy\ts1\tmarvin\nT\tw1\tworking\tchat\t~\n\
                     S\t6d696c6c6572\tmiller\ts2\tcalc\n"
                )
            )
        );
        // Viewing from the server itself keeps rows as-is.
        let same = pastes_for(&snap, "omarchy");
        assert!(same[0].1.starts_with("S\t-\tlocal\ts1\tmarvin\n"));
    }

    #[test]
    fn failed_hosts_keep_their_rows_and_get_a_note() {
        let snap = json!({
            "hosts": [
                host("local", "S\t-\tlocal\ts1\ta\n", Some(5), None),
                host("miller", "S\tx\tmiller\ts2\tb\n", Some(60000), Some("watcher exited (1)\nssh: gone")),
                host("drummer", "", None, None),
            ],
            "tasks": [], "tasks_checked": false, "tasks_error": null,
        });
        assert_eq!(
            pastes(&snap),
            vec![
                (
                    "rows",
                    "S\t-\tlocal\ts1\ta\nS\tx\tmiller\ts2\tb\n\
                E\tmiller\twatcher exited (1) ssh: gone\n\
                E\tdrummer\tnot listed yet\n"
                        .to_string()
                ),
                ("live", "[]".to_string()),
            ]
        );
    }

    #[test]
    fn nothing_listed_anywhere_is_an_error_or_nothing() {
        let mut snap = json!({
            "hosts": [host("local", "", None, Some("can't start rex")), host("miller", "", None, None)],
            "tasks": null, "tasks_checked": false, "tasks_error": "bad json",
        });
        assert_eq!(
            pastes(&snap),
            vec![
                ("error", "local: can't start rex".to_string()),
                ("tasks-error", "bad json".to_string()),
            ]
        );
        // A server that has only just started knows nothing yet.
        snap["hosts"][0]["error"] = Value::Null;
        snap["tasks_error"] = Value::Null;
        assert_eq!(pastes(&snap), [] as [(&str, String); 0]);
        assert_eq!(
            pastes(&json!({ "hosts": [], "tasks": null })),
            [] as [(&str, String); 0]
        );
    }

    #[test]
    fn unchecked_tasks_that_failed_their_check_stay_live() {
        let snap = json!({
            "hosts": [host("local", "", Some(1), None)],
            "tasks": [], "tasks_checked": false, "tasks_error": "ssh: timeout",
        });
        assert_eq!(
            pastes(&snap),
            vec![
                ("rows", String::new()),
                ("live", "[]".to_string()),
                ("tasks-error", "ssh: timeout".to_string()),
            ]
        );
    }

    #[test]
    fn parses_http_urls() {
        assert_eq!(
            parse_http_url("http://omarchy.taila14c2.ts.net:7391/").unwrap(),
            (
                "omarchy.taila14c2.ts.net".into(),
                7391,
                "omarchy.taila14c2.ts.net:7391".into(),
                String::new()
            )
        );
        assert_eq!(
            parse_http_url("http://[::1]:7391/sub/").unwrap(),
            ("::1".into(), 7391, "[::1]:7391".into(), "/sub".into())
        );
        assert_eq!(
            parse_http_url("http://omarchy").unwrap(),
            ("omarchy".into(), 80, "omarchy".into(), String::new())
        );
        assert!(parse_http_url("https://omarchy:7391").is_err());
        assert!(parse_http_url("http://").is_err());
    }
}

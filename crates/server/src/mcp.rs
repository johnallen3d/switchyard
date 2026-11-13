//! The MCP endpoint: Switchyard's task operations for agents on any tailnet
//! host. Only the config host (the Mac) can spawn, and nothing can reach it
//! but this: the endpoint listens on loopback and the Mac's Tailscale
//! address, and answers peers from loopback or the tailnet only. There is
//! no token; the tailnet is the boundary.
//!
//! Transport: MCP's Streamable HTTP, at `POST /mcp`, with every reply a
//! single JSON body (no SSE streams, so `GET /mcp` is 405) and no sessions.
//! A request with a browser `Origin` is refused, so a web page can't reach
//! it through DNS rebinding.
//!
//! Tools (see `TOOLS`) wrap core: `list_tasks` (the snapshot's open tasks
//! with live agent status), `spawn_task` (`launch_text`), `read_task`
//! (`transcript`), `message_task` (`actions::message`) and `stop_task` (a
//! plain archive, never force-archive). They name a task `PROJECT/NAME`, as
//! the pickers label it.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use switchyard_core::hosts::catalog;
use switchyard_core::launch::launch_text;
use switchyard_core::naming::name_char;
use switchyard_core::tasks::{in_window, live_tasks, load_tasks};
use switchyard_core::{actions, transcript};

use crate::state::Shared;

/// Protocol revisions this endpoint speaks, newest first: it uses only
/// what they share (tools, plain JSON replies).
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The most a request's headers or body may hold.
const MAX_HEADERS: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;
/// How long a kept-alive connection may sit idle.
const IDLE: Duration = Duration::from_secs(120);
/// How often to look for the Tailscale address until it's there.
const TAILSCALE_RETRY: Duration = Duration::from_secs(30);
/// Transcript entries `read_task` returns by default, and at most.
const READ_DEFAULT: usize = 20;
const READ_MAX: usize = 150;

const INSTRUCTIONS: &str = "Switchyard runs agent tasks: each is an agent (claude or pi) in a Rex tab, \
working in a Git worktree of a catalog project on one of John's hosts. Tasks are named PROJECT/NAME. \
list_tasks shows the open ones with live status (working, idle, blocked on a prompt, ...). \
spawn_task starts one with a first prompt; read_task shows what its agent has been saying; \
message_task types text into its tab; stop_task archives it (refused while it has uncommitted changes).";

#[derive(Clone)]
pub struct Endpoint {
    pub shared: Arc<Shared>,
    /// The task check (see `tasks::run`), prompted after a change.
    pub refresh: Sender<&'static str>,
}

/// Listen on loopback now and on the Tailscale address once there is one.
pub fn start(endpoint: Endpoint, port: u16) {
    match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => {
            crate::log(&format!("mcp: listening on http://127.0.0.1:{port}/mcp"));
            let endpoint = endpoint.clone();
            thread::spawn(move || accept(l, endpoint));
        }
        Err(e) => crate::log(&format!("mcp: 127.0.0.1:{port}: {e}")),
    }
    thread::spawn(move || {
        loop {
            if let Some(ip) = tailscale_ipv4() {
                match TcpListener::bind((ip, port)) {
                    Ok(l) => {
                        crate::log(&format!("mcp: listening on http://{ip}:{port}/mcp"));
                        return accept(l, endpoint);
                    }
                    Err(e) => crate::log(&format!("mcp: {ip}:{port}: {e}")),
                }
            }
            thread::sleep(TAILSCALE_RETRY);
        }
    });
}

/// This machine's Tailscale IPv4 address: the interface address in
/// 100.64.0.0/10 (Tailscale's CGNAT range).
fn tailscale_ipv4() -> Option<Ipv4Addr> {
    let mut found = None;
    // SAFETY: getifaddrs fills a list freed below with freeifaddrs; each
    // address is read as sockaddr_in only when its family says AF_INET.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut cur = list;
        while !cur.is_null() {
            let addr = (*cur).ifa_addr;
            if !addr.is_null() && (*addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(addr as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if in_tailnet(IpAddr::V4(ip)) {
                    found = Some(ip);
                    break;
                }
            }
            cur = (*cur).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    found
}

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
fn in_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (64..128).contains(&b)
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

fn allowed_peer(peer: SocketAddr) -> bool {
    let ip = match peer.ip() {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        ip => ip,
    };
    ip.is_loopback() || in_tailnet(ip)
}

fn accept(listener: TcpListener, endpoint: Endpoint) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let endpoint = endpoint.clone();
                thread::spawn(move || {
                    let _ = connection(stream, &endpoint);
                });
            }
            Err(e) => crate::log(&format!("mcp: {e}")),
        }
    }
}

/// One HTTP request, as far as this endpoint reads it.
#[derive(Debug, Default)]
struct Request {
    method: String,
    path: String,
    origin: Option<String>,
    close: bool,
    body: Vec<u8>,
}

/// Read the next request; None when the client has hung up between
/// requests. Err(status) for one this endpoint won't read.
fn read_request(reader: &mut impl BufRead) -> io::Result<Option<Result<Request, u16>>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (method, target, version) = (parts.next(), parts.next(), parts.next());
    let (Some(method), Some(target), Some(version)) = (method, target, version) else {
        return Ok(Some(Err(400)));
    };
    let mut req = Request {
        method: method.to_string(),
        path: target.split('?').next().unwrap_or("").to_string(),
        close: version == "HTTP/1.0",
        ..Request::default()
    };
    let (mut length, mut size) = (0usize, line.len());
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        size += header.len();
        if size > MAX_HEADERS {
            return Ok(Some(Err(431)));
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => match value.parse() {
                Ok(n) => length = n,
                Err(_) => return Ok(Some(Err(400))),
            },
            "transfer-encoding" => return Ok(Some(Err(411))),
            "origin" => req.origin = Some(value.to_string()),
            "connection" => match value.to_ascii_lowercase().as_str() {
                "close" => req.close = true,
                "keep-alive" => req.close = false,
                _ => {}
            },
            _ => {}
        }
    }
    if length > MAX_BODY {
        return Ok(Some(Err(413)));
    }
    req.body = vec![0; length];
    reader.read_exact(&mut req.body)?;
    Ok(Some(Ok(req)))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Error",
    }
}

fn respond(out: &mut impl Write, status: u16, body: Option<&Value>, close: bool) -> io::Result<()> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\n",
        reason(status),
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if status == 405 {
        head.push_str("Allow: POST\r\n");
    }
    head.push_str(if close {
        "Connection: close\r\n\r\n"
    } else {
        "Connection: keep-alive\r\n\r\n"
    });
    out.write_all(head.as_bytes())?;
    out.write_all(body.as_bytes())?;
    out.flush()
}

fn connection(stream: TcpStream, endpoint: &Endpoint) -> io::Result<()> {
    let peer = stream.peer_addr()?;
    if !allowed_peer(peer) {
        crate::log(&format!("mcp: refused {peer}: not on the tailnet"));
        return Ok(());
    }
    stream.set_read_timeout(Some(IDLE))?;
    let mut out = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    while let Some(req) = read_request(&mut reader)? {
        let req = match req {
            Ok(req) => req,
            Err(status) => return respond(&mut out, status, None, true),
        };
        let (status, body) = route(&req, endpoint);
        respond(&mut out, status, body.as_ref(), req.close)?;
        if req.close {
            break;
        }
    }
    Ok(())
}

fn route(req: &Request, endpoint: &Endpoint) -> (u16, Option<Value>) {
    if req.origin.is_some() {
        return (
            403,
            Some(rpc_error(
                Value::Null,
                -32600,
                "browser requests are refused",
            )),
        );
    }
    if req.path != "/mcp" {
        return (404, None);
    }
    if req.method != "POST" {
        return (405, None);
    }
    let Ok(message) = serde_json::from_slice::<Value>(&req.body) else {
        return (400, Some(rpc_error(Value::Null, -32700, "parse error")));
    };
    // Batches came and went between revisions; answering them costs little.
    let replies: Vec<Value> = match &message {
        Value::Array(batch) => batch.iter().filter_map(|m| handle(m, endpoint)).collect(),
        m => handle(m, endpoint).into_iter().collect(),
    };
    match (message.is_array(), replies.len()) {
        (_, 0) => (202, None),
        (false, _) => (200, replies.into_iter().next()),
        (true, _) => (200, Some(Value::Array(replies))),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The reply to one JSON-RPC message; None for notifications and responses.
fn handle(message: &Value, endpoint: &Endpoint) -> Option<Value> {
    let Some(method) = message["method"].as_str() else {
        // A response to something this server never asks, or junk.
        return message
            .get("id")
            .is_none()
            .then(|| rpc_error(Value::Null, -32600, "invalid request"));
    };
    let id = message.get("id")?.clone();
    let params = &message["params"];
    let result = match method {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or("");
            let version = VERSIONS
                .iter()
                .find(|v| **v == asked)
                .unwrap_or(&VERSIONS[0]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "switchyard", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let args = &params["arguments"];
            let started = Instant::now();
            let outcome = call(name, args, endpoint);
            let took = started.elapsed().as_millis();
            match &outcome {
                Ok(_) => crate::log(&format!("mcp: {name} ok ({took} ms)")),
                Err(e) => crate::log(&format!("mcp: {name} failed ({took} ms): {e}")),
            }
            match outcome {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": value.to_string() }],
                    "structuredContent": value,
                    "isError": false,
                }),
                Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
            }
        }
        _ => return Some(rpc_error(id, -32601, &format!("unknown method {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn tools() -> Value {
    let task =
        json!({ "type": "string", "description": "The task, as PROJECT/NAME (see list_tasks)." });
    json!([
        {
            "name": "list_tasks",
            "description": "List the open agent tasks with each agent's live status.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "spawn_task",
            "description": "Start an agent task: a new worktree branch of a catalog project, \
    with an agent working on PROMPT in its own Rex tab. Returns the task's PROJECT/NAME.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": { "type": "string", "description": "Catalog project: its name (the folder name) or path." },
                    "name": { "type": "string", "description": "Task name, normalised as the picker does (lowercase, spaces become dashes); the branch is feature/NAME." },
                    "harness": { "type": "string", "enum": ["claude", "pi"], "default": "claude" },
                    "prompt": { "type": "string", "description": "The agent's first message." },
                    "host": { "type": "string", "description": "Rex host label, or auto (the default) for the project's least busy host." },
                    "work_item": { "type": "string", "description": "Optional issue or card the work is for (a ref or URL)." },
                },
                "required": ["project", "name", "prompt"],
            },
        },
        {
            "name": "read_task",
            "description": "What a task's agent has been saying: its live status and the last entries of its \
    transcript (user and assistant messages, tool calls, questions). An agent just spawned may have \
    no transcript yet (transcript_error says why).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "task": task,
                    "entries": { "type": "integer", "minimum": 1, "maximum": READ_MAX, "default": READ_DEFAULT },
                },
                "required": ["task"],
            },
            "annotations": { "readOnlyHint": true },
        },
        {
            "name": "message_task",
            "description": "Send text to a task's agent as if typed in its tab: an idle agent starts a turn, \
    a working one takes it in at its next chance. Refused while the agent waits on a prompt.",
            "inputSchema": {
                "type": "object",
                "properties": { "task": task, "text": { "type": "string" } },
                "required": ["task", "text"],
            },
        },
        {
            "name": "stop_task",
            "description": "Archive a task: close its tab and remove its worktree. Refused while the worktree \
    has uncommitted changes.",
            "inputSchema": { "type": "object", "properties": { "task": task }, "required": ["task"] },
            "annotations": { "destructiveHint": true },
        },
    ])
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} is required"))
}

fn opt_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn label(task: &Value) -> String {
    format!(
        "{}/{}",
        task["project"].as_str().unwrap_or(""),
        task["name"].as_str().unwrap_or("")
    )
}

fn call(name: &str, args: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    match name {
        "list_tasks" => {
            Ok(json!({ "tasks": open_tasks(endpoint)?.iter().map(summary).collect::<Vec<_>>() }))
        }
        "spawn_task" => spawn(args, endpoint),
        "read_task" => {
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let wanted = args["entries"]
                .as_u64()
                .map_or(READ_DEFAULT, |n| (n as usize).clamp(1, READ_MAX));
            let (host, session, window) = tab(&task);
            let mut log = transcript::fetch_on(host, session, window);
            let mut out = summary(&task);
            // The task is there even when its log isn't (yet: an agent just
            // spawned writes none until it starts), so a poller still gets
            // its status.
            match log["entries"].as_array_mut() {
                Some(entries) => {
                    let skip = entries.len().saturating_sub(wanted);
                    out["transcript"] = entries.drain(skip..).collect::<Vec<_>>().into();
                }
                None => {
                    out["transcript"] = json!([]);
                    out["transcript_error"] = log["error"].take();
                }
            }
            Ok(out)
        }
        "message_task" => {
            let text = str_arg(args, "text")?;
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let (host, session, window) = tab(&task);
            actions::message(host, session, window, text).map(|msg| json!({ "message": msg }))
        }
        "stop_task" => {
            let task = find_open(str_arg(args, "task")?, endpoint)?;
            let (host, session, window) = tab(&task);
            let result = actions::act("archive", host, session, window);
            let _ = endpoint.refresh.send("mcp");
            result.map(|msg| json!({ "message": msg }))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

fn tab(task: &Value) -> (&str, &str, &str) {
    let f = |k: &str| task[k].as_str().unwrap_or("");
    (f("host"), f("session_id"), f("window_id"))
}

/// The open tasks with live status: the snapshot's, or a direct look
/// before the server's first check.
fn open_tasks(endpoint: &Endpoint) -> Result<Vec<Value>, String> {
    let snap = endpoint.shared.lock().snapshot(Instant::now());
    let tasks = match snap["tasks"].as_array() {
        Some(tasks) => tasks.clone(),
        None => live_tasks()?,
    };
    Ok(tasks
        .into_iter()
        .filter(|t| t["ended_at"].is_null())
        .collect())
}

/// What a caller needs of a task record.
fn summary(task: &Value) -> Value {
    let mut out = json!({ "task": label(task) });
    for key in [
        "project",
        "name",
        "host",
        "harness",
        "branch",
        "worktree",
        "status",
        "status_msg",
        "status_at",
        "created_at",
        "links",
    ] {
        if let Some(v) = task.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }
    out
}

/// The open task `wanted` names (see `find_task`): from the snapshot, for
/// its live status, else from the registry, which has a task just spawned
/// before the server's next check does.
fn find_open(wanted: &str, endpoint: &Endpoint) -> Result<Value, String> {
    find_task(wanted, &open_tasks(endpoint)?).or_else(|e| {
        let registry: Vec<Value> = load_tasks()?
            .into_iter()
            .filter(|t| t["ended_at"].is_null())
            .collect();
        find_task(wanted, &registry).map_err(|_| e)
    })
}

/// The open task named `PROJECT/NAME`, or a bare NAME only one has.
fn find_task(wanted: &str, open: &[Value]) -> Result<Value, String> {
    let matches: Vec<&Value> = match wanted.split_once('/') {
        Some(_) => open.iter().filter(|t| label(t) == wanted).collect(),
        None => open
            .iter()
            .filter(|t| t["name"].as_str() == Some(wanted))
            .collect(),
    };
    match matches.as_slice() {
        [task] => Ok((*task).clone()),
        [] => Err(format!("no open task {wanted} (see list_tasks)")),
        _ => Err(format!(
            "{wanted} names {} open tasks; give PROJECT/NAME",
            matches.len()
        )),
    }
}

fn normalise(name: &str) -> String {
    name.chars().filter_map(name_char).collect()
}

fn spawn(args: &Value, endpoint: &Endpoint) -> Result<Value, String> {
    let project = str_arg(args, "project")?;
    // As the picker takes typed names: `MCP smoke test` is `mcp-smoke-test`.
    let name = normalise(str_arg(args, "name")?);
    let name = name.as_str();
    let prompt = str_arg(args, "prompt")?;
    let harness = opt_arg(args, "harness").unwrap_or("claude");
    let host = opt_arg(args, "host").unwrap_or("auto");
    // Catalog projects only: a folder outside it would need its host's
    // checkout, which a remote caller can't see.
    let projects = catalog()?;
    let found = projects
        .iter()
        .find(|p| p.name == project || p.path == project)
        .ok_or_else(|| format!("{project} is not a catalog project"))?;
    let (host, session, window) = launch_text(
        host,
        &found.path,
        name,
        harness,
        prompt,
        opt_arg(args, "work_item"),
    )?;
    let _ = endpoint.refresh.send("mcp");
    // The registry has it now: name it as list_tasks will.
    let label = load_tasks()
        .ok()
        .and_then(|tasks| {
            tasks
                .iter()
                .rev()
                .find(|t| t["ended_at"].is_null() && in_window(t, &window))
                .map(label)
        })
        .unwrap_or_else(|| format!("{}/{name}", found.name));
    Ok(json!({ "task": label, "host": host, "session_id": session, "window_id": window }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::mpsc;

    fn endpoint() -> Endpoint {
        let (refresh, _) = mpsc::channel();
        Endpoint {
            shared: Arc::new(Shared::new()),
            refresh,
        }
    }

    fn post(body: &str) -> Request {
        Request {
            method: "POST".into(),
            path: "/mcp".into(),
            body: body.as_bytes().to_vec(),
            ..Request::default()
        }
    }

    #[test]
    fn tailnet_and_loopback_peers_only() {
        let ok = |s: &str| allowed_peer(s.parse().unwrap());
        assert!(ok("127.0.0.1:1"));
        assert!(ok("100.95.69.110:1"));
        assert!(ok("[fd7a:115c:a1e0::1]:1"));
        assert!(ok("[::ffff:100.64.0.1]:1"));
        assert!(!ok("100.128.0.1:1"));
        assert!(!ok("192.168.1.10:1"));
        assert!(!ok("[2001:db8::1]:1"));
    }

    #[test]
    fn reads_a_request_and_its_body() {
        let raw = "POST /mcp?x=1 HTTP/1.1\r\nHost: mac\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
        let req = read_request(&mut Cursor::new(raw))
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            (req.method.as_str(), req.path.as_str(), req.close),
            ("POST", "/mcp", true)
        );
        assert_eq!(req.body, b"{}");
        let chunked = "POST /mcp HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n";
        assert_eq!(
            read_request(&mut Cursor::new(chunked))
                .unwrap()
                .unwrap()
                .unwrap_err(),
            411
        );
        assert!(read_request(&mut Cursor::new("")).unwrap().is_none());
    }

    #[test]
    fn initializes_with_a_version_both_speak() {
        let e = endpoint();
        let init = |v: &str| {
            let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": v, "capabilities": {}, "clientInfo": { "name": "t", "version": "0" } } });
            route(&post(&body.to_string()), &e).1.unwrap()["result"]["protocolVersion"].clone()
        };
        assert_eq!(init("2025-06-18"), "2025-06-18");
        assert_eq!(init("1999-01-01"), VERSIONS[0]);
    }

    #[test]
    fn notifications_get_202_and_unknown_methods_an_error() {
        let e = endpoint();
        let (status, body) = route(
            &post(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            &e,
        );
        assert_eq!((status, body), (202, None));
        let (status, body) = route(&post(r#"{"jsonrpc":"2.0","id":"a","method":"nope"}"#), &e);
        assert_eq!(status, 200);
        assert_eq!(body.unwrap()["error"]["code"], -32601);
        assert_eq!(route(&post("not json"), &e).0, 400);
    }

    #[test]
    fn refuses_browsers_other_paths_and_methods() {
        let e = endpoint();
        let mut req = post("{}");
        req.origin = Some("http://evil.example".into());
        assert_eq!(route(&req, &e).0, 403);
        let get = Request {
            method: "GET".into(),
            path: "/mcp".into(),
            ..Request::default()
        };
        assert_eq!(route(&get, &e).0, 405);
        let other = Request {
            method: "POST".into(),
            path: "/".into(),
            ..Request::default()
        };
        assert_eq!(route(&other, &e).0, 404);
    }

    #[test]
    fn lists_five_tools() {
        let names: Vec<String> = tools()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().into())
            .collect();
        assert_eq!(
            names,
            [
                "list_tasks",
                "spawn_task",
                "read_task",
                "message_task",
                "stop_task"
            ]
        );
    }

    #[test]
    fn finds_tasks_by_label_or_unique_name() {
        let t = |p: &str, n: &str| json!({ "project": p, "name": n, "ended_at": null });
        let open = vec![t("switchyard", "api"), t("rex", "api"), t("rex", "fix-it")];
        assert_eq!(label(&find_task("rex/api", &open).unwrap()), "rex/api");
        assert_eq!(label(&find_task("fix-it", &open).unwrap()), "rex/fix-it");
        assert!(
            find_task("api", &open)
                .unwrap_err()
                .contains("2 open tasks")
        );
        assert!(find_task("rex/nope", &open).is_err());
    }

    #[test]
    fn normalises_names_as_the_picker_does() {
        assert_eq!(normalise("MCP smoke_test!"), "mcp-smoke-test");
    }

    #[test]
    fn tool_errors_are_results_not_protocol_errors() {
        let e = endpoint();
        let body = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "message_task", "arguments": { "task": "x/y" } } });
        let reply = route(&post(&body.to_string()), &e).1.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(reply["result"]["content"][0]["text"], "text is required");
    }
}
